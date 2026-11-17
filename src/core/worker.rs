//! Background worker thread for mihomo lifecycle, polling, and controller commands.

use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;

use crate::config::mihomo;
use crate::config::state_store::{self, PersistedState};
use crate::config::tray::{self, TraySettings};
use crate::core::api::{Client, CoreConfigs};
use crate::core::discover;
use crate::core::process::{self, ManagedChild, RunningProcess, RuntimeRecord};
use crate::platform;
use crate::state::{AppEvent, Group, Provider, SharedSnapshot, WorkerCommand};
use crate::text;

const KERNEL_STARTUP_BUDGET: Duration = Duration::from_secs(6);
const KERNEL_RESTART_BUDGET: Duration = Duration::from_secs(10);

pub fn spawn(
    portable_root: PathBuf,
    settings_path: PathBuf,
    initial_settings: TraySettings,
    shared: SharedSnapshot,
    proxy: EventLoopProxy<AppEvent>,
    startup_error: Option<String>,
) -> Sender<WorkerCommand> {
    let (tx, rx) = mpsc::channel::<WorkerCommand>();

    std::thread::Builder::new()
        .name("mihomo-worker".to_string())
        .spawn(move || {
            let mut worker = Worker::new(
                portable_root,
                settings_path,
                initial_settings,
                shared,
                proxy,
                startup_error,
            );
            worker.run(rx);
        })
        .expect("failed to spawn worker thread");

    tx
}

