//! Background worker thread for mihomo lifecycle, polling, and controller commands.

mod config_tracker;
mod state_sync;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;

use self::config_tracker::ConfigTracker;
use self::state_sync::StateSync;
use crate::config::mihomo;
use crate::config::state_store::PersistedState;
use crate::config::tray::TraySettings;
use crate::core::api::{Client, CoreConfigs};
use crate::core::discover;
use crate::core::process::{self, ManagedChild, ProcessInspector, RunningProcess, RuntimeRecord};
use crate::platform;
use crate::state::{
    self, AppEvent, AsyncTaskKind, Provider, ProxyMode, SharedSnapshot, WorkerCommand,
};
use crate::text;

const KERNEL_STARTUP_BUDGET: Duration = Duration::from_secs(6);
const KERNEL_RESTART_BUDGET: Duration = Duration::from_secs(10);

pub struct WorkerHandle {
    pub tx: Sender<WorkerCommand>,
    stop_requested: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    pub fn send(&self, cmd: WorkerCommand) {
        if matches!(cmd, WorkerCommand::Quit { .. }) {
            self.stop_requested.store(true, Ordering::Release);
        }
        let _ = self.tx.send(cmd);
    }

    pub fn join(&mut self) {
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Clone)]
enum UiNotifier {
    Loop(EventLoopProxy<AppEvent>),
    #[cfg(test)]
    Channel(Sender<AppEvent>),
}

impl UiNotifier {
    fn send(&self, event: AppEvent) {
        match self {
            Self::Loop(proxy) => {
                let _ = proxy.send_event(event);
            }
            #[cfg(test)]
            Self::Channel(tx) => {
                let _ = tx.send(event);
            }
        }
    }
}

pub fn spawn(
    portable_root: PathBuf,
    settings_path: PathBuf,
    initial_settings: TraySettings,
    shared: SharedSnapshot,
    proxy: EventLoopProxy<AppEvent>,
    startup_error: Option<String>,
) -> WorkerHandle {
    let stop_requested = Arc::new(AtomicBool::new(false));
    // Initialize Worker (and initial Snapshot fields) synchronously before returning
    // so the GUI thread's first menu build observes resolved paths and startup errors.
    let (mut worker, rx) = Worker::new(
        portable_root,
        settings_path,
        initial_settings,
        shared,
        UiNotifier::Loop(proxy),
        startup_error,
        Arc::clone(&stop_requested),
    );
    let tx = worker.tx.clone();

    let join = std::thread::Builder::new()
        .name("mihomo-worker".to_string())
        .spawn(move || {
            worker.run(rx);
        })
        .expect("failed to spawn worker thread");

    WorkerHandle {
        tx,
        stop_requested,
        join: Some(join),
    }
}

struct Worker {
    cfg: ConfigTracker,
    state_sync: StateSync,
    shared: SharedSnapshot,
    proxy: UiNotifier,
    stop_requested: Arc<AtomicBool>,
    tx: Sender<WorkerCommand>,
    testing_latency: bool,
    sub_task_active: bool,
    pending_sub_reload: bool,
    process_inspector: ProcessInspector,
    managed: Option<ManagedChild>,
    recorded_pid: Option<u32>,
    client: Option<Client>,
    cached_version: String,
    last_controller_ok: bool,
}

impl Worker {
    fn new(
        portable_root: PathBuf,
        settings_path: PathBuf,
        settings: TraySettings,
        shared: SharedSnapshot,
        proxy: UiNotifier,
        startup_error: Option<String>,
        stop_requested: Arc<AtomicBool>,
    ) -> (Self, mpsc::Receiver<WorkerCommand>) {
        let (tx, rx) = mpsc::channel::<WorkerCommand>();
        let cfg = ConfigTracker::new(portable_root, settings_path, settings, startup_error);
        {
            let mut snap = shared.lock().unwrap();
            snap.page_size = cfg.settings.groups.page_size;
            snap.resolved_config = Some(cfg.config_file.clone());
            snap.config_error = cfg.current_config_error();
        }
        let worker = Self {
            cfg,
            state_sync: StateSync::default(),
            shared,
            proxy,
            stop_requested,
            tx,
            testing_latency: false,
            sub_task_active: false,
            pending_sub_reload: false,
            process_inspector: ProcessInspector::new(),
            managed: None,
            recorded_pid: None,
            client: None,
            cached_version: String::new(),
            last_controller_ok: false,
        };
        (worker, rx)
    }

    fn run(&mut self, rx: mpsc::Receiver<WorkerCommand>) {
        self.startup();

        loop {
            let interval = Duration::from_millis(self.cfg.settings.ui.poll_interval_ms as u64);
            match rx.recv_timeout(interval) {
                Ok(WorkerCommand::Quit { interactive }) => {
                    if self.handle_quit(interactive) {
                        break;
                    }
                    self.refresh();
                }
                Ok(cmd) => {
                    let mut clear_error = false;
                    let mut burst_error = None;
                    let mut handle_or_quit =
                        |worker: &mut Self, c: WorkerCommand| -> Option<bool> {
                            if let WorkerCommand::Quit { interactive } = c {
                                return Some(worker.handle_quit(interactive));
                            }
                            match worker.handle_command(c) {
                                Ok(clears) => clear_error |= clears,
                                Err(err) => burst_error = Some(err),
                            }
                            None
                        };

                    if let Some(should_exit) = handle_or_quit(self, cmd) {
                        if should_exit {
                            break;
                        }
                        self.refresh();
                        continue;
                    }
                    // Coalesce burst commands before doing a single refresh poll.
                    while let Ok(extra) = rx.try_recv() {
                        if let Some(should_exit) = handle_or_quit(self, extra) {
                            if should_exit {
                                return;
                            }
                            break;
                        }
                    }
                    if let Some(err) = burst_error {
                        self.set_action_error(Some(err));
                    } else if clear_error {
                        self.set_action_error(None);
                    }
                    self.refresh();
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.refresh();
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.handle_quit(false);
                    break;
                }
            }
        }
    }

