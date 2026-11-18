//! Background worker thread for mihomo lifecycle, polling, and controller commands.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use winit::event_loop::EventLoopProxy;

use crate::config::mihomo;
use crate::config::state_store::{self, PersistedState};
use crate::config::tray::{self, TraySettings};
use crate::core::api::{Client, CoreConfigs};
use crate::core::discover;
use crate::core::process::{self, ManagedChild, ProcessInspector, RunningProcess, RuntimeRecord};
use crate::platform;
use crate::state::{self, AppEvent, Provider, SharedSnapshot, WorkerCommand};
use crate::text;

const KERNEL_STARTUP_BUDGET: Duration = Duration::from_secs(6);
const KERNEL_RESTART_BUDGET: Duration = Duration::from_secs(10);
const AUTOSTART_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

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
        proxy,
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
    portable_root: PathBuf,
    settings_path: PathBuf,
    settings: TraySettings,
    last_settings_mtime: Option<SystemTime>,
    last_config_mtime: Option<SystemTime>,
    settings_error: Option<String>,
    kernel_path_error: Option<String>,
    last_controller_ok: bool,
    shared: SharedSnapshot,
    proxy: EventLoopProxy<AppEvent>,
    stop_requested: Arc<AtomicBool>,
    tx: Sender<WorkerCommand>,
    testing_latency: Arc<AtomicBool>,
    refreshing_subs: Arc<AtomicBool>,
    updating_sub: Arc<AtomicBool>,
    process_inspector: ProcessInspector,
    managed: Option<ManagedChild>,
    known_kernel_path: Option<PathBuf>,
    recorded_pid: Option<u32>,
    client: Option<Client>,
    cached_version: String,
    cached_autostart: bool,
    last_autostart_check: Option<Instant>,
    home_dir: PathBuf,
    config_file: PathBuf,
    persisted_state: Option<PersistedState>,
    pending_restore: Option<PersistedState>,
}

impl Worker {
    fn new(
        portable_root: PathBuf,
        settings_path: PathBuf,
        settings: TraySettings,
        shared: SharedSnapshot,
        proxy: EventLoopProxy<AppEvent>,
        startup_error: Option<String>,
        stop_requested: Arc<AtomicBool>,
    ) -> (Self, mpsc::Receiver<WorkerCommand>) {
        let (tx, rx) = mpsc::channel::<WorkerCommand>();
        let (home_dir, config_file) =
            discover::resolve_home_and_config(&portable_root, &settings, None);
        let last_settings_mtime = file_mtime(&settings_path);
        let last_config_mtime = file_mtime(&config_file);
        {
            let mut snap = shared.lock().unwrap();
            snap.page_size = settings.groups.page_size;
            snap.resolved_config = Some(config_file.clone());
            snap.config_error = startup_error.clone();
        }
        let worker = Self {
            portable_root,
            settings_path,
            settings,
            last_settings_mtime,
            last_config_mtime,
            settings_error: startup_error,
            kernel_path_error: None,
            last_controller_ok: false,
            shared,
            proxy,
            stop_requested,
            tx,
            testing_latency: Arc::new(AtomicBool::new(false)),
            refreshing_subs: Arc::new(AtomicBool::new(false)),
            updating_sub: Arc::new(AtomicBool::new(false)),
            process_inspector: ProcessInspector::new(),
            managed: None,
            known_kernel_path: None,
            recorded_pid: None,
            client: None,
            cached_version: String::new(),
            cached_autostart: false,
            last_autostart_check: None,
            home_dir,
            config_file,
            persisted_state: None,
            pending_restore: None,
        };
        (worker, rx)
    }