struct Worker {
    portable_root: PathBuf,
    settings_path: PathBuf,
    settings: TraySettings,
    shared: SharedSnapshot,
    proxy: EventLoopProxy<AppEvent>,
    managed: Option<ManagedChild>,
    known_kernel_path: Option<PathBuf>,
    recorded_pid: Option<u32>,
    client: Option<Client>,
    cached_version: String,
    home_dir: PathBuf,
    config_file: PathBuf,
    persisted_state: PersistedState,
    has_persisted_state: bool,
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
    ) -> Self {
        let (home_dir, config_file) =
            discover::resolve_home_and_config(&portable_root, &settings, None);
        {
            let mut snap = shared.lock().unwrap();
            snap.page_size = settings.groups.page_size;
            snap.resolved_config = Some(config_file.clone());
            if let Some(err) = startup_error {
                snap.action_error = Some(err);
            }
        }
        Self {
            portable_root,
            settings_path,
            settings,
            shared,
            proxy,
            managed: None,
            known_kernel_path: None,
            recorded_pid: None,
            client: None,
            cached_version: String::new(),
            home_dir,
            config_file,
            persisted_state: PersistedState::default(),
            has_persisted_state: false,
            pending_restore: None,
        }
    }

    fn run(&mut self, rx: mpsc::Receiver<WorkerCommand>) {
        self.startup();

        loop {
            let interval = Duration::from_millis(self.settings.ui.poll_interval_ms as u64);
            match rx.recv_timeout(interval) {
                Ok(WorkerCommand::Quit) => {
                    self.handle_quit();
                    break;
                }
                Ok(cmd) => {
                    self.handle_command(cmd);
                    // Coalesce burst commands before doing a full poll.
                    while let Ok(extra) = rx.try_recv() {
                        if matches!(extra, WorkerCommand::Quit) {
                            self.handle_quit();
                            return;
                        }
                        self.handle_command(extra);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.refresh(true);
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    fn startup(&mut self) {
        self.reload_paths_and_runtime();
        self.load_initial_persisted_state();
        let running = process::list_running_mihomo(self.known_kernel_path.as_deref());
        self.re_resolve_controller(&running);

        let controller_alive = self.client.as_ref().is_some_and(|c| c.probe().is_ok());

        if !controller_alive && running.is_empty() && self.settings.mihomo.auto_start {
            let _ = self.start_core_internal();
        }
        self.refresh(true);
    }

    fn load_initial_persisted_state(&mut self) {
        match state_store::load(&self.home_dir) {
            Some(st) => {
                self.has_persisted_state = true;
                self.pending_restore = Some(st.clone());
                self.persisted_state = st;
            }
            None => {
                self.has_persisted_state = false;
                self.pending_restore = None;
                self.persisted_state = PersistedState::default();
            }
        }
    }

    fn save_persisted_state(&mut self) {
        self.has_persisted_state = true;
        state_store::save(&self.home_dir, &self.persisted_state);
    }

    fn reload_paths_and_runtime(&mut self) {
        if let Ok(reloaded) = tray::load(&self.settings_path) {
            self.settings = reloaded;
        }
        let kernel = discover::find_kernel(&self.portable_root, &self.settings);
        match kernel {
            Ok(Some(k)) => {
                self.known_kernel_path = Some(k.path().to_path_buf());
            }
            Ok(None) => {
                self.known_kernel_path = None;
            }
            Err(err) => {
                self.set_action_error(Some(err));
            }
        }

        let (home, cfg) = discover::resolve_home_and_config(
            &self.portable_root,
            &self.settings,
            self.known_kernel_path.as_deref(),
        );
        self.home_dir = home;
        self.config_file = cfg;

        if let Some(rec) = process::load_runtime_record(&self.home_dir) {
            if process::is_pid_alive(rec.pid) {
                self.recorded_pid = Some(rec.pid);
                if self.known_kernel_path.is_none() {
                    self.known_kernel_path = Some(rec.kernel_path);
                }
            } else {
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

    fn handle_command(&mut self, cmd: WorkerCommand) {
        let result: Result<(), String> = match cmd {
            WorkerCommand::SetMode(mode) => self.set_mode(&mode),
            WorkerCommand::ToggleSystemProxy => self.toggle_system_proxy(),
            WorkerCommand::ToggleTun => self.toggle_tun(),
            WorkerCommand::SelectProxy { group, member } => {
                self.with_client(|c| c.select_proxy(&group, &member))
            }
            WorkerCommand::UnfixGroup(group) => self.with_client(|c| c.unfix_group(&group)),
            WorkerCommand::TestGroupLatency(group) => {
                self.with_client(|c| c.test_group_delay(&group))
            }
            WorkerCommand::RefreshSubscriptions => self.refresh_subscriptions(),
            WorkerCommand::SetSubscriptionUrl(url) => self.set_subscription_url(&url),
            WorkerCommand::RestartCore => self.force_restart_core(),
            WorkerCommand::ToggleAutostart => {
                let enabled = platform::autostart_is_enabled();
                platform::autostart_set(!enabled)
            }
            WorkerCommand::Quit => return,
        };

        match result {
            Ok(()) => self.set_action_error(None),
            Err(err) => self.set_action_error(Some(err)),
        }
        self.refresh(true);
    }

    fn set_mode(&mut self, mode: &str) -> Result<(), String> {
        self.with_client(|c| c.set_mode(mode))?;
        self.persisted_state.mode = Some(mode.to_string());
        self.save_persisted_state();
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
            self.persisted_state.system_proxy = false;
            self.save_persisted_state();
            return Ok(());
        }
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())?;
        let cfg = client.get_configs()?;
        if cfg.mixed_port == 0 {
            return Err(
                "mihomo mixed-port/port is 0 or not configured; cannot enable system proxy."
                    .to_string(),
            );
        }
        platform::proxy_enable(cfg.mixed_port, &self.settings.proxy.bypass)?;
        self.persisted_state.system_proxy = true;
        self.save_persisted_state();
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
        self.persisted_state.tun = Some(want_enable);
        self.save_persisted_state();
        Ok(())
    }

    fn apply_tun_state(&mut self, want_enable: bool) -> Result<bool, String> {
        let client = self
            .client
            .clone()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())?;
        client.set_tun(want_enable)?;
        let after = client.get_configs()?;
        if !want_enable || after.tun_enable {
            return Ok(after.tun_enable);
        }

        // TUN failed to activate -> elevate running kernel via hidden UAC self-helper.
        let running = process::list_running_mihomo(self.known_kernel_path.as_deref());
        let target = process::pick_target_process(
            &running,
            self.active_pid(),
            self.known_kernel_path.as_deref(),
        )
        .ok_or_else(|| text::ERR_TUN_ELEVATION_DECLINED.to_string())?;

        let launch_args = discover::build_launch_args(
            &self.portable_root,
            &self.settings,
            target.exe.as_deref().or(self.known_kernel_path.as_deref()),
        )?;

        self.set_status_note(Some(text::STATUS_ELEVATING_CORE));
        let mut helper_args = vec![
            platform::ELEVATE_START_FLAG.to_string(),
            target.pid.to_string(),
        ];
        helper_args.extend(launch_args);

        let code = platform::run_elevated_self(&helper_args);
        self.set_status_note(None);

        let new_pid = match code {
            Ok(c) if c > 0 => c as u32,
            Ok(_) | Err(_) => return Err(text::ERR_TUN_ELEVATION_DECLINED.to_string()),
        };

        self.managed = None;
        self.recorded_pid = Some(new_pid);
        if let Some(kernel) = target
            .exe
            .clone()
            .or_else(|| self.known_kernel_path.clone())
        {
            process::save_runtime_record(
                &self.home_dir,
                &RuntimeRecord {
                    pid: new_pid,
                    kernel_path: kernel,
                    elevated: true,
                },
            );
        }

        self.wait_for_controller(KERNEL_RESTART_BUDGET, text::ERR_CORE_RESTART_TIMEOUT)?;
        let verify = self.with_client_read_configs()?;
        if !verify.tun_enable {
            return Err(text::ERR_TUN_FAILED_AFTER_ELEVATION.to_string());
        }
        Ok(true)
    }

    fn with_client_read_configs(&self) -> Result<crate::core::api::CoreConfigs, String> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())?;
        client.get_configs()
    }

    fn start_core_internal(&mut self) -> Result<(), String> {
        self.reload_paths_and_runtime();
        let running = process::list_running_mihomo(self.known_kernel_path.as_deref());
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

        let kernel = discover::find_kernel(&self.portable_root, &self.settings)?
            .ok_or_else(discover::kernel_missing_error)?;
        let kernel_path = kernel.path().to_path_buf();
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
        let elevated = child.elevated;
        self.managed = Some(child);
        self.recorded_pid = Some(pid);
        self.cached_version.clear();
        process::save_runtime_record(
            &self.home_dir,
            &RuntimeRecord {
                pid,
                kernel_path,
                elevated,
            },
        );

        let running_after = process::list_running_mihomo(self.known_kernel_path.as_deref());
        self.re_resolve_controller(&running_after);
        let res = self.wait_for_controller(KERNEL_STARTUP_BUDGET, text::ERR_CORE_NOT_RESPONDING);
        self.set_status_note(None);
        res
    }

    fn stop_core_internal(&mut self, disable_proxy: bool) -> Result<(), String> {
        self.set_status_note(Some(text::STATUS_STOPPING_CORE));
        let res = self.stop_active_kernel(true);
        self.set_status_note(None);
        res?;
        if disable_proxy && platform::proxy_is_enabled() {
            let _ = platform::proxy_disable();
        }
        Ok(())
    }

    fn force_restart_core(&mut self) -> Result<(), String> {
        let saved = self.capture_live_or_persisted_state();
        self.pending_restore = Some(saved);
        self.reload_paths_and_runtime();
        let running = process::list_running_mihomo(self.known_kernel_path.as_deref());
        if !running.is_empty() {
            self.set_status_note(Some(text::STATUS_RESTARTING_CORE));
            let stop_res = self.stop_active_kernel(true);
            self.set_status_note(None);
            stop_res?;
        }
        self.start_core_internal()
    }

    fn stop_active_kernel(&mut self, prompt_foreign: bool) -> Result<(), String> {
        // First try managed non-elevated child directly.
        if let Some(mut managed) = self.managed.take()
            && let Some(ref mut child) = managed.child
        {
            if matches!(child.try_wait(), Ok(None)) {
                if child.kill().is_ok() {
                    let _ = child.wait();
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

        let running = process::list_running_mihomo(self.known_kernel_path.as_deref());
        let target = process::pick_target_process(
            &running,
            self.active_pid(),
            self.known_kernel_path.as_deref(),
        )
        .cloned()
        .or_else(|| {
            if prompt_foreign && running.len() == 1 {
                let candidate = &running[0];
                let exe_label = candidate
                    .exe
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<elevated or unreadable path>".to_string());
                let msg = format!(
                    "Stop running mihomo process?\n\nPID: {}\nExecutable: {}",
                    candidate.pid, exe_label
                );
                if platform::confirm_yes_no("Confirm Stop Core", &msg) {
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
            if let Some(ref mut managed) = self.managed
                && let Some(ref mut child) = managed.child
                && let Ok(Some(_)) = child.try_wait()
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

    fn refresh_subscriptions(&mut self) -> Result<(), String> {
        let client = self
            .client
            .clone()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())?;
        self.set_status_note(Some(text::STATUS_REFRESHING_SUBSCRIPTION));
        let res = (|| {
            let providers = client.get_providers()?.providers;
            let refreshable: Vec<String> = providers
                .into_iter()
                .filter(Provider::refreshable)
                .map(|p| p.name)
                .collect();
            if refreshable.is_empty() {
                return Err(
                    "No subscription provider configured. Use Set Subscription URL first."
                        .to_string(),
                );
            }
            for name in refreshable {
                client.refresh_provider(&name)?;
            }
            Ok(())
        })();
        self.set_status_note(None);
        res
    }

    fn set_subscription_url(&mut self, url: &str) -> Result<(), String> {
        self.set_status_note(Some("Updating subscription..."));
        let res = (|| {
            let saved = self.capture_live_or_persisted_state();
            self.pending_restore = Some(saved);
            mihomo::apply_subscription_url(&self.config_file, url)?;
            if let Some(client) = &self.client
                && client.probe().is_ok()
            {
                let sub_cache = self.home_dir.join("providers").join("subscription.yaml");
                let had_cached_sub = sub_cache.is_file();
                client.reload_configs()?;
                if had_cached_sub || !sub_cache.is_file() {
                    client.refresh_provider("subscription")?;
                }
                return Ok(());
            }
            self.start_core_internal()
        })();
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
            PersistedState {
                system_proxy: self.persisted_state.system_proxy || platform::proxy_is_enabled(),
                mode: self.persisted_state.mode.clone(),
                tun: self.persisted_state.tun,
            }
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
                Ok(actual) => {
                    if let Some(client) = &self.client
                        && let Ok(updated) = client.get_configs()
                    {
                        *cfg = updated;
                    } else {
                        cfg.tun_enable = actual;
                    }
                }
                Err(err) => {
                    self.persisted_state.tun = Some(false);
                    self.save_persisted_state();
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

        if restore.system_proxy
            && cfg.mixed_port > 0
            && let Err(err) = platform::proxy_enable(cfg.mixed_port, &self.settings.proxy.bypass)
        {
            self.set_action_error(Some(err));
        }
    }

    fn handle_quit(&mut self) {
        let final_state = self.capture_live_or_persisted_state();
        self.persisted_state = final_state;
        self.save_persisted_state();
        let _ = self.stop_core_internal(true);
        let _ = self.proxy.send_event(AppEvent::QuitCompleted);
    }

    fn active_pid(&mut self) -> Option<u32> {
        if let Some(ref mut managed) = self.managed {
            if let Some(ref mut child) = managed.child
                && matches!(child.try_wait(), Ok(None))
            {
                return Some(managed.pid);
            }
            self.managed = None;
        }
        if let Some(pid) = self.recorded_pid {
            if process::is_pid_alive(pid) {
                return Some(pid);
            }
            self.recorded_pid = None;
        }
        None
    }

    fn refresh(&mut self, include_groups: bool) {
        let mut running = process::list_running_mihomo(self.known_kernel_path.as_deref());
        let kernel_running = self.active_pid().is_some() || !running.is_empty();

        if !kernel_running {
            if let Ok(reloaded) = tray::load(&self.settings_path) {
                self.settings = reloaded;
            }
            if let Ok(Some(k)) = discover::find_kernel(&self.portable_root, &self.settings) {
                self.known_kernel_path = Some(k.path().to_path_buf());
            }
            let (home, cfg) = discover::resolve_home_and_config(
                &self.portable_root,
                &self.settings,
                self.known_kernel_path.as_deref(),
            );
            self.home_dir = home;
            self.config_file = cfg;
            running = process::list_running_mihomo(self.known_kernel_path.as_deref());
            self.re_resolve_controller(&running);
        } else if self.client.is_none() {
            self.re_resolve_controller(&running);
        }

        let client_opt = self.client.clone();
        let mut controller_ok = false;
        let mut mode = String::new();
        let mut mixed_port = 0u16;
        let mut tun = false;
        let mut controller_error = None;
        let mut web_ui_url = None;

        if let Some(client) = &client_opt {
            web_ui_url = Some(client.format_web_ui_url(&self.settings.ui.web_url));
            match client.get_configs() {
                Ok(mut cfg) => {
                    self.apply_pending_restore_if_needed(&mut cfg);
                    controller_ok = true;
                    mode = cfg.mode;
                    mixed_port = cfg.mixed_port;
                    tun = cfg.tun_enable;
                    if self.cached_version.is_empty()
                        && let Ok(ver) = client.get_version()
                    {
                        self.cached_version = ver;
                    }
                }
                Err(err) => {
                    controller_error = Some(err);
                }
            }
        } else {
            controller_error = Some(text::ERR_NO_CONTROLLER.to_string());
        }

        let sysproxy = platform::proxy_is_enabled();
        let autostart = platform::autostart_is_enabled();

        if controller_ok && self.pending_restore.is_none() {
            let live = PersistedState {
                system_proxy: sysproxy,
                mode: (!mode.is_empty()).then(|| mode.clone()),
                tun: Some(tun),
            };
            if !self.has_persisted_state || live != self.persisted_state {
                self.persisted_state = live;
                self.save_persisted_state();
            }
        }

        // Phase 1 snapshot: immediate state + controls update.
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
            snap.controller_error = controller_error;
            snap.web_ui_url = web_ui_url;
            snap.resolved_config = Some(self.config_file.clone());
            snap.page_size = self.settings.groups.page_size;
            if !controller_ok {
                snap.groups.clear();
            }
        }
        let _ = self
            .proxy
            .send_event(AppEvent::SnapshotUpdated { rebuild_menu: true });

        if !include_groups || !controller_ok {
            return;
        }

        // Phase 2 snapshot: fetch proxy groups & providers.
        if let Some(client) = &client_opt {
            let proxies_res = client.get_proxies();
            let providers_res = client.get_providers();

            let mut snap = self.shared.lock().unwrap();
            if let Ok(proxies) = proxies_res {
                snap.groups = order_and_filter_groups(proxies.groups, &self.settings);
                let mut latencies = proxies.latency;
                if let Ok(prov) = providers_res {
                    snap.providers = prov.providers;
                    latencies.extend(prov.latency);
                }
                snap.node_latency = latencies;
            } else if let Ok(prov) = providers_res {
                snap.providers = prov.providers;
            }
            drop(snap);
            let _ = self
                .proxy
                .send_event(AppEvent::SnapshotUpdated { rebuild_menu: true });
        }
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

    fn set_action_error(&self, err: Option<String>) {
        let mut snap = self.shared.lock().unwrap();
        snap.action_error = err;
    }

    fn set_controller_error(&self, err: Option<String>) {
        let mut snap = self.shared.lock().unwrap();
        snap.controller_error = err;
    }
}

fn order_and_filter_groups(groups: Vec<Group>, settings: &TraySettings) -> Vec<Group> {
    let include = &settings.groups.include;
    let exclude = &settings.groups.exclude;
    let order = &settings.groups.order;

    let mut filtered: Vec<Group> = groups
        .into_iter()
        .filter(|g| {
            if !include.is_empty() && !include.iter().any(|x| x.eq_ignore_ascii_case(&g.name)) {
                return false;
            }
            if exclude.iter().any(|x| x.eq_ignore_ascii_case(&g.name)) {
                return false;
            }
            true
        })
        .collect();

    filtered.sort_by(|a, b| {
        if a.name == "GLOBAL" && b.name != "GLOBAL" {
            return std::cmp::Ordering::Less;
        }
        if b.name == "GLOBAL" && a.name != "GLOBAL" {
            return std::cmp::Ordering::Greater;
        }
        let pos_a = order.iter().position(|x| x.eq_ignore_ascii_case(&a.name));
        let pos_b = order.iter().position(|x| x.eq_ignore_ascii_case(&b.name));
        match (pos_a, pos_b) {
            (Some(ia), Some(ib)) => ia.cmp(&ib),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a
                .name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase()),
        }
    });
    filtered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(name: &str) -> Group {
        Group {
            name: name.to_string(),
            kind: "Selector".to_string(),
            switchable: true,
            ..Default::default()
        }
    }

    #[test]
    fn keeps_global_first_and_applies_order_and_filters() {
        let mut settings = TraySettings::default();
        settings.groups.order = vec!["Streaming".to_string(), "Proxy".to_string()];
        settings.groups.exclude = vec!["HiddenOne".to_string()];

        let input = vec![
            g("Alpha"),
            g("Proxy"),
            g("HiddenOne"),
            g("GLOBAL"),
            g("Streaming"),
        ];
        let out: Vec<String> = order_and_filter_groups(input, &settings)
            .into_iter()
            .map(|x| x.name)
            .collect();
        assert_eq!(out, vec!["GLOBAL", "Streaming", "Proxy", "Alpha"]);
    }
}
