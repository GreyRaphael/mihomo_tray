//! Background worker thread for mihomo lifecycle, polling, and controller commands.

mod config_tracker;
mod lifecycle;
mod state_sync;
mod tasks;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use winit::event_loop::EventLoopProxy;

use self::config_tracker::ConfigTracker;
use self::lifecycle::KernelRuntime;
use self::state_sync::StateSync;
use self::tasks::{SubscriptionTaskState, TaskSlot};
use crate::config::mihomo;
use crate::config::state_store::PersistedState;
use crate::config::tray::{GroupsSettings, TraySettings};
use crate::core::api::{Client, CoreConfigs, ProvidersData, ProxiesData};
use crate::core::process::RunningProcess;
use crate::platform;
use crate::state::{self, AppEvent, ProxyMode, SharedSnapshot, Snapshot, WorkerCommand};
use crate::text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActionErrorKind {
    TransientWait,
    GuiOpen,
    General,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ActionError {
    pub kind: ActionErrorKind,
    pub message: String,
}

impl ActionError {
    pub fn general(message: impl Into<String>) -> Self {
        Self {
            kind: ActionErrorKind::General,
            message: message.into(),
        }
    }

    pub fn transient_wait(message: impl Into<String>) -> Self {
        Self {
            kind: ActionErrorKind::TransientWait,
            message: message.into(),
        }
    }

    pub fn gui_open(message: impl Into<String>) -> Self {
        Self {
            kind: ActionErrorKind::GuiOpen,
            message: message.into(),
        }
    }
}

impl From<String> for ActionError {
    fn from(message: String) -> Self {
        Self::general(message)
    }
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandOutcome {
    ClearError,
    ClearKind(ActionErrorKind),
    KeepError,
}

#[derive(Default)]
struct LiveState {
    controller_ok: bool,
    mode: String,
    mixed_port: u16,
    tun: bool,
    sysproxy: bool,
}

#[derive(Default)]
struct PolledConfig {
    controller_ok: bool,
    mode: String,
    mixed_port: u16,
    tun: bool,
    controller_error: Option<String>,
    web_ui_url: Option<String>,
}

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
            worker.run(&rx);
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
    kernel: KernelRuntime,
    live: LiveState,
    shared: SharedSnapshot,
    proxy: UiNotifier,
    stop_requested: Arc<AtomicBool>,
    tx: Sender<WorkerCommand>,
    latency_tasks: TaskSlot,
    sub_tasks: SubscriptionTaskState,
    client: Option<Client>,
    last_controller_error: Option<String>,
    action_error: Option<ActionError>,
    cached_version: String,
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
            snap.page_size = cfg.settings().groups.page_size;
            snap.resolved_config = Some(cfg.config_file().to_path_buf());
            snap.config_error = cfg.current_config_error();
        }
        let worker = Self {
            cfg,
            state_sync: StateSync::default(),
            kernel: KernelRuntime::new(),
            live: LiveState::default(),
            shared,
            proxy,
            stop_requested,
            tx,
            latency_tasks: TaskSlot::default(),
            sub_tasks: SubscriptionTaskState::default(),
            client: None,
            last_controller_error: None,
            action_error: None,
            cached_version: String::new(),
        };
        (worker, rx)
    }

    fn run(&mut self, rx: &mpsc::Receiver<WorkerCommand>) {
        self.startup();

        'worker: loop {
            let interval = Duration::from_millis(self.cfg.settings().ui.poll_interval_ms as u64);
            match rx.recv_timeout(interval) {
                Ok(first) => {
                    let mut outcome = CommandOutcome::KeepError;
                    let mut burst_error: Option<ActionError> = None;
                    let mut quit_cancelled = false;
                    let mut next = Some(first);

                    // Coalesce burst commands before doing a single refresh poll.
                    while let Some(cmd) = next.take().or_else(|| rx.try_recv().ok()) {
                        if let WorkerCommand::Quit { interactive } = cmd {
                            if self.handle_quit(interactive) {
                                break 'worker;
                            }
                            quit_cancelled = true;
                            break;
                        }
                        match self.handle_command(cmd) {
                            Ok(CommandOutcome::ClearError) => {
                                burst_error = None;
                                outcome = CommandOutcome::ClearError;
                            }
                            Ok(CommandOutcome::ClearKind(kind)) => {
                                if burst_error.as_ref().is_some_and(|e| e.kind == kind) {
                                    burst_error = None;
                                }
                                if self.action_error.as_ref().is_some_and(|e| e.kind == kind) {
                                    outcome = CommandOutcome::ClearError;
                                }
                            }
                            Ok(CommandOutcome::KeepError) => {}
                            Err(err) => burst_error = Some(err),
                        }
                    }

                    if !quit_cancelled {
                        if let Some(err) = burst_error {
                            self.set_action_error(Some(err));
                        } else if outcome == CommandOutcome::ClearError {
                            self.set_action_error(None);
                        }
                    }
                    self.refresh();
                }
                Err(RecvTimeoutError::Timeout) => self.refresh(),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    fn startup(&mut self) {
        self.reload_paths_and_runtime();
        self.state_sync.load_initial(self.cfg.home_dir());
        self.cfg.refresh_autostart_cache();
        let running = self.kernel.list_running(self.cfg.known_kernel_path());
        self.startup_with_running(&running);
        if !self.stop_requested.load(Ordering::Acquire) {
            self.refresh();
        }
    }

    fn startup_with_running(&mut self, running: &[RunningProcess]) {
        self.re_resolve_controller(running);

        let controller_alive = self.client.as_ref().is_some_and(|c| c.probe().is_ok());

        if controller_alive || !running.is_empty() {
            // Core is already running; preserve its live mode/TUN instead of overriding from disk.
            self.state_sync.clear_pending();
        } else if self.cfg.settings().mihomo.auto_start
            && let Err(err) = self.start_core_internal()
        {
            tracing::warn!("Auto-start core failed: {err}");
            if self.cfg.current_config_error().as_deref() != Some(err.message.as_str()) {
                self.set_action_error(Some(err));
            }
        }
    }

    fn reload_paths_and_runtime(&mut self) {
        self.cfg.reload_settings_and_paths();
        self.sync_config_error();

        if let Some(kernel_path) = self.kernel.sync_runtime_record(self.cfg.home_dir()) {
            self.cfg.set_known_kernel_path_if_none(kernel_path);
        }
    }

    pub(super) fn invalidate_async_tasks(&mut self) {
        self.sub_tasks.invalidate();
        self.latency_tasks.invalidate();
    }

    fn handle_command(&mut self, cmd: WorkerCommand) -> Result<CommandOutcome, ActionError> {
        match cmd {
            WorkerCommand::SetMode(mode) => {
                self.set_mode(mode).map(|()| CommandOutcome::ClearError)
            }
            WorkerCommand::ToggleSystemProxy => self
                .toggle_system_proxy()
                .map(|()| CommandOutcome::ClearError),
            WorkerCommand::ToggleTun => self.toggle_tun().map(|()| CommandOutcome::ClearError),
            WorkerCommand::SelectProxy { group, member } => Ok(self
                .require_client()?
                .select_proxy(&group, &member)
                .map(|()| CommandOutcome::ClearError)?),
            WorkerCommand::UnfixGroup(group) => Ok(self
                .require_client()?
                .unfix_group(&group)
                .map(|()| CommandOutcome::ClearError)?),
            WorkerCommand::TestGroupLatency(group) => self.start_test_group_latency(group),
            WorkerCommand::RefreshSubscriptions => self.start_refresh_subscriptions(),
            WorkerCommand::SetSubscriptionUrl(url) => self
                .set_subscription_url(&url)
                .map(|()| CommandOutcome::ClearError),
            WorkerCommand::CopyEnvVars => self.copy_env_vars().map(|()| CommandOutcome::ClearError),
            WorkerCommand::RestartCore => self
                .force_restart_core()
                .map(|()| CommandOutcome::ClearError),
            WorkerCommand::ToggleAutostart => {
                let enabled = platform::autostart_is_enabled();
                let res = platform::autostart_set(!enabled);
                self.cfg.refresh_autostart_cache();
                Ok(res.map(|()| CommandOutcome::ClearError)?)
            }
            WorkerCommand::GuiOpenResult(Err(err)) => Err(ActionError::gui_open(err)),
            WorkerCommand::GuiOpenResult(Ok(())) => {
                Ok(CommandOutcome::ClearKind(ActionErrorKind::GuiOpen))
            }
            WorkerCommand::AsyncDone { kind, error } => self.handle_async_done(kind, error),
            WorkerCommand::Quit { .. } => unreachable!("Quit is handled directly in Worker::run"),
        }
    }

    fn require_client(&self) -> Result<&Client, String> {
        self.client
            .as_ref()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())
    }

    fn set_mode(&mut self, mode: ProxyMode) -> Result<(), ActionError> {
        let mode_str = mode.as_api_str();
        if mode_str.is_empty() {
            return Err(ActionError::general(text::ERR_UNSUPPORTED_PROXY_MODE));
        }
        self.require_client()?.set_mode(mode_str)?;
        self.state_sync
            .update(self.cfg.home_dir(), |s| s.mode = Some(mode_str.to_string()));
        self.live.mode = mode_str.to_string();
        Ok(())
    }

    fn toggle_system_proxy(&mut self) -> Result<(), ActionError> {
        if platform::proxy_is_enabled() {
            platform::proxy_disable()?;
            self.state_sync
                .update(self.cfg.home_dir(), |s| s.system_proxy = false);
            self.live.sysproxy = false;
            return Ok(());
        }
        let cfg = self.require_client()?.get_configs()?;
        if cfg.mixed_port == 0 {
            return Err(ActionError::general(text::ERR_NO_MIXED_PORT_FOR_PROXY));
        }
        platform::proxy_enable(cfg.mixed_port, &self.cfg.settings().proxy.bypass)?;
        self.state_sync
            .update(self.cfg.home_dir(), |s| s.system_proxy = true);
        self.live.sysproxy = true;
        Ok(())
    }

    fn toggle_tun(&mut self) -> Result<(), ActionError> {
        let current = self.require_client()?.get_configs()?;
        let want_enable = !current.tun_enable;
        self.apply_tun_state(want_enable)?;
        self.state_sync
            .update(self.cfg.home_dir(), |s| s.tun = Some(want_enable));
        self.live.tun = want_enable;
        Ok(())
    }

    fn copy_env_vars(&self) -> Result<(), ActionError> {
        let port = if self.live.mixed_port > 0 {
            self.live.mixed_port
        } else {
            mihomo::read_file(self.cfg.config_file())
                .map(|info| info.mixed_port)
                .unwrap_or(0)
        };
        if port == 0 {
            return Err(ActionError::general(text::ERR_NO_MIXED_PORT_FOR_ENV));
        }
        let cmd = platform::format_env_vars_command(port);
        platform::copy_to_clipboard(&cmd)?;
        tracing::info!("Copied proxy environment variables for port {port}");
        Ok(())
    }

    fn capture_live_or_persisted_state(&self) -> PersistedState {
        self.state_sync.capture(
            self.live.controller_ok,
            &self.live.mode,
            self.live.tun,
            self.live.sysproxy,
        )
    }

    fn apply_pending_restore_if_needed(&mut self, cfg: &mut CoreConfigs) {
        let Some(mut restore) = self.state_sync.take_ready_restore() else {
            return;
        };
        if self.stop_requested.load(Ordering::Acquire) {
            self.state_sync.arm_ready_restore(restore);
            return;
        }

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
                    restore.tun = Some(cfg.tun_enable);
                }
                Err(err) => {
                    if self.stop_requested.load(Ordering::Acquire) {
                        self.state_sync.arm_ready_restore(restore);
                        return;
                    }
                    self.state_sync
                        .update(self.cfg.home_dir(), |s| s.tun = Some(cfg.tun_enable));
                    self.set_action_error(Some(err));
                }
            }
        }

        if self.stop_requested.load(Ordering::Acquire) {
            self.state_sync.arm_ready_restore(restore);
            return;
        }

        if let Some(ref mode) = restore.mode
            && !mode.is_empty()
            && !cfg.mode.eq_ignore_ascii_case(mode)
            && let Some(client) = &self.client
        {
            match client.set_mode(mode) {
                Ok(()) => cfg.mode = mode.clone(),
                Err(err) => self.set_action_error(Some(ActionError::general(err))),
            }
        }

        if restore.system_proxy {
            if cfg.mixed_port == 0 {
                self.set_action_error(Some(ActionError::general(
                    text::ERR_NO_MIXED_PORT_FOR_RESTORE,
                )));
            } else if let Err(err) =
                platform::proxy_enable(cfg.mixed_port, &self.cfg.settings().proxy.bypass)
            {
                self.set_action_error(Some(ActionError::general(err)));
            }
        }
    }

    fn handle_quit(&mut self, interactive: bool) -> bool {
        let final_state = self.capture_live_or_persisted_state();
        self.state_sync
            .update(self.cfg.home_dir(), |s| *s = final_state);
        match self.stop_core_internal(interactive) {
            Ok(()) => {
                self.proxy.send(AppEvent::QuitCompleted);
                true
            }
            Err(err) => {
                self.stop_requested.store(false, Ordering::Release);
                self.set_action_error(Some(ActionError::general(err)));
                self.proxy.send(AppEvent::QuitCancelled);
                false
            }
        }
    }

    fn resolve_live_controller(&mut self) -> bool {
        let tracked_alive = self
            .kernel
            .active_pid(self.cfg.known_kernel_path(), self.cfg.home_dir())
            .is_some();
        let changes = self.cfg.poll_changes(tracked_alive);
        if changes.error_changed {
            self.sync_config_error();
        }
        if changes.settings_changed {
            self.set_action_error(None);
        }

        let might_resolve = changes.controller_changed
            || changes.config_file_changed
            || self.client.is_none()
            || !self.live.controller_ok;

        let running = if !tracked_alive || might_resolve {
            self.kernel.list_running(self.cfg.known_kernel_path())
        } else {
            Vec::new()
        };
        let kernel_running = tracked_alive || !running.is_empty();

        let need_resolve = changes.controller_changed
            || (changes.settings_changed && !kernel_running)
            || changes.config_file_changed
            || (kernel_running && (self.client.is_none() || !self.live.controller_ok));
        if need_resolve {
            self.re_resolve_controller(&running);
        }
        kernel_running
    }

    fn poll_controller_configs(&mut self) -> PolledConfig {
        let Some(client) = self.client.clone() else {
            return PolledConfig::default();
        };
        match client.get_configs() {
            Ok(mut cfg) => {
                self.apply_pending_restore_if_needed(&mut cfg);
                let active_client = self.client.as_ref().unwrap_or(&client);
                let web_ui_url =
                    Some(active_client.format_web_ui_url(&self.cfg.settings().ui.web_url));
                if self.cached_version.is_empty()
                    && let Ok(ver) = active_client.get_version()
                {
                    self.cached_version = ver;
                }
                PolledConfig {
                    controller_ok: true,
                    mode: cfg.mode,
                    mixed_port: cfg.mixed_port,
                    tun: cfg.tun_enable,
                    controller_error: None,
                    web_ui_url,
                }
            }
            Err(err) => PolledConfig {
                controller_ok: false,
                controller_error: Some(err),
                ..PolledConfig::default()
            },
        }
    }

    fn refresh(&mut self) {
        let kernel_running = self.resolve_live_controller();
        let polled = self.poll_controller_configs();
        let sysproxy = platform::proxy_is_enabled();
        self.cfg.refresh_autostart_if_stale();

        self.live.controller_ok = polled.controller_ok;
        self.live.mode = polled.mode.clone();
        self.live.mixed_port = polled.mixed_port;
        self.live.tun = polled.tun;
        self.live.sysproxy = sysproxy;

        if polled.controller_ok {
            self.state_sync.sync_live_if_idle(
                self.cfg.home_dir(),
                sysproxy,
                &polled.mode,
                polled.tun,
            );
            if self
                .action_error
                .as_ref()
                .is_some_and(|e| e.kind == ActionErrorKind::TransientWait)
            {
                self.action_error = None;
            }
        }

        let (proxies_res, providers_res) = if polled.controller_ok
            && let Some(client) = &self.client
        {
            (Some(client.get_proxies()), Some(client.get_providers()))
        } else {
            (None, None)
        };

        {
            let mut snap = self.shared.lock().unwrap();
            let controller_ok = polled.controller_ok;
            self.apply_polled_config(&mut snap, polled, sysproxy, kernel_running);
            snap.action_error = self.action_error.as_ref().map(|e| e.message.clone());

            if !controller_ok {
                snap.groups.clear();
                snap.providers.clear();
                snap.node_latency.clear();
            } else {
                apply_proxies_and_providers(
                    &mut snap,
                    &self.cfg.settings().groups,
                    proxies_res,
                    providers_res,
                );
            }
        }
        self.proxy
            .send(AppEvent::SnapshotUpdated { rebuild_menu: true });
    }

    fn apply_polled_config(
        &self,
        snap: &mut Snapshot,
        polled: PolledConfig,
        sysproxy: bool,
        kernel_running: bool,
    ) {
        snap.controller_ok = polled.controller_ok;
        snap.version = if polled.controller_ok {
            self.cached_version.clone()
        } else {
            String::new()
        };
        snap.mode = polled.mode;
        snap.mixed_port = polled.mixed_port;
        snap.tun = polled.tun;
        snap.sysproxy = sysproxy;
        snap.autostart = self.cfg.cached_autostart();
        snap.kernel_running = kernel_running;
        snap.controller_error = if self.client.is_some() {
            (!polled.controller_ok && kernel_running)
                .then_some(polled.controller_error)
                .flatten()
        } else if kernel_running || self.cfg.config_file().is_file() {
            self.last_controller_error.clone()
        } else {
            None
        };
        snap.web_ui_url = polled.web_ui_url;
        snap.resolved_config = Some(self.cfg.config_file().to_path_buf());
        snap.page_size = self.cfg.settings().groups.page_size;
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

    fn set_action_error(&mut self, err: Option<ActionError>) {
        self.action_error = err;
        let msg = self.action_error.as_ref().map(|e| e.message.clone());
        let mut snap = self.shared.lock().unwrap();
        snap.action_error = msg;
    }
}