    fn startup(&mut self) {
        self.reload_paths_and_runtime();
        self.state_sync.load_initial(&self.cfg.home_dir);
        self.cfg.refresh_autostart_cache();
        let running = self
            .process_inspector
            .list_running_mihomo(self.cfg.known_kernel_path.as_deref());
        self.re_resolve_controller(&running);

        let controller_alive = self.client.as_ref().is_some_and(|c| c.probe().is_ok());

        if controller_alive {
            // Core is already running; preserve its live mode/TUN instead of overriding from disk.
            self.state_sync.clear_pending_restore();
        } else if running.is_empty()
            && self.cfg.settings.mihomo.auto_start
            && let Err(err) = self.start_core_internal()
        {
            tracing::warn!("Auto-start core failed: {err}");
            let has_error = self.shared.lock().unwrap().action_error.is_some();
            if !has_error {
                self.set_action_error(Some(err));
            }
        }
        if !self.stop_requested.load(Ordering::Acquire) {
            self.refresh();
        }
    }

    fn reload_paths_and_runtime(&mut self) {
        self.cfg.reload_settings_if_changed();
        self.cfg.refresh_paths();
        self.sync_config_error();

        if let Some(rec) = process::load_runtime_record(&self.cfg.home_dir) {
            if self
                .process_inspector
                .is_recorded_kernel_alive(rec.pid, Some(&rec.kernel_path))
            {
                self.recorded_pid = Some(rec.pid);
                if self.cfg.known_kernel_path.is_none() {
                    self.cfg.known_kernel_path = Some(rec.kernel_path);
                }
            } else {
                self.recorded_pid = None;
                process::clear_runtime_record(&self.cfg.home_dir);
            }
        }
    }

    fn re_resolve_controller(&mut self, running: &[RunningProcess]) {
        match discover::resolve_controller(
            &self.cfg.portable_root,
            &self.cfg.settings,
            running,
            Some(&self.cfg.config_file),
        ) {
            Ok(client) => {
                self.client = Some(client);
                self.set_controller_error(None);
            }
            Err(err) => {
                self.client = None;
                self.set_controller_error(Some(err));
            }
        }
    }

    fn handle_command(&mut self, cmd: WorkerCommand) -> Result<bool, String> {
        match cmd {
            WorkerCommand::SetMode(mode) => self.set_mode(mode).map(|()| true),
            WorkerCommand::ToggleSystemProxy => self.toggle_system_proxy().map(|()| true),
            WorkerCommand::ToggleTun => self.toggle_tun().map(|()| true),
            WorkerCommand::SelectProxy { group, member } => self
                .with_client(|c| c.select_proxy(&group, &member))
                .map(|()| true),
            WorkerCommand::UnfixGroup(group) => {
                self.with_client(|c| c.unfix_group(&group)).map(|()| true)
            }
            WorkerCommand::TestGroupLatency(group) => self.start_test_group_latency(group),
            WorkerCommand::RefreshSubscriptions => self.start_refresh_subscriptions(),
            WorkerCommand::SetSubscriptionUrl(url) => {
                self.set_subscription_url(&url).map(|()| true)
            }
            WorkerCommand::CopyEnvVars => self.copy_env_vars().map(|()| true),
            WorkerCommand::RestartCore => self.force_restart_core().map(|()| true),
            WorkerCommand::ToggleAutostart => {
                let enabled = platform::autostart_is_enabled();
                let res = platform::autostart_set(!enabled);
                self.cfg.refresh_autostart_cache();
                res.map(|()| true)
            }
            WorkerCommand::AsyncDone { kind, error } => self.handle_async_done(kind, error),
            WorkerCommand::Quit { .. } => Ok(false),
        }
    }

    fn handle_async_done(
        &mut self,
        kind: AsyncTaskKind,
        error: Option<String>,
    ) -> Result<bool, String> {
        match kind {
            AsyncTaskKind::LatencyTest => {
                self.testing_latency = false;
                match error {
                    Some(err) => Err(err),
                    None => Ok(false),
                }
            }
            AsyncTaskKind::RefreshSubscriptions => {
                self.sub_task_active = false;
                if self.pending_sub_reload {
                    self.pending_sub_reload = false;
                    if let Some(err) = error {
                        tracing::warn!("Superseded subscription refresh failed: {err}");
                    }
                    if let Some(client) = self.client.clone()
                        && client.probe().is_ok()
                    {
                        self.spawn_apply_subscription_reload(client);
                        return Ok(false);
                    }
                    self.set_status_note(None);
                    self.state_sync.promote_reload_restore(true);
                    return Ok(false);
                }
                self.set_status_note(None);
                match error {
                    Some(err) => Err(err),
                    None => Ok(false),
                }
            }
            AsyncTaskKind::ApplySubscription { reload_ok } => {
                self.sub_task_active = false;
                if self.pending_sub_reload {
                    self.pending_sub_reload = false;
                    if let Some(client) = self.client.clone() {
                        if let Some(err) = error {
                            tracing::warn!("Superseded subscription apply failed: {err}");
                        }
                        self.spawn_apply_subscription_reload(client);
                        return Ok(false);
                    }
                }
                self.set_status_note(None);
                self.state_sync.promote_reload_restore(reload_ok);
                match error {
                    Some(err) => Err(err),
                    None => Ok(false),
                }
            }
        }
    }

    fn set_mode(&mut self, mode: ProxyMode) -> Result<(), String> {
        let mode_str = mode.as_api_str();
        if mode_str.is_empty() {
            return Err("unsupported proxy mode".to_string());
        }
        self.with_client(|c| c.set_mode(mode_str))?;
        self.state_sync
            .update(&self.cfg.home_dir, |s| s.mode = Some(mode_str.to_string()));
        Ok(())
    }