    fn run(&mut self, rx: mpsc::Receiver<WorkerCommand>) {
        self.startup();

        loop {
            let interval = Duration::from_millis(self.settings.ui.poll_interval_ms as u64);
            match rx.recv_timeout(interval) {
                Ok(WorkerCommand::Quit { interactive }) => {
                    if self.handle_quit(interactive) {
                        break;
                    }
                    self.refresh();
                }
                Ok(cmd) => {
                    let mut burst_error = self.handle_command(cmd).err();
                    // Coalesce burst commands before doing a single refresh poll.
                    while let Ok(extra) = rx.try_recv() {
                        if let WorkerCommand::Quit { interactive } = extra {
                            if self.handle_quit(interactive) {
                                return;
                            }
                            break;
                        }
                        if let Err(err) = self.handle_command(extra) {
                            burst_error = Some(err);
                        }
                    }
                    self.set_action_error(burst_error);
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
        self.load_initial_persisted_state();
        self.refresh_autostart_cache();
        let running = self
            .process_inspector
            .list_running_mihomo(self.known_kernel_path.as_deref());
        self.re_resolve_controller(&running);

        let controller_alive = self.client.as_ref().is_some_and(|c| c.probe().is_ok());

        if controller_alive {
            // Core is already running; preserve its live mode/TUN instead of overriding from disk.
            self.pending_restore = None;
        } else if running.is_empty()
            && self.settings.mihomo.auto_start
            && let Err(err) = self.start_core_internal()
        {
            tracing::warn!("Auto-start core failed: {err}");
            let has_error = self.shared.lock().unwrap().action_error.is_some();
            if !has_error {
                self.set_action_error(Some(err));
            }
        }
        self.refresh();
    }

    fn load_initial_persisted_state(&mut self) {
        self.persisted_state = state_store::load(&self.home_dir);
        self.pending_restore = self.persisted_state.clone();
    }

    fn update_persisted_state(&mut self, update: impl FnOnce(&mut PersistedState)) {
        let state = self
            .persisted_state
            .get_or_insert_with(PersistedState::default);
        update(state);
        state_store::save(&self.home_dir, state);
    }

    fn load_settings_from_disk(&mut self) -> bool {
        self.last_settings_mtime = file_mtime(&self.settings_path);
        let prev_controller = self.settings.controller.clone();
        match tray::load(&self.settings_path) {
            Ok(reloaded) => {
                self.settings = reloaded;
                self.settings_error = None;
                self.sync_config_error();
                self.settings.controller != prev_controller
            }
            Err(err) => {
                self.settings_error = Some(err);
                self.sync_config_error();
                false
            }
        }
    }

    fn refresh_paths(&mut self) {
        match discover::find_kernel(&self.portable_root, &self.settings) {
            Ok(path) => {
                self.known_kernel_path = path;
                self.kernel_path_error = None;
            }
            Err(err) => {
                self.known_kernel_path = None;
                self.kernel_path_error = Some(err);
            }
        }
        let (home, cfg) = discover::resolve_home_and_config(
            &self.portable_root,
            &self.settings,
            self.known_kernel_path.as_deref(),
        );
        self.home_dir = home;
        self.config_file = cfg;
        self.last_config_mtime = file_mtime(&self.config_file);
        self.sync_config_error();
    }

    fn reload_settings_if_changed(&mut self) -> bool {
        if file_mtime(&self.settings_path) == self.last_settings_mtime {
            return false;
        }
        self.load_settings_from_disk()
    }

    fn refresh_autostart_cache(&mut self) {
        self.cached_autostart = platform::autostart_is_enabled();
        self.last_autostart_check = Some(Instant::now());
    }

    fn refresh_autostart_if_stale(&mut self) {
        if self
            .last_autostart_check
            .is_none_or(|t| t.elapsed() >= AUTOSTART_REFRESH_INTERVAL)
        {
            self.refresh_autostart_cache();
        }
    }

    fn reload_paths_and_runtime(&mut self) {
        self.reload_settings_if_changed();
        self.refresh_paths();

        if let Some(rec) = process::load_runtime_record(&self.home_dir) {
            if self
                .process_inspector
                .is_recorded_kernel_alive(rec.pid, Some(&rec.kernel_path))
            {
                self.recorded_pid = Some(rec.pid);
                if self.known_kernel_path.is_none() {
                    self.known_kernel_path = Some(rec.kernel_path);
                }
            } else {
                self.recorded_pid = None;
                process::clear_runtime_record(&self.home_dir);
            }
        }
    }

    fn re_resolve_controller(&mut self, running: &[RunningProcess]) {
        match discover::resolve_controller(
            &self.portable_root,
            &self.settings,
            running,
            Some(&self.config_file),
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

    fn handle_command(&mut self, cmd: WorkerCommand) -> Result<(), String> {
        match cmd {
            WorkerCommand::SetMode(mode) => self.set_mode(&mode),
            WorkerCommand::ToggleSystemProxy => self.toggle_system_proxy(),
            WorkerCommand::ToggleTun => self.toggle_tun(),
            WorkerCommand::SelectProxy { group, member } => {
                self.with_client(|c| c.select_proxy(&group, &member))
            }
            WorkerCommand::UnfixGroup(group) => self.with_client(|c| c.unfix_group(&group)),
            WorkerCommand::TestGroupLatency(group) => self.start_test_group_latency(group),
            WorkerCommand::RefreshSubscriptions => self.start_refresh_subscriptions(),
            WorkerCommand::SetSubscriptionUrl(url) => self.set_subscription_url(&url),
            WorkerCommand::CopyEnvVars => self.copy_env_vars(),
            WorkerCommand::RestartCore => self.force_restart_core(),
            WorkerCommand::ToggleAutostart => {
                let enabled = platform::autostart_is_enabled();
                let res = platform::autostart_set(!enabled);
                self.refresh_autostart_cache();
                res
            }
            WorkerCommand::AsyncDone { error, clear_note } => {
                if clear_note {
                    self.set_status_note(None);
                }
                match error {
                    Some(err) => Err(err),
                    None => Ok(()),
                }
            }
            WorkerCommand::Quit { .. } => Ok(()),
        }
    }

    fn set_mode(&mut self, mode: &str) -> Result<(), String> {
        self.with_client(|c| c.set_mode(mode))?;
        self.update_persisted_state(|s| s.mode = Some(mode.to_string()));
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
            self.update_persisted_state(|s| s.system_proxy = false);
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
        platform::proxy_enable(cfg.mixed_port, &self.settings.proxy.bypass)?;
        self.update_persisted_state(|s| s.system_proxy = true);
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
        self.update_persisted_state(|s| s.tun = Some(want_enable));
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
            .list_running_mihomo(self.known_kernel_path.as_deref());
        let target = process::pick_target_process(
            &running,
            self.active_pid(),
            self.known_kernel_path.as_deref(),
        )
        .ok_or_else(|| text::ERR_LOCATE_ELEVATE_PROCESS.to_string())?;

        let kernel_path = target.exe.clone().or_else(|| self.known_kernel_path.clone());
        let launch_args =
            discover::build_launch_args(&self.portable_root, &self.settings, kernel_path.as_deref())?;

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
                &self.home_dir,
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
            .list_running_mihomo(self.known_kernel_path.as_deref());
        if process::pick_target_process(
            &running,
            self.active_pid(),
            self.known_kernel_path.as_deref(),
        )
        .is_some()
        {
            self.re_resolve_controller(&running);
            return Ok(());
        }

        let kernel_path = discover::find_kernel(&self.portable_root, &self.settings)?
            .ok_or_else(|| text::ERR_CORE_NOT_FOUND.to_string())?;
        self.known_kernel_path = Some(kernel_path.clone());

        let (home_dir, config_file) = discover::resolve_home_and_config(
            &self.portable_root,
            &self.settings,
            Some(&kernel_path),
        );
        self.home_dir = home_dir.clone();
        self.config_file = config_file.clone();
        let _ = std::fs::create_dir_all(&home_dir);
        mihomo::ensure_default_config(&config_file);

        let args =
            discover::build_launch_args(&self.portable_root, &self.settings, Some(&kernel_path))?;

        self.set_status_note(Some(text::STATUS_STARTING_CORE));
        let child = process::spawn_normal(&kernel_path, &args)?;
        let pid = child.pid;
        self.managed = Some(child);
        self.recorded_pid = Some(pid);
        self.cached_version.clear();
        process::save_runtime_record(
            &self.home_dir,
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
        if self
            .persisted_state
            .as_ref()
            .is_some_and(|s| s.system_proxy)
            && platform::proxy_is_enabled()
            && let Err(err) = platform::proxy_disable()
        {
            tracing::warn!("Failed to disable system proxy on shutdown: {err}");
        }
    }

    fn stop_core_internal(&mut self, interactive: bool) -> Result<(), String> {
        self.set_status_note(Some(text::STATUS_STOPPING_CORE));
        let res = self.stop_active_kernel(interactive);
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
        self.pending_restore = Some(saved);
        self.reload_paths_and_runtime();
        let running = self
            .process_inspector
            .list_running_mihomo(self.known_kernel_path.as_deref());
        if !running.is_empty() {
            self.set_status_note(Some(text::STATUS_RESTARTING_CORE));
            let stop_res = self.stop_active_kernel(true);
            self.set_status_note(None);
            stop_res?;
        }
        self.start_core_internal()
    }

    fn stop_active_kernel(&mut self, interactive: bool) -> Result<(), String> {
        // First try managed non-elevated child directly.
        if let Some(mut managed) = self.managed.take() {
            if matches!(managed.child.try_wait(), Ok(None)) {
                if managed.child.kill().is_ok() {
                    let _ = managed.child.wait();
                    self.recorded_pid = None;
                    self.cached_version.clear();
                    process::clear_runtime_record(&self.home_dir);
                    return Ok(());
                }
            } else {
                self.recorded_pid = None;
                self.cached_version.clear();
                process::clear_runtime_record(&self.home_dir);
                return Ok(());
            }
        }

        let running = self
            .process_inspector
            .list_running_mihomo(self.known_kernel_path.as_deref());
        let target = process::pick_target_process(
            &running,
            self.active_pid(),
            self.known_kernel_path.as_deref(),
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
            process::clear_runtime_record(&self.home_dir);
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

        self.recorded_pid = None;
        self.cached_version.clear();
        process::clear_runtime_record(&self.home_dir);
        Ok(())
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
                process::clear_runtime_record(&self.home_dir);
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
            mihomo::read_file(&self.config_file)
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
        flag: &Arc<AtomicBool>,
        status_note: Option<&'static str>,
        task: F,
    ) where
        F: FnOnce() -> Result<(), String> + Send + 'static,
    {
        if flag
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        if let Some(note) = status_note {
            self.set_status_note(Some(note));
        }
        let flag_clone = Arc::clone(flag);
        let tx = self.tx.clone();
        let clear_note = status_note.is_some();
        if std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                struct ResetFlag(Arc<AtomicBool>);
                impl Drop for ResetFlag {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let error = {
                    let _reset = ResetFlag(flag_clone);
                    task().err()
                };
                let _ = tx.send(WorkerCommand::AsyncDone { error, clear_note });
            })
            .is_err()
        {
            flag.store(false, Ordering::Release);
            if clear_note {
                self.set_status_note(None);
            }
        }
    }

    fn start_test_group_latency(&self, group: String) -> Result<(), String> {
        let client = self
            .client
            .clone()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())?;
        self.spawn_background_task("mihomo-latency", &self.testing_latency, None, move || {
            client.test_group_delay(&group)
        });
        Ok(())
    }

    fn start_refresh_subscriptions(&self) -> Result<(), String> {
        let client = self
            .client
            .clone()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())?;
        self.spawn_background_task(
            "mihomo-sub-refresh",
            &self.refreshing_subs,
            Some(text::STATUS_REFRESHING_SUBSCRIPTION),
            move || {
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
            },
        );
        Ok(())
    }

    fn set_subscription_url(&mut self, url: &str) -> Result<(), String> {
        let saved = self.capture_live_or_persisted_state();
        mihomo::apply_subscription_url(&self.config_file, url)?;
        if let Some(client) = self.client.clone()
            && client.probe().is_ok()
        {
            self.pending_restore = Some(saved);
            let sub_cache = self
                .home_dir
                .join(mihomo::SUBSCRIPTION_LOCAL_PATH.trim_start_matches("./"));
            self.spawn_background_task(
                "mihomo-sub-apply",
                &self.updating_sub,
                Some(text::STATUS_UPDATING_SUBSCRIPTION),
                move || {
                    let had_cached_sub = sub_cache.is_file();
                    client.reload_configs()?;
                    // When no cache existed before, `reload_configs` already fetched the new provider;
                    // only force a provider refresh if replacing an existing cache or if reload didn't create it.
                    if had_cached_sub || !sub_cache.is_file() {
                        client.refresh_provider(mihomo::SUBSCRIPTION_PROVIDER_NAME)?;
                    }
                    Ok(())
                },
            );
            return Ok(());
        }
        self.set_status_note(Some(text::STATUS_UPDATING_SUBSCRIPTION));
        self.pending_restore = Some(saved);
        let res = self.start_core_internal();
        self.set_status_note(None);
        res
    }

    fn capture_live_or_persisted_state(&self) -> PersistedState {
        if let Some(pending) = &self.pending_restore {
            return pending.clone();
        }
        let snap = self.shared.lock().unwrap();
        if snap.controller_ok {
            PersistedState {
                system_proxy: platform::proxy_is_enabled(),
                mode: (!snap.mode.is_empty()).then(|| snap.mode.clone()),
                tun: Some(snap.tun),
            }
        } else {
            self.persisted_state.clone().unwrap_or_default()
        }
    }

    fn apply_pending_restore_if_needed(&mut self, cfg: &mut CoreConfigs) {
        let Some(restore) = self.pending_restore.take() else {
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
                    self.update_persisted_state(|s| s.tun = Some(false));
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
                platform::proxy_enable(cfg.mixed_port, &self.settings.proxy.bypass)
            {
                self.set_action_error(Some(err));
            }
        }
    }

    fn handle_quit(&mut self, interactive: bool) -> bool {
        let final_state = self.capture_live_or_persisted_state();
        self.update_persisted_state(|s| *s = final_state);
        match self.stop_core_internal(interactive) {
            Ok(()) => {
                let _ = self.proxy.send_event(AppEvent::QuitCompleted);
                true
            }
            Err(err) => {
                self.stop_requested.store(false, Ordering::Release);
                let _ = self.proxy.send_event(AppEvent::QuitCancelled(err));
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
                .is_recorded_kernel_alive(pid, self.known_kernel_path.as_deref())
            {
                return Some(pid);
            }
            self.recorded_pid = None;
            process::clear_runtime_record(&self.home_dir);
        }
        None
    }

    fn resolve_live_controller(&mut self) -> bool {
        let settings_changed = self.reload_settings_if_changed();
        let tracked_alive = self.active_pid().is_some();
        let mut running = if !tracked_alive || self.client.is_none() || settings_changed {
            self.process_inspector
                .list_running_mihomo(self.known_kernel_path.as_deref())
        } else {
            Vec::new()
        };
        let kernel_running = tracked_alive || !running.is_empty();

        if settings_changed && !kernel_running {
            self.refresh_paths();
        }

        let config_mtime = file_mtime(&self.config_file);
        let config_changed = config_mtime != self.last_config_mtime;
        if config_changed {
            self.last_config_mtime = config_mtime;
        }

        let need_resolve = settings_changed
            || config_changed
            || (kernel_running && (self.client.is_none() || !self.last_controller_ok));
        if need_resolve {
            if running.is_empty() && kernel_running {
                running = self
                    .process_inspector
                    .list_running_mihomo(self.known_kernel_path.as_deref());
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
        let web_ui_url = Some(client.format_web_ui_url(&self.settings.ui.web_url));
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

    fn sync_live_persisted_state(&mut self, sysproxy: bool, mode: &str, tun: bool) {
        if self.pending_restore.is_some() {
            return;
        }
        let live = PersistedState {
            system_proxy: sysproxy,
            mode: (!mode.is_empty()).then(|| mode.to_string()),
            tun: Some(tun),
        };
        if self.persisted_state.as_ref() != Some(&live) {
            self.update_persisted_state(|s| *s = live);
        }
    }

    fn refresh(&mut self) {
        let kernel_running = self.resolve_live_controller();
        let client_opt = self.client.clone();
        let (controller_ok, mode, mixed_port, tun, controller_error, web_ui_url) =
            self.poll_controller_configs(client_opt.as_ref());
        self.last_controller_ok = controller_ok;

        let sysproxy = platform::proxy_is_enabled();
        self.refresh_autostart_if_stale();
        let autostart = self.cached_autostart;

        if controller_ok {
            self.sync_live_persisted_state(sysproxy, &mode, tun);
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
                snap.controller_error = controller_error;
            } else if snap.controller_error.is_none() {
                snap.controller_error = Some(text::ERR_NO_CONTROLLER.to_string());
            }
            snap.web_ui_url = web_ui_url;
            snap.resolved_config = Some(self.config_file.clone());
            snap.page_size = self.settings.groups.page_size;

            if !controller_ok {
                snap.groups.clear();
                snap.providers.clear();
                snap.node_latency.clear();
            } else {
                match (proxies_res, providers_res) {
                    (Some(Ok(proxies)), Some(Ok(prov))) => {
                        snap.groups = state::order_and_filter_groups(
                            proxies.groups,
                            &self.settings.groups.include,
                            &self.settings.groups.exclude,
                            &self.settings.groups.order,
                        );
                        snap.providers = prov.providers;
                        let mut latencies = proxies.latency;
                        for (name, delay) in prov.latency {
                            latencies
                                .entry(name)
                                .and_modify(|existing| {
                                    if existing.is_none() {
                                        *existing = delay;
                                    }
                                })
                                .or_insert(delay);
                        }
                        snap.node_latency = latencies;
                    }
                    (Some(Ok(proxies)), _) => {
                        snap.groups = state::order_and_filter_groups(
                            proxies.groups,
                            &self.settings.groups.include,
                            &self.settings.groups.exclude,
                            &self.settings.groups.order,
                        );
                        snap.node_latency = proxies.latency;
                    }
                    (_, Some(Ok(prov))) => {
                        snap.providers = prov.providers;
                    }
                    _ => {}
                }
            }
        }
        let _ = self
            .proxy
            .send_event(AppEvent::SnapshotUpdated { rebuild_menu: true });
    }

    fn set_status_note(&self, note: Option<&str>) {
        {
            let mut snap = self.shared.lock().unwrap();
            snap.status_note = note.map(ToString::to_string);
        }
        let _ = self.proxy.send_event(AppEvent::SnapshotUpdated {
            rebuild_menu: false,
        });
    }

    fn set_config_error(&self, err: Option<String>) {
        let mut snap = self.shared.lock().unwrap();
        snap.config_error = err;
    }

    fn sync_config_error(&self) {
        let err = self
            .settings_error
            .clone()
            .or_else(|| self.kernel_path_error.clone());
        self.set_config_error(err);
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

fn file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
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
}