fn apply_proxies_and_providers(
    snap: &mut Snapshot,
    groups_cfg: &GroupsSettings,
    proxies_res: Option<Result<ProxiesData, String>>,
    providers_res: Option<Result<ProvidersData, String>>,
) {
    let mut latencies = match proxies_res {
        Some(Ok(proxies)) => {
            snap.groups = state::order_and_filter_groups(
                proxies.groups,
                &groups_cfg.include,
                &groups_cfg.exclude,
                &groups_cfg.order,
            );
            Some(proxies.latency)
        }
        Some(Err(err)) => {
            tracing::warn!("Failed to poll /proxies: {err}");
            None
        }
        None => None,
    };

    match providers_res {
        Some(Ok(prov)) => {
            snap.providers = prov.providers;
            match latencies {
                Some(ref mut map) => merge_latency(map, prov.latency, false),
                None => merge_latency(&mut snap.node_latency, prov.latency, true),
            }
        }
        Some(Err(err)) => {
            tracing::warn!("Failed to poll /providers/proxies: {err}");
        }
        None => {}
    }

    if let Some(map) = latencies {
        snap.node_latency = map;
    }
}

fn merge_latency(
    target: &mut BTreeMap<String, Option<u32>>,
    extra: BTreeMap<String, Option<u32>>,
    overwrite_with_measured: bool,
) {
    for (name, delay) in extra {
        target
            .entry(name)
            .and_modify(|existing| {
                if existing.is_none() || (overwrite_with_measured && delay.is_some()) {
                    *existing = delay;
                }
            })
            .or_insert(delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AsyncTaskKind, SharedSnapshot};
    use std::sync::Mutex;

    pub(super) fn read_http_request(stream: &mut std::net::TcpStream) -> String {
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

    pub(super) fn test_worker(
        root: &std::path::Path,
        settings: TraySettings,
        startup_error: Option<String>,
    ) -> (Worker, mpsc::Receiver<WorkerCommand>, SharedSnapshot) {
        let settings_path = crate::config::tray::settings_path(root);
        let shared = Arc::new(Mutex::new(Snapshot::default()));
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

        worker.cfg.reload_settings_and_paths();
        worker.sync_config_error();
        assert!(shared.lock().unwrap().config_error.is_some());

        std::fs::write(
            &settings_path,
            "mihomo:\n  path: ./custom/my-core\n  config: ./alt/config.yaml\n",
        )
        .unwrap();
        // Force mtime check to observe edit deterministically across fast filesystems.
        worker.cfg.invalidate_settings_mtime();
        worker.resolve_live_controller();

        assert_eq!(worker.cfg.known_kernel_path(), Some(exe.as_path()));
        assert_eq!(
            worker.cfg.config_file(),
            root.join("alt").join("config.yaml")
        );
        assert_eq!(shared.lock().unwrap().config_error, None);

        // Runtime syntax error in tray.yml must surface in Snapshot.config_error.
        std::fs::write(&settings_path, "mihomo: [unclosed").unwrap();
        worker.cfg.invalidate_settings_mtime();
        worker.resolve_live_controller();
        assert!(shared.lock().unwrap().config_error.is_some());

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
        worker.startup();
        worker.set_action_error(Some(ActionError::general(text::ERR_CORE_NOT_FOUND)));
        worker.refresh();

        let snap = shared.lock().unwrap();
        assert!(!snap.controller_ok);
        assert_eq!(snap.controller_error, None);
        assert_eq!(snap.web_ui_url, None);
        assert_eq!(snap.error(), Some(text::ERR_CORE_NOT_FOUND));
        drop(snap);

        worker.set_action_error(None);
        worker.refresh();
        assert_eq!(shared.lock().unwrap().error(), None);
        assert_eq!(shared.lock().unwrap().web_ui_url, None);

        // Missing config.yaml while stopped must not surface ERR_NO_CONTROLLER noise.
        std::fs::remove_file(cfg_dir.join("config.yaml")).unwrap();
        worker.re_resolve_controller(&[]);
        worker.refresh();
        assert_eq!(shared.lock().unwrap().controller_error, None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn async_done_success_does_not_clobber_existing_action_error() {
        let root = crate::config::test_temp_dir("worker_async_err");
        let (mut worker, _rx, shared) = test_worker(&root, TraySettings::default(), None);

        worker.set_action_error(Some(ActionError::general("subscription refresh failed")));
        let gen0 = worker.latency_tasks.begin();

        let outcome = worker
            .handle_command(WorkerCommand::AsyncDone {
                kind: AsyncTaskKind::LatencyTest { generation: gen0 },
                error: None,
            })
            .unwrap();
        assert_eq!(outcome, CommandOutcome::KeepError);
        assert!(!worker.latency_tasks.is_active());
        assert_eq!(
            shared.lock().unwrap().action_error.as_deref(),
            Some("subscription refresh failed")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn set_mode_updates_live_state_immediately_so_capture_keeps_latest_mode() {
        use std::io::Write;
        use std::net::TcpListener;

        let root = crate::config::test_temp_dir("worker_mode_truth");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let req = read_http_request(&mut stream);
            assert!(req.starts_with("PATCH /configs "));
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .unwrap();
        });

        let (mut worker, _rx, _shared) = test_worker(&root, TraySettings::default(), None);
        worker.client = Some(Client::new(&format!("127.0.0.1:{port}"), "", 2000).unwrap());
        worker.live.controller_ok = true;
        worker.live.mode = "rule".to_string();

        worker
            .handle_command(WorkerCommand::SetMode(ProxyMode::Global))
            .unwrap();
        assert_eq!(worker.live.mode, "global");
        assert_eq!(
            worker.capture_live_or_persisted_state().mode.as_deref(),
            Some("global")
        );

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shutdown_interruption_preserves_pending_tun_restore_state() {
        let root = crate::config::test_temp_dir("worker_tun_interrupt");
        let (mut worker, _rx, _shared) = test_worker(&root, TraySettings::default(), None);
        let saved = PersistedState {
            system_proxy: true,
            mode: Some("global".to_string()),
            tun: Some(true),
        };
        worker.state_sync.arm_ready_restore(saved);
        worker.stop_requested.store(true, Ordering::Release);

        let mut cfg = CoreConfigs {
            mode: "rule".to_string(),
            mixed_port: 7890,
            tun_enable: false,
        };
        worker.apply_pending_restore_if_needed(&mut cfg);

        assert!(worker.state_sync.has_ready_restore());
        assert_eq!(
            worker.state_sync.persisted().and_then(|s| s.tun),
            Some(true)
        );
        worker.live.controller_ok = true;
        worker.live.tun = false;
        assert_eq!(worker.capture_live_or_persisted_state().tun, Some(true));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn merge_latency_preserves_measured_delays_when_proxies_poll_fails() {
        let mut snap = Snapshot::default();
        snap.node_latency.insert("Node-A".to_string(), Some(25));
        snap.node_latency.insert("Node-B".to_string(), None);
        snap.node_latency.insert("Node-D".to_string(), Some(15));

        let mut prov_latency = BTreeMap::new();
        prov_latency.insert("Node-A".to_string(), None);
        prov_latency.insert("Node-B".to_string(), Some(40));
        prov_latency.insert("Node-C".to_string(), Some(60));
        prov_latency.insert("Node-D".to_string(), Some(55));

        let prov = ProvidersData {
            providers: Vec::new(),
            latency: prov_latency,
        };
        apply_proxies_and_providers(
            &mut snap,
            &GroupsSettings::default(),
            Some(Err("temporary /proxies failure".to_string())),
            Some(Ok(prov)),
        );

        assert_eq!(snap.node_latency.get("Node-A"), Some(&Some(25)));
        assert_eq!(snap.node_latency.get("Node-B"), Some(&Some(40)));
        assert_eq!(snap.node_latency.get("Node-C"), Some(&Some(60)));
        assert_eq!(snap.node_latency.get("Node-D"), Some(&Some(55)));
    }

    #[test]
    fn burst_coalescing_clears_earlier_error_when_later_command_succeeds() {
        use std::io::Write;
        use std::net::TcpListener;

        let root = crate::config::test_temp_dir("worker_burst_clear");
        let cfg_dir = root.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            cfg_dir.join("config.yaml"),
            format!("external-controller: 127.0.0.1:{port}\n"),
        )
        .unwrap();

        let server = std::thread::spawn(move || {
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().unwrap();
                let req = read_http_request(&mut stream);
                let (status, body) = if req.starts_with("GET /version ") {
                    ("200 OK", r#"{"version":"v1.19.0"}"#.to_string())
                } else if req.starts_with("GET /configs ") {
                    (
                        "200 OK",
                        r#"{"mode":"global","mixed-port":7890,"tun":{"enable":false}}"#.to_string(),
                    )
                } else if req.starts_with("PATCH /configs ") {
                    ("204 No Content", String::new())
                } else if req.starts_with("GET /proxies ") {
                    ("200 OK", r#"{"proxies":{}}"#.to_string())
                } else if req.starts_with("GET /providers/proxies ") {
                    ("200 OK", r#"{"providers":{}}"#.to_string())
                } else {
                    ("404 Not Found", String::new())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).unwrap();
            }
        });

        let mut settings = TraySettings::default();
        settings.mihomo.auto_start = false;
        let (mut worker, rx, shared) = test_worker(&root, settings, None);
        worker.client = Some(Client::new(&format!("127.0.0.1:{port}"), "", 2000).unwrap());

        // Queue a failing command followed by a succeeding error-clearing command, then Quit.
        worker
            .tx
            .send(WorkerCommand::SetMode(ProxyMode::Unknown))
            .unwrap();
        worker
            .tx
            .send(WorkerCommand::SetMode(ProxyMode::Global))
            .unwrap();
        worker
            .tx
            .send(WorkerCommand::Quit { interactive: false })
            .unwrap();

        worker.run(&rx);
        assert_eq!(shared.lock().unwrap().action_error, None);

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn transient_wait_error_clears_on_controller_recovery_while_preserving_other_errors() {
        use std::io::Write;
        use std::net::TcpListener;

        let root = crate::config::test_temp_dir("worker_transient_err");
        let cfg_dir = root.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            cfg_dir.join("config.yaml"),
            format!("external-controller: 127.0.0.1:{port}\n"),
        )
        .unwrap();

        let server = std::thread::spawn(move || {
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                let req = read_http_request(&mut stream);
                if req.starts_with("POST /__stop ") {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                    break;
                }
                let (status, body) = if req.starts_with("GET /version ") {
                    ("200 OK", r#"{"version":"v1.19.0"}"#.to_string())
                } else if req.starts_with("GET /configs ") {
                    (
                        "200 OK",
                        r#"{"mode":"rule","mixed-port":7890,"tun":{"enable":false}}"#.to_string(),
                    )
                } else if req.starts_with("GET /proxies ") {
                    ("200 OK", r#"{"proxies":{}}"#.to_string())
                } else if req.starts_with("GET /providers/proxies ") {
                    ("200 OK", r#"{"providers":{}}"#.to_string())
                } else {
                    ("404 Not Found", String::new())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).unwrap();
            }
        });

        let (mut worker, _rx, shared) = test_worker(&root, TraySettings::default(), None);
        worker.client = Some(Client::new(&format!("127.0.0.1:{port}"), "", 2000).unwrap());

        // Transient timeout clears once controller responds.
        worker.set_action_error(Some(ActionError::transient_wait(
            text::ERR_CORE_NOT_RESPONDING,
        )));
        worker.refresh();
        assert!(shared.lock().unwrap().controller_ok);
        assert_eq!(shared.lock().unwrap().action_error, None);

        // Non-transient action error must be preserved across healthy polls.
        worker.set_action_error(Some(ActionError::general(text::ERR_TUN_ELEVATION_DECLINED)));
        worker.refresh();
        assert_eq!(
            shared.lock().unwrap().action_error.as_deref(),
            Some(text::ERR_TUN_ELEVATION_DECLINED)
        );

        let _ = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .unwrap()
            .write_all(b"POST /__stop HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n");
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn gui_open_result_sets_and_clears_only_matching_action_error() {
        let root = crate::config::test_temp_dir("worker_gui_open_err");
        let mut settings = TraySettings::default();
        settings.mihomo.auto_start = false;
        let (mut worker, rx, shared) = test_worker(&root, settings, None);

        let err = worker
            .handle_command(WorkerCommand::GuiOpenResult(Err("open failed".to_string())))
            .unwrap_err();
        worker.set_action_error(Some(err));
        assert_eq!(
            shared.lock().unwrap().action_error.as_deref(),
            Some("open failed")
        );

        // If an unrelated error replaced it, GuiOpenResult(Ok) must not clear the unrelated error.
        worker.set_action_error(Some(ActionError::general("other error")));
        assert_eq!(
            worker
                .handle_command(WorkerCommand::GuiOpenResult(Ok(())))
                .unwrap(),
            CommandOutcome::ClearKind(ActionErrorKind::GuiOpen)
        );
        assert_eq!(
            shared.lock().unwrap().action_error.as_deref(),
            Some("other error")
        );

        // Same-burst GuiOpenResult(Err) followed by GuiOpenResult(Ok) must cancel itself out
        // without clobbering the preexisting unrelated "other error".
        worker
            .tx
            .send(WorkerCommand::GuiOpenResult(Err(
                "open failed 2".to_string()
            )))
            .unwrap();
        worker
            .tx
            .send(WorkerCommand::GuiOpenResult(Ok(())))
            .unwrap();
        worker
            .tx
            .send(WorkerCommand::Quit { interactive: false })
            .unwrap();
        worker.run(&rx);
        assert_eq!(
            shared.lock().unwrap().action_error.as_deref(),
            Some("other error")
        );

        // When no unrelated error is active, same-burst Err -> Ok ends cleanly with None.
        worker.set_action_error(None);
        worker
            .tx
            .send(WorkerCommand::GuiOpenResult(Err(
                "open failed 3".to_string()
            )))
            .unwrap();
        worker
            .tx
            .send(WorkerCommand::GuiOpenResult(Ok(())))
            .unwrap();
        worker
            .tx
            .send(WorkerCommand::Quit { interactive: false })
            .unwrap();
        worker.run(&rx);
        assert_eq!(shared.lock().unwrap().action_error, None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn startup_clears_pending_restore_when_existing_process_is_not_yet_probing() {
        let root = crate::config::test_temp_dir("worker_startup_existing_proc");
        let cfg_dir = root.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        crate::config::state_store::save(
            &cfg_dir,
            &PersistedState {
                system_proxy: true,
                mode: Some("global".to_string()),
                tun: Some(true),
            },
        );

        let mut settings = TraySettings::default();
        settings.mihomo.auto_start = false;
        let (mut worker, _rx, _shared) = test_worker(&root, settings, None);
        worker.state_sync.load_initial(&cfg_dir);
        assert!(worker.state_sync.has_ready_restore());

        // Existing external process whose controller isn't reachable yet must clear pending restore.
        let running = [RunningProcess {
            pid: 999_991,
            exe: None,
            cmd: Vec::new(),
        }];
        worker.startup_with_running(&running);
        assert!(!worker.state_sync.has_ready_restore());

        let _ = std::fs::remove_dir_all(&root);
    }
}