    fn with_client<F>(&self, f: F) -> Result<(), String>
    where
        F: FnOnce(&Client) -> Result<(), String>,
    {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())?;
        f(client)
    }

    fn toggle_system_proxy(&mut self) -> Result<(), String> {
        if platform::proxy_is_enabled() {
            platform::proxy_disable()?;
            self.state_sync
                .update(&self.cfg.home_dir, |s| s.system_proxy = false);
            return Ok(());
        }
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())?;
        let cfg = client.get_configs()?;
        if cfg.mixed_port == 0 {
            return Err(text::ERR_NO_MIXED_PORT_FOR_PROXY.to_string());
        }
        platform::proxy_enable(cfg.mixed_port, &self.cfg.settings.proxy.bypass)?;
        self.state_sync
            .update(&self.cfg.home_dir, |s| s.system_proxy = true);
        Ok(())
    }

    fn toggle_tun(&mut self) -> Result<(), String> {
        let client = self
            .client
            .clone()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())?;
        let current = client.get_configs()?;
        let want_enable = !current.tun_enable;
        self.apply_tun_state(want_enable)?;
        self.state_sync
            .update(&self.cfg.home_dir, |s| s.tun = Some(want_enable));
        Ok(())
    }

    fn apply_tun_state(&mut self, want_enable: bool) -> Result<(), String> {
        let client = self
            .client
            .clone()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())?;
        let set_res = client.set_tun(want_enable);
        let after = match client.get_configs() {
            Ok(cfg) => cfg,
            Err(cfg_err) => return Err(set_res.err().unwrap_or(cfg_err)),
        };
        if !want_enable {
            set_res?;
            if after.tun_enable {
                return Err("TUN mode remained enabled after disabling.".to_string());
            }
            return Ok(());
        }
        if after.tun_enable {
            set_res?;
            return Ok(());
        }
        if let Err(err) = &set_res {
            tracing::info!("Non-elevated set_tun(true) failed ({err}); attempting UAC elevation");
        }

        // TUN failed to activate -> elevate running kernel via hidden UAC self-helper.
        let running = self
            .process_inspector
            .list_running_mihomo(self.cfg.known_kernel_path.as_deref());
        let target = process::pick_target_process(
            &running,
            self.active_pid(),
            self.cfg.known_kernel_path.as_deref(),
        )
        .ok_or_else(|| text::ERR_LOCATE_ELEVATE_PROCESS.to_string())?;

        let kernel_path = target
            .exe
            .clone()
            .or_else(|| self.cfg.known_kernel_path.clone());
        let launch_args = discover::build_launch_args(
            &self.cfg.portable_root,
            &self.cfg.settings,
            kernel_path.as_deref(),
        )?;

        self.set_status_note(Some(text::STATUS_ELEVATING_CORE));
        let mut helper_args = vec![
            platform::ELEVATE_START_FLAG.to_string(),
            target.pid.to_string(),
        ];
        helper_args.extend(launch_args.iter().cloned());

        let code = platform::run_elevated_self(&helper_args);
        self.set_status_note(None);

        let new_pid = match code {
            Ok(c) if c > 0 => c as u32,
            Ok(c) => return Err(text::format_elevated_helper_failed(c)),
            Err(_) => return Err(text::ERR_TUN_ELEVATION_DECLINED.to_string()),
        };

        self.managed = None;
        self.recorded_pid = Some(new_pid);
        self.cached_version.clear();
        if let Some(ref kernel) = kernel_path {
            process::save_runtime_record(
                &self.cfg.home_dir,
                &RuntimeRecord {
                    pid: new_pid,
                    kernel_path: kernel.clone(),
                },
            );
        }

        let elevated = [RunningProcess {
            pid: new_pid,
            exe: kernel_path,
            cmd: launch_args,
        }];
        self.re_resolve_controller(&elevated);
        self.wait_for_controller(KERNEL_RESTART_BUDGET, text::ERR_CORE_RESTART_TIMEOUT)?;
        let elevated_client = self
            .client
            .as_ref()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())?;
        verify_or_enable_tun_after_elevation(elevated_client)
    }

    fn start_core_internal(&mut self) -> Result<(), String> {
        self.reload_paths_and_runtime();
        let running = self
            .process_inspector
            .list_running_mihomo(self.cfg.known_kernel_path.as_deref());
        if process::pick_target_process(
            &running,
            self.active_pid(),
            self.cfg.known_kernel_path.as_deref(),
        )
        .is_some()
        {
            self.re_resolve_controller(&running);
            return Ok(());
        }

        let kernel_path = discover::find_kernel(&self.cfg.portable_root, &self.cfg.settings)?
            .ok_or_else(|| text::ERR_CORE_NOT_FOUND.to_string())?;
        self.cfg.known_kernel_path = Some(kernel_path.clone());

        let (home_dir, config_file) = discover::resolve_home_and_config(
            &self.cfg.portable_root,
            &self.cfg.settings,
            Some(&kernel_path),
        );
        self.cfg.home_dir = home_dir.clone();
        self.cfg.config_file = config_file.clone();
        let _ = std::fs::create_dir_all(&home_dir);
        mihomo::ensure_default_config(&config_file);

        let args = discover::build_launch_args(
            &self.cfg.portable_root,
            &self.cfg.settings,
            Some(&kernel_path),
        )?;

        self.set_status_note(Some(text::STATUS_STARTING_CORE));
        let child = process::spawn_normal(&kernel_path, &args)?;
        let pid = child.pid;
        self.managed = Some(child);
        self.recorded_pid = Some(pid);
        self.cached_version.clear();
        process::save_runtime_record(
            &self.cfg.home_dir,
            &RuntimeRecord {
                pid,
                kernel_path: kernel_path.clone(),
            },
        );

        let spawned = [RunningProcess {
            pid,
            exe: Some(kernel_path),
            cmd: args,
        }];
        self.re_resolve_controller(&spawned);
        let res = self.wait_for_controller(KERNEL_STARTUP_BUDGET, text::ERR_CORE_NOT_RESPONDING);
        self.set_status_note(None);
        res
    }

    fn disable_managed_system_proxy(&self) {
        if self.state_sync.has_persisted_system_proxy()
            && platform::proxy_is_enabled()
            && let Err(err) = platform::proxy_disable()
        {
            tracing::warn!("Failed to disable system proxy on shutdown: {err}");
        }
    }

    fn stop_core_internal(&mut self, interactive: bool) -> Result<(), String> {
        self.set_status_note(Some(text::STATUS_STOPPING_CORE));
        let res = self.stop_active_kernel(interactive, None);
        self.set_status_note(None);
        match res {
            Ok(()) => {
                self.disable_managed_system_proxy();
                Ok(())
            }
            Err(err) if !interactive => {
                tracing::warn!("Failed to stop core during non-interactive shutdown: {err}");
                self.disable_managed_system_proxy();
                Ok(())
            }
            Err(err) => {
                let msg = text::format_confirm_quit_anyway(&err);
                if platform::confirm_yes_no(text::TITLE_QUIT_ANYWAY, &msg) {
                    self.disable_managed_system_proxy();
                    Ok(())
                } else {
                    Err(err)
                }
            }
        }
    }

    fn force_restart_core(&mut self) -> Result<(), String> {
        let saved = self.capture_live_or_persisted_state();
        self.state_sync.set_pending_restore(saved);
        self.reload_paths_and_runtime();
        let running = self
            .process_inspector
            .list_running_mihomo(self.cfg.known_kernel_path.as_deref());
        if self.managed.is_some() || !running.is_empty() {
            self.set_status_note(Some(text::STATUS_RESTARTING_CORE));
            let stop_res = self.stop_active_kernel(true, Some(running));
            self.set_status_note(None);
            stop_res?;
        }
        self.start_core_internal()
    }

    fn stop_active_kernel(
        &mut self,
        interactive: bool,
        prefetched: Option<Vec<RunningProcess>>,
    ) -> Result<(), String> {
        // First try managed non-elevated child directly.
        if let Some(mut managed) = self.managed.take() {
            if matches!(managed.child.try_wait(), Ok(None)) {
                if managed.child.kill().is_ok() {
                    let _ = managed.child.wait();
                    self.clear_running_tracking();
                    return Ok(());
                }
            } else {
                self.clear_running_tracking();
                return Ok(());
            }
        }

        let running = prefetched.unwrap_or_else(|| {
            self.process_inspector
                .list_running_mihomo(self.cfg.known_kernel_path.as_deref())
        });
        let target = process::pick_target_process(
            &running,
            self.active_pid(),
            self.cfg.known_kernel_path.as_deref(),
        )
        .cloned()
        .or_else(|| {
            if interactive && running.len() == 1 {
                let candidate = &running[0];
                let msg = text::format_confirm_stop_core(candidate.pid, candidate.exe.as_deref());
                if platform::confirm_yes_no(text::TITLE_CONFIRM_STOP, &msg) {
                    return Some(candidate.clone());
                }
            }
            None
        });

        let Some(target) = target else {
            self.recorded_pid = None;
            process::clear_runtime_record(&self.cfg.home_dir);
            return Ok(());
        };

        if process::terminate_pid_and_wait(target.pid).is_err() {
            if !interactive {
                return Err(text::ERR_STOP_ELEVATION_REQUIRED.to_string());
            }
            let code = platform::run_elevated_self(&[
                platform::ELEVATE_STOP_FLAG.to_string(),
                target.pid.to_string(),
            ])?;
            if code != 0 {
                return Err(text::ERR_STOP_ELEVATION_REQUIRED.to_string());
            }
        }

        self.clear_running_tracking();
        Ok(())
    }

    fn clear_running_tracking(&mut self) {
        self.recorded_pid = None;
        self.cached_version.clear();
        process::clear_runtime_record(&self.cfg.home_dir);
    }

    fn wait_for_controller(&mut self, budget: Duration, timeout_msg: &str) -> Result<(), String> {
        let start = Instant::now();
        while start.elapsed() < budget {
            if self.stop_requested.load(Ordering::Acquire) {
                return Err(text::ERR_WAIT_INTERRUPTED_FOR_SHUTDOWN.to_string());
            }
            if let Some(ref mut managed) = self.managed
                && let Ok(Some(_)) = managed.child.try_wait()
            {
                self.managed = None;
                self.recorded_pid = None;
                process::clear_runtime_record(&self.cfg.home_dir);
                return Err(text::ERR_CORE_STOPPED_UNEXPECTEDLY.to_string());
            }
            if let Some(client) = &self.client
                && client.probe().is_ok()
            {
                return Ok(());
            }
            if start.elapsed() >= Duration::from_secs(2) {
                self.set_status_note(Some(text::STATUS_WAITING_CONTROLLER));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(timeout_msg.to_string())
    }

    fn copy_env_vars(&self) -> Result<(), String> {
        let snap_port = self.shared.lock().unwrap().mixed_port;
        let port = if snap_port > 0 {
            snap_port
        } else {
            mihomo::read_file(&self.cfg.config_file)
                .map(|info| info.mixed_port)
                .unwrap_or(0)
        };
        if port == 0 {
            return Err(text::ERR_NO_MIXED_PORT_FOR_ENV.to_string());
        }
        let cmd = platform::format_env_vars_command(port);
        platform::copy_to_clipboard(&cmd)?;
        tracing::info!("Copied proxy environment variables for port {port}");
        Ok(())
    }

    fn spawn_background_task<F>(
        &self,
        name: &str,
        fallback_kind: AsyncTaskKind,
        status_note: Option<&'static str>,
        task: F,
    ) -> bool
    where
        F: FnOnce() -> (AsyncTaskKind, Option<String>) + Send + 'static,
    {
        if let Some(note) = status_note {
            self.set_status_note(Some(note));
        }
        let tx = self.tx.clone();
        if std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                struct PanicGuard {
                    tx: Sender<WorkerCommand>,
                    kind: AsyncTaskKind,
                    sent: bool,
                }
                impl Drop for PanicGuard {
                    fn drop(&mut self) {
                        if !self.sent {
                            let _ = self.tx.send(WorkerCommand::AsyncDone {
                                kind: self.kind,
                                error: Some("background task panicked".to_string()),
                            });
                        }
                    }
                }
                let mut guard = PanicGuard {
                    tx: tx.clone(),
                    kind: fallback_kind,
                    sent: false,
                };
                let (kind, error) = task();
                guard.sent = true;
                let _ = tx.send(WorkerCommand::AsyncDone { kind, error });
            })
            .is_err()
        {
            if status_note.is_some() {
                self.set_status_note(None);
            }
            return false;
        }
        true
    }

    fn start_test_group_latency(&mut self, group: String) -> Result<bool, String> {
        let client = self
            .client
            .clone()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())?;
        if self.testing_latency {
            return Ok(false);
        }
        if self.spawn_background_task(
            "mihomo-latency",
            AsyncTaskKind::LatencyTest,
            None,
            move || {
                (
                    AsyncTaskKind::LatencyTest,
                    client.test_group_delay(&group).err(),
                )
            },
        ) {
            self.testing_latency = true;
        }
        Ok(true)
    }

    fn start_refresh_subscriptions(&mut self) -> Result<bool, String> {
        let client = self
            .client
            .clone()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())?;
        if self.sub_task_active {
            return Ok(false);
        }
        if self.spawn_background_task(
            "mihomo-sub-refresh",
            AsyncTaskKind::RefreshSubscriptions,
            Some(text::STATUS_REFRESHING_SUBSCRIPTION),
            move || {
                let err = (|| -> Result<(), String> {
                    let providers = client.get_providers()?.providers;
                    let refreshable: Vec<String> = providers
                        .into_iter()
                        .filter(Provider::refreshable)
                        .map(|p| p.name)
                        .collect();
                    if refreshable.is_empty() {
                        return Err(text::ERR_NO_SUBSCRIPTION_PROVIDER.to_string());
                    }
                    for name in refreshable {
                        client.refresh_provider(&name)?;
                    }
                    Ok(())
                })()
                .err();
                (AsyncTaskKind::RefreshSubscriptions, err)
            },
        ) {
            self.sub_task_active = true;
        }
        Ok(true)
    }

    fn spawn_apply_subscription_reload(&mut self, client: Client) {
        let sub_cache = self
            .cfg
            .home_dir
            .join(mihomo::SUBSCRIPTION_LOCAL_PATH.trim_start_matches("./"));
        if self.spawn_background_task(
            "mihomo-sub-apply",
            AsyncTaskKind::ApplySubscription { reload_ok: false },
            Some(text::STATUS_UPDATING_SUBSCRIPTION),
            move || {
                let had_cached_sub = sub_cache.is_file();
                if let Err(err) = client.reload_configs() {
                    return (
                        AsyncTaskKind::ApplySubscription { reload_ok: false },
                        Some(err),
                    );
                }
                // When no cache existed before, `reload_configs` already fetched the new provider;
                // only force a provider refresh if replacing an existing cache or if reload didn't create it.
                let err = if had_cached_sub || !sub_cache.is_file() {
                    client
                        .refresh_provider(mihomo::SUBSCRIPTION_PROVIDER_NAME)
                        .err()
                } else {
                    None
                };
                (AsyncTaskKind::ApplySubscription { reload_ok: true }, err)
            },
        ) {
            self.sub_task_active = true;
        } else {
            self.state_sync.promote_reload_restore(true);
        }
    }

    fn set_subscription_url(&mut self, url: &str) -> Result<(), String> {
        let saved = self.capture_live_or_persisted_state();
        mihomo::apply_subscription_url(&self.cfg.config_file, url)?;
        if let Some(client) = self.client.clone()
            && client.probe().is_ok()
        {
            self.state_sync.arm_reload_restore(saved);
            if self.sub_task_active {
                self.pending_sub_reload = true;
                self.set_status_note(Some(text::STATUS_UPDATING_SUBSCRIPTION));
            } else {
                self.spawn_apply_subscription_reload(client);
            }
            return Ok(());
        }
        self.set_status_note(Some(text::STATUS_UPDATING_SUBSCRIPTION));
        self.state_sync.set_pending_restore(saved);
        let res = self.start_core_internal();
        self.set_status_note(None);
        res
    }

    fn capture_live_or_persisted_state(&self) -> PersistedState {
        let snap = self.shared.lock().unwrap();
        self.state_sync.capture_live_or_persisted(&snap)
    }

    fn apply_pending_restore_if_needed(&mut self, cfg: &mut CoreConfigs) {
        let Some(restore) = self.state_sync.take_pending_restore() else {
            return;
        };

        if let Some(want_tun) = restore.tun
            && want_tun != cfg.tun_enable
        {
            match self.apply_tun_state(want_tun) {
                Ok(()) => {
                    if let Some(client) = &self.client
                        && let Ok(updated) = client.get_configs()
                    {
                        *cfg = updated;
                    } else {
                        cfg.tun_enable = want_tun;
                    }
                }
                Err(err) => {
                    self.state_sync
                        .update(&self.cfg.home_dir, |s| s.tun = Some(false));
                    self.set_action_error(Some(err));
                }
            }
        }

        if let Some(ref mode) = restore.mode
            && !mode.is_empty()
            && !cfg.mode.eq_ignore_ascii_case(mode)
            && let Some(client) = &self.client
            && client.set_mode(mode).is_ok()
        {
            cfg.mode = mode.clone();
        }

        if restore.system_proxy {
            if cfg.mixed_port == 0 {
                self.set_action_error(Some(text::ERR_NO_MIXED_PORT_FOR_RESTORE.to_string()));
            } else if let Err(err) =
                platform::proxy_enable(cfg.mixed_port, &self.cfg.settings.proxy.bypass)
            {
                self.set_action_error(Some(err));
            }
        }
    }

    fn handle_quit(&mut self, interactive: bool) -> bool {
        let final_state = self.capture_live_or_persisted_state();
        self.state_sync
            .update(&self.cfg.home_dir, |s| *s = final_state);
        match self.stop_core_internal(interactive) {
            Ok(()) => {
                self.proxy.send(AppEvent::QuitCompleted);
                true
            }
            Err(err) => {
                self.stop_requested.store(false, Ordering::Release);
                self.proxy.send(AppEvent::QuitCancelled(err));
                false
            }
        }
    }

    fn active_pid(&mut self) -> Option<u32> {
        if let Some(ref mut managed) = self.managed {
            if matches!(managed.child.try_wait(), Ok(None)) {
                return Some(managed.pid);
            }
            self.managed = None;
        }
        if let Some(pid) = self.recorded_pid {
            if self
                .process_inspector
                .is_recorded_kernel_alive(pid, self.cfg.known_kernel_path.as_deref())
            {
                return Some(pid);
            }
            self.recorded_pid = None;
            process::clear_runtime_record(&self.cfg.home_dir);
        }
        None
    }

    fn resolve_live_controller(&mut self) -> bool {
        let reload = self.cfg.reload_settings_if_changed();
        if reload.changed || self.cfg.settings_error.is_none() {
            self.sync_config_error();
        }
        let tracked_alive = self.active_pid().is_some();
        let mut running = if !tracked_alive || self.client.is_none() || reload.changed {
            self.process_inspector
                .list_running_mihomo(self.cfg.known_kernel_path.as_deref())
        } else {
            Vec::new()
        };
        let kernel_running = tracked_alive || !running.is_empty();

        if reload.changed && !kernel_running {
            self.cfg.refresh_paths();
            self.sync_config_error();
        }

        let config_changed = self.cfg.config_file_changed();
        let need_resolve = reload.controller_changed
            || (reload.changed && !kernel_running)
            || config_changed
            || (kernel_running && (self.client.is_none() || !self.last_controller_ok));
        if need_resolve {
            if running.is_empty() && kernel_running {
                running = self
                    .process_inspector
                    .list_running_mihomo(self.cfg.known_kernel_path.as_deref());
            }
            self.re_resolve_controller(&running);
        }
        kernel_running
    }

    fn poll_controller_configs(
        &mut self,
        client_opt: Option<&Client>,
    ) -> (bool, String, u16, bool, Option<String>, Option<String>) {
        let Some(client) = client_opt else {
            return (false, String::new(), 0, false, None, None);
        };
        let web_ui_url = Some(client.format_web_ui_url(&self.cfg.settings.ui.web_url));
        match client.get_configs() {
            Ok(mut cfg) => {
                self.apply_pending_restore_if_needed(&mut cfg);
                if self.cached_version.is_empty()
                    && let Ok(ver) = client.get_version()
                {
                    self.cached_version = ver;
                }
                (
                    true,
                    cfg.mode,
                    cfg.mixed_port,
                    cfg.tun_enable,
                    None,
                    web_ui_url,
                )
            }
            Err(err) => (false, String::new(), 0, false, Some(err), web_ui_url),
        }
    }

    fn refresh(&mut self) {
        let kernel_running = self.resolve_live_controller();
        let client_opt = self.client.clone();
        let (controller_ok, mode, mixed_port, tun, controller_error, web_ui_url) =
            self.poll_controller_configs(client_opt.as_ref());
        self.last_controller_ok = controller_ok;

        let sysproxy = platform::proxy_is_enabled();
        self.cfg.refresh_autostart_if_stale();
        let autostart = self.cfg.cached_autostart;

        if controller_ok {
            self.state_sync
                .sync_live_if_idle(&self.cfg.home_dir, sysproxy, &mode, tun);
        }

        let (proxies_res, providers_res) = if controller_ok && let Some(client) = &client_opt {
            (Some(client.get_proxies()), Some(client.get_providers()))
        } else {
            (None, None)
        };

        {
            let mut snap = self.shared.lock().unwrap();
            snap.controller_ok = controller_ok;
            snap.version = if controller_ok {
                self.cached_version.clone()
            } else {
                String::new()
            };
            snap.mode = mode;
            snap.mixed_port = mixed_port;
            snap.tun = tun;
            snap.sysproxy = sysproxy;
            snap.autostart = autostart;
            snap.kernel_running = kernel_running;
            if client_opt.is_some() {
                snap.controller_error = if !controller_ok && !kernel_running {
                    None
                } else {
                    controller_error
                };
            } else if snap.controller_error.is_none() {
                snap.controller_error = Some(text::ERR_NO_CONTROLLER.to_string());
            }
            snap.web_ui_url = web_ui_url;
            snap.resolved_config = Some(self.cfg.config_file.clone());
            snap.page_size = self.cfg.settings.groups.page_size;

            if !controller_ok {
                snap.groups.clear();
                snap.providers.clear();
                snap.node_latency.clear();
            } else {
                let mut latencies = None;
                if let Some(Ok(proxies)) = proxies_res {
                    snap.groups = state::order_and_filter_groups(
                        proxies.groups,
                        &self.cfg.settings.groups.include,
                        &self.cfg.settings.groups.exclude,
                        &self.cfg.settings.groups.order,
                    );
                    latencies = Some(proxies.latency);
                }
                if let Some(Ok(prov)) = providers_res {
                    snap.providers = prov.providers;
                    if let Some(ref mut map) = latencies {
                        for (name, delay) in prov.latency {
                            map.entry(name)
                                .and_modify(|existing| {
                                    if existing.is_none() {
                                        *existing = delay;
                                    }
                                })
                                .or_insert(delay);
                        }
                    }
                }
                if let Some(map) = latencies {
                    snap.node_latency = map;
                }
            }
        }
        self.proxy
            .send(AppEvent::SnapshotUpdated { rebuild_menu: true });
    }

    fn set_status_note(&self, note: Option<&str>) {
        {
            let mut snap = self.shared.lock().unwrap();
            snap.status_note = note.map(ToString::to_string);
        }
        self.proxy.send(AppEvent::SnapshotUpdated {
            rebuild_menu: false,
        });
    }

    fn sync_config_error(&self) {
        let err = self.cfg.current_config_error();
        let mut snap = self.shared.lock().unwrap();
        snap.config_error = err;
    }

    fn set_action_error(&self, err: Option<String>) {
        let mut snap = self.shared.lock().unwrap();
        snap.action_error = err;
    }

    fn set_controller_error(&self, err: Option<String>) {
        let mut snap = self.shared.lock().unwrap();
        snap.controller_error = err;
    }
}

fn verify_or_enable_tun_after_elevation(client: &Client) -> Result<(), String> {
    client
        .set_tun(true)
        .map_err(|err| text::format_tun_enable_after_elevation_failed(&err))?;
    let verify = client.get_configs()?;
    if !verify.tun_enable {
        return Err(text::ERR_TUN_FAILED_AFTER_ELEVATION.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;

        let mut data = Vec::new();
        let mut buf = [0u8; 1024];
        let mut header_end = None;
        let mut content_len = 0usize;
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
            if header_end.is_none()
                && let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n")
            {
                let end = pos + 4;
                header_end = Some(end);
                let headers = String::from_utf8_lossy(&data[..end]);
                content_len = headers
                    .lines()
                    .find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.trim()
                            .eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
            }
            if let Some(end) = header_end
                && data.len() >= end + content_len
            {
                break;
            }
        }
        String::from_utf8_lossy(&data).into_owned()
    }

    #[test]
    fn verify_or_enable_tun_after_elevation_patches_and_verifies_tun() {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let mut tun_on = false;
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let req = read_http_request(&mut stream);
                if req.starts_with("PATCH /configs ") {
                    let (_, body) = req.split_once("\r\n\r\n").unwrap();
                    let json: serde_json::Value = serde_json::from_str(body).unwrap();
                    assert_eq!(json["tun"]["enable"].as_bool(), Some(true));
                    tun_on = true;
                    stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                        .unwrap();
                } else {
                    let body = format!(
                        r#"{{"mode":"rule","mixed-port":7890,"tun":{{"enable":{tun_on}}}}}"#
                    );
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(resp.as_bytes()).unwrap();
                }
            }
        });

        let client = Client::new(&format!("127.0.0.1:{port}"), "", 2000).unwrap();
        verify_or_enable_tun_after_elevation(&client).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn verify_or_enable_tun_after_elevation_errors_when_still_disabled() {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let req = read_http_request(&mut stream);
                if req.starts_with("PATCH /configs ") {
                    stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                        .unwrap();
                } else {
                    let body = r#"{"mode":"rule","mixed-port":7890,"tun":{"enable":false}}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(resp.as_bytes()).unwrap();
                }
            }
        });

        let client = Client::new(&format!("127.0.0.1:{port}"), "", 2000).unwrap();
        let err = verify_or_enable_tun_after_elevation(&client).unwrap_err();
        assert_eq!(err, text::ERR_TUN_FAILED_AFTER_ELEVATION);
        handle.join().unwrap();
    }

    fn test_worker(
        root: &std::path::Path,
        settings: TraySettings,
        startup_error: Option<String>,
    ) -> (Worker, mpsc::Receiver<WorkerCommand>, SharedSnapshot) {
        let settings_path = crate::config::tray::settings_path(root);
        let shared = Arc::new(Mutex::new(crate::state::Snapshot::default()));
        let (ui_tx, _ui_rx) = mpsc::channel();
        let (worker, rx) = Worker::new(
            root.to_path_buf(),
            settings_path,
            settings,
            Arc::clone(&shared),
            UiNotifier::Channel(ui_tx),
            startup_error,
            Arc::new(AtomicBool::new(false)),
        );
        (worker, rx, shared)
    }

    #[test]
    fn settings_reload_tracks_non_controller_changes_and_refreshes_paths() {
        let root = crate::config::test_temp_dir("worker_reload_paths");
        let core_dir = root.join("custom");
        std::fs::create_dir_all(&core_dir).unwrap();
        let exe = core_dir.join("my-core");
        std::fs::write(&exe, b"bin").unwrap();

        let settings_path = crate::config::tray::settings_path(&root);
        std::fs::write(&settings_path, "mihomo:\n  path: ./custom/missing\n").unwrap();
        let initial = crate::config::tray::load(&settings_path).unwrap();
        let (mut worker, _rx, shared) = test_worker(&root, initial, None);

        worker.cfg.refresh_paths();
        worker.sync_config_error();
        assert!(shared.lock().unwrap().config_error.is_some());

        std::fs::write(
            &settings_path,
            "mihomo:\n  path: ./custom/my-core\n  config: ./alt/config.yaml\n",
        )
        .unwrap();
        // Force mtime check to observe edit deterministically across fast filesystems.
        worker.cfg.last_settings_mtime = None;
        worker.resolve_live_controller();

        assert_eq!(worker.cfg.known_kernel_path.as_deref(), Some(exe.as_path()));
        assert_eq!(worker.cfg.config_file, root.join("alt").join("config.yaml"));
        assert_eq!(shared.lock().unwrap().config_error, None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stopped_core_suppresses_http_controller_error_and_surfaces_startup_errors() {
        let root = crate::config::test_temp_dir("worker_error_mask");
        let cfg_dir = root.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_port = listener.local_addr().unwrap().port();
        drop(listener);
        std::fs::write(
            cfg_dir.join("config.yaml"),
            format!("external-controller: 127.0.0.1:{dead_port}\n"),
        )
        .unwrap();

        let mut settings = TraySettings::default();
        settings.mihomo.auto_start = false;
        let (mut worker, _rx, shared) = test_worker(&root, settings, None);
        worker.set_action_error(Some(text::ERR_CORE_NOT_FOUND.to_string()));
        worker.startup();

        let snap = shared.lock().unwrap();
        assert!(!snap.controller_ok);
        assert_eq!(snap.controller_error, None);
        assert_eq!(snap.error(), Some(text::ERR_CORE_NOT_FOUND));
        drop(snap);

        worker.set_action_error(None);
        worker.refresh();
        assert_eq!(shared.lock().unwrap().error(), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn async_done_success_does_not_clobber_existing_action_error() {
        let root = crate::config::test_temp_dir("worker_async_err");
        let (mut worker, _rx, shared) = test_worker(&root, TraySettings::default(), None);

        worker.set_action_error(Some("subscription refresh failed".to_string()));
        worker.testing_latency = true;

        let clears = worker
            .handle_command(WorkerCommand::AsyncDone {
                kind: AsyncTaskKind::LatencyTest,
                error: None,
            })
            .unwrap();
        assert!(!clears);
        assert!(!worker.testing_latency);
        assert_eq!(
            shared.lock().unwrap().action_error.as_deref(),
            Some("subscription refresh failed")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn subscription_async_reload_preserves_pending_restore_until_async_done() {
        use std::io::Write;
        use std::net::TcpListener;

        let root = crate::config::test_temp_dir("worker_sub_restore");
        let cfg_dir = root.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            cfg_dir.join("config.yaml"),
            format!("mixed-port: 7890\nmode: rule\nexternal-controller: 127.0.0.1:{port}\n"),
        )
        .unwrap();

        let (done_tx, done_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut mode = "rule".to_string();
            let mut tun_on = false;
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                let req = read_http_request(&mut stream);
                let (status, body) = if req.starts_with("GET /version ") {
                    ("200 OK", r#"{"version":"v1.19.0"}"#.to_string())
                } else if req.starts_with("GET /configs ") {
                    (
                        "200 OK",
                        format!(
                            r#"{{"mode":"{mode}","mixed-port":7890,"tun":{{"enable":{tun_on}}}}}"#
                        ),
                    )
                } else if req.starts_with("PUT /configs?force=true ") {
                    mode = "rule".to_string();
                    tun_on = false;
                    ("204 No Content", String::new())
                } else if req.starts_with("PUT /providers/proxies/subscription ") {
                    ("204 No Content", String::new())
                } else if req.starts_with("PATCH /configs ") {
                    let (_, payload) = req.split_once("\r\n\r\n").unwrap();
                    let json: serde_json::Value = serde_json::from_str(payload).unwrap();
                    if let Some(m) = json.get("mode").and_then(|v| v.as_str()) {
                        mode = m.to_string();
                    }
                    if let Some(t) = json
                        .get("tun")
                        .and_then(|v| v.get("enable"))
                        .and_then(|v| v.as_bool())
                    {
                        tun_on = t;
                    }
                    ("204 No Content", String::new())
                } else if req.starts_with("GET /proxies ") {
                    ("200 OK", r#"{"proxies":{}}"#.to_string())
                } else if req.starts_with("GET /providers/proxies ") {
                    ("200 OK", r#"{"providers":{}}"#.to_string())
                } else if req.starts_with("POST /__stop ") {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                    break;
                } else {
                    ("404 Not Found", String::new())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).unwrap();
                if req.starts_with("PATCH /configs ") && mode == "global" && !tun_on {
                    let _ = done_tx.send(());
                }
            }
        });

        let (mut worker, rx, shared) = test_worker(&root, TraySettings::default(), None);
        worker.client = Some(Client::new(&format!("127.0.0.1:{port}"), "", 2000).unwrap());
        {
            let mut snap = shared.lock().unwrap();
            snap.controller_ok = true;
            snap.mode = "global".to_string();
            snap.tun = false;
        }
        worker.state_sync.persisted = Some(PersistedState {
            system_proxy: false,
            mode: Some("global".to_string()),
            tun: Some(false),
        });

        assert!(
            worker
                .handle_command(WorkerCommand::SetSubscriptionUrl(
                    "https://example.com/sub".to_string()
                ))
                .unwrap()
        );
        assert!(worker.state_sync.pending_restore.is_none());
        assert!(worker.state_sync.pending_reload_restore.is_some());

        let mut mid_cfg = CoreConfigs {
            mode: "rule".to_string(),
            mixed_port: 7890,
            tun_enable: false,
        };
        worker.apply_pending_restore_if_needed(&mut mid_cfg);
        worker
            .state_sync
            .sync_live_if_idle(&worker.cfg.home_dir, false, "rule", false);
        assert_eq!(
            worker
                .state_sync
                .persisted
                .as_ref()
                .and_then(|s| s.mode.as_deref()),
            Some("global")
        );

        let async_cmd = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        worker.handle_command(async_cmd).unwrap();
        assert!(worker.state_sync.pending_restore.is_some());

        let (ok, mode, _, _, _, _) = worker.poll_controller_configs(worker.client.clone().as_ref());
        assert!(ok);
        assert_eq!(mode, "global");
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let _ = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .unwrap()
            .write_all(b"POST /__stop HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n");
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
}
