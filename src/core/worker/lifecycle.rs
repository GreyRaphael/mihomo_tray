//! Kernel process lifecycle, controller probing, and TUN UAC elevation for [`super::Worker`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{ActionError, RuntimeConfig};
use crate::config::mihomo;
use crate::config::tray::TraySettings;
use crate::core::api::{Client, CoreConfigs};
use crate::core::discover;
use crate::core::process::{self, ManagedChild, ProcessInspector, RunningProcess, RuntimeRecord};
use crate::platform;
use crate::text;

const KERNEL_STARTUP_BUDGET: Duration = Duration::from_secs(6);
const KERNEL_RESTART_BUDGET: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StopOutcome {
    Stopped,
    DeclinedByUser,
    AmbiguousExternal,
}

struct KernelRuntime {
    inspector: ProcessInspector,
    managed: Option<ManagedChild>,
    recorded_pid: Option<u32>,
}

impl KernelRuntime {
    fn new() -> Self {
        Self {
            inspector: ProcessInspector::new(),
            managed: None,
            recorded_pid: None,
        }
    }

    fn has_managed(&self) -> bool {
        self.managed.is_some()
    }

    fn list_running(&mut self, expected_kernel: Option<&Path>) -> Vec<RunningProcess> {
        self.inspector.list_running_mihomo(expected_kernel)
    }

    fn active_pid(&mut self, expected_kernel: Option<&Path>, home_dir: &Path) -> Option<u32> {
        if let Some(ref mut managed) = self.managed {
            if matches!(managed.child.try_wait(), Ok(None)) {
                return Some(managed.pid());
            }
            self.managed = None;
        }
        if let Some(pid) = self.recorded_pid {
            if self
                .inspector
                .is_recorded_kernel_alive(pid, expected_kernel)
            {
                return Some(pid);
            }
            self.clear_tracking(home_dir);
        }
        None
    }

    fn sync_runtime_record(&mut self, home_dir: &Path) -> Option<PathBuf> {
        let rec = process::load_runtime_record(home_dir)?;
        if self
            .inspector
            .is_recorded_kernel_alive(rec.pid, Some(&rec.kernel_path))
        {
            self.recorded_pid = Some(rec.pid);
            Some(rec.kernel_path)
        } else {
            self.clear_tracking(home_dir);
            None
        }
    }

    fn record_managed(&mut self, child: ManagedChild, home_dir: &Path, kernel_path: &Path) -> u32 {
        let pid = child.pid();
        self.managed = Some(child);
        self.recorded_pid = Some(pid);
        process::save_runtime_record(
            home_dir,
            &RuntimeRecord {
                pid,
                kernel_path: kernel_path.to_path_buf(),
            },
        );
        pid
    }

    fn record_elevated(&mut self, pid: u32, home_dir: &Path, kernel_path: Option<&Path>) {
        self.managed = None;
        self.recorded_pid = Some(pid);
        if let Some(kernel) = kernel_path {
            process::save_runtime_record(
                home_dir,
                &RuntimeRecord {
                    pid,
                    kernel_path: kernel.to_path_buf(),
                },
            );
        } else {
            process::clear_runtime_record(home_dir);
        }
    }

    fn clear_tracking(&mut self, home_dir: &Path) {
        self.recorded_pid = None;
        process::clear_runtime_record(home_dir);
    }

    fn clear_dead(&mut self, home_dir: &Path) {
        self.managed = None;
        self.clear_tracking(home_dir);
    }

    fn kill_managed_if_running(&mut self, home_dir: &Path) -> bool {
        let Some(ref mut managed) = self.managed else {
            return false;
        };
        if matches!(managed.child.try_wait(), Ok(None)) {
            if managed.child.kill().is_ok() {
                let _ = managed.child.wait();
                self.clear_dead(home_dir);
                return true;
            }
            false
        } else {
            self.managed = None;
            false
        }
    }

    fn try_wait_managed_exited(&mut self, home_dir: &Path) -> bool {
        if let Some(ref mut managed) = self.managed
            && let Ok(Some(_)) = managed.child.try_wait()
        {
            self.clear_dead(home_dir);
            return true;
        }
        false
    }
}

pub(super) struct CoreLifecycle {
    kernel: KernelRuntime,
    client: Option<Client>,
    last_controller_error: Option<String>,
    cached_version: String,
    stop_requested: Arc<AtomicBool>,
}

impl CoreLifecycle {
    pub fn new(stop_requested: Arc<AtomicBool>) -> Self {
        Self {
            kernel: KernelRuntime::new(),
            client: None,
            last_controller_error: None,
            cached_version: String::new(),
            stop_requested,
        }
    }

    pub fn client(&self) -> Option<&Client> {
        self.client.as_ref()
    }

    pub fn require_client(&self) -> Result<&Client, String> {
        self.client
            .as_ref()
            .ok_or_else(|| text::ERR_NO_CONTROLLER.to_string())
    }

    pub fn last_controller_error(&self) -> Option<&str> {
        self.last_controller_error.as_deref()
    }

    pub fn cached_version(&self) -> &str {
        &self.cached_version
    }

    pub fn refresh_version_if_empty(&mut self) {
        if self.cached_version.is_empty()
            && let Some(client) = &self.client
            && let Ok(ver) = client.get_version()
        {
            self.cached_version = ver;
        }
    }

    pub fn list_running(&mut self, cfg: &RuntimeConfig) -> Vec<RunningProcess> {
        self.kernel.list_running(cfg.known_kernel_path.as_deref())
    }

    pub fn active_pid(&mut self, cfg: &RuntimeConfig) -> Option<u32> {
        self.kernel
            .active_pid(cfg.known_kernel_path.as_deref(), &cfg.home_dir)
    }

    pub fn sync_runtime_record(&mut self, home_dir: &Path) -> Option<PathBuf> {
        self.kernel.sync_runtime_record(home_dir)
    }

    #[cfg(test)]
    pub fn set_client_for_test(&mut self, client: Client) {
        self.client = Some(client);
    }

    pub fn resolve_controller(&mut self, cfg: &RuntimeConfig, running: &[RunningProcess]) {
        match discover::resolve_controller(&cfg.settings, running, &cfg.config_file) {
            Ok(client) => {
                self.client = Some(client);
                self.last_controller_error = None;
            }
            Err(err) => {
                self.client = None;
                self.last_controller_error = Some(err);
            }
        }
    }

    pub fn apply_tun_state(
        &mut self,
        cfg: &RuntimeConfig,
        want_enable: bool,
        on_status: &dyn Fn(Option<&str>),
    ) -> Result<(), ActionError> {
        let client = self.require_client()?.clone();
        let set_res = client.set_tun(want_enable);
        let after = match client.get_configs() {
            Ok(configs) => configs,
            Err(cfg_err) => return Err(ActionError::general(set_res.err().unwrap_or(cfg_err))),
        };
        if !want_enable {
            set_res?;
            if after.tun_enable {
                return Err(ActionError::general(text::ERR_TUN_STILL_ENABLED));
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
        self.elevate_and_enable_tun(cfg, &after.mode, on_status)
    }

    fn elevate_and_enable_tun(
        &mut self,
        cfg: &RuntimeConfig,
        previous_mode: &str,
        on_status: &dyn Fn(Option<&str>),
    ) -> Result<(), ActionError> {
        // TUN failed to activate -> elevate running kernel via hidden UAC self-helper.
        let running = self.list_running(cfg);
        let tracked_pid = self.active_pid(cfg);
        let target =
            process::pick_target_process(&running, tracked_pid, cfg.known_kernel_path.as_deref())
                .ok_or_else(|| ActionError::general(text::ERR_LOCATE_ELEVATE_PROCESS))?
                .clone();

        let kernel_path = target.exe.clone().or_else(|| cfg.known_kernel_path.clone());
        let launch_args = resolve_elevation_launch_args(
            tracked_pid == Some(target.pid),
            &target.cmd,
            &cfg.portable_root,
            &cfg.settings,
            &cfg.home_dir,
            &cfg.config_file,
        )?;
        let before_pids: HashSet<u32> = running.iter().map(|p| p.pid).collect();

        on_status(Some(text::STATUS_ELEVATING_CORE));
        let mut helper_args = vec![
            platform::ELEVATE_START_FLAG.to_string(),
            target.pid.to_string(),
        ];
        helper_args.extend(launch_args.iter().cloned());

        let code = platform::run_elevated_self(&helper_args);
        on_status(None);

        match code {
            Ok(0) => {}
            Ok(c) => {
                let reason = process::helper_exit_reason(c);
                tracing::error!(
                    "Elevated helper failed while starting mihomo (code {c}: {})",
                    reason.unwrap_or("unknown")
                );
                return Err(ActionError::general(text::format_elevated_helper_failed(
                    c, reason,
                )));
            }
            Err(_) => return Err(ActionError::general(text::ERR_TUN_ELEVATION_DECLINED)),
        }

        self.cached_version.clear();
        let after_running = self
            .kernel
            .list_running(kernel_path.as_deref().or(cfg.known_kernel_path.as_deref()));
        let Some(spawned) =
            process::pick_spawned_process(&after_running, &before_pids, kernel_path.as_deref())
                .cloned()
        else {
            self.kernel.clear_dead(&cfg.home_dir);
            return Err(ActionError::transient_wait(
                text::ERR_CORE_STOPPED_UNEXPECTEDLY,
            ));
        };

        let resolved_exe = spawned.exe.clone().or(kernel_path);
        self.kernel
            .record_elevated(spawned.pid, &cfg.home_dir, resolved_exe.as_deref());

        let cmd = if spawned.cmd.is_empty() {
            synthetic_argv(resolved_exe.as_deref(), &launch_args)
        } else {
            spawned.cmd
        };
        let elevated = [RunningProcess {
            pid: spawned.pid,
            exe: resolved_exe,
            cmd,
        }];
        self.resolve_controller(cfg, &elevated);
        self.wait_for_controller(
            &cfg.home_dir,
            KERNEL_RESTART_BUDGET,
            text::ERR_CORE_RESTART_TIMEOUT,
            on_status,
        )?;
        let elevated_client = self.require_client()?.clone();
        let tun_res = verify_or_enable_tun_after_elevation(&elevated_client);
        let needs_mode_restore = !previous_mode.is_empty()
            && match &tun_res {
                Ok(configs) => !configs.mode.eq_ignore_ascii_case(previous_mode),
                Err(_) => true,
            };
        if needs_mode_restore && let Err(err) = elevated_client.set_mode(previous_mode) {
            tracing::warn!(
                "Failed to restore proxy mode '{previous_mode}' after TUN elevation: {err}"
            );
        }
        tun_res.map(|_| ()).map_err(ActionError::general)
    }

    pub fn start(
        &mut self,
        cfg: &RuntimeConfig,
        on_status: &dyn Fn(Option<&str>),
    ) -> Result<(), ActionError> {
        let running = self.list_running(cfg);
        let active_pid = self.active_pid(cfg);
        if process::pick_target_process(&running, active_pid, cfg.known_kernel_path.as_deref())
            .is_some()
        {
            self.resolve_controller(cfg, &running);
            return Ok(());
        }

        let kernel_path = match (
            cfg.known_kernel_path.as_ref(),
            cfg.kernel_path_error.as_deref(),
        ) {
            (Some(path), _) => path.clone(),
            (None, Some(err)) => return Err(ActionError::general(err)),
            (None, None) => return Err(ActionError::general(text::ERR_CORE_NOT_FOUND)),
        };

        let _ = std::fs::create_dir_all(&cfg.home_dir);
        mihomo::ensure_default_config(&cfg.config_file);

        let args = discover::build_launch_args_for_paths(
            &cfg.portable_root,
            &cfg.settings,
            &cfg.home_dir,
            &cfg.config_file,
        )?;

        on_status(Some(text::STATUS_STARTING_CORE));
        let child = match process::spawn_normal(&kernel_path, &args) {
            Ok(child) => child,
            Err(err) => {
                on_status(None);
                return Err(ActionError::general(err));
            }
        };
        let pid = self
            .kernel
            .record_managed(child, &cfg.home_dir, &kernel_path);
        self.cached_version.clear();

        let cmd = synthetic_argv(Some(&kernel_path), &args);
        let spawned = [RunningProcess {
            pid,
            exe: Some(kernel_path),
            cmd,
        }];
        self.resolve_controller(cfg, &spawned);
        self.wait_for_controller(
            &cfg.home_dir,
            KERNEL_STARTUP_BUDGET,
            text::ERR_CORE_NOT_RESPONDING,
            on_status,
        )
    }

    pub fn stop_for_restart(
        &mut self,
        cfg: &RuntimeConfig,
        running: Vec<RunningProcess>,
        on_status: &dyn Fn(Option<&str>),
    ) -> Result<bool, ActionError> {
        if !self.kernel.has_managed() && running.is_empty() {
            return Ok(true);
        }
        on_status(Some(text::STATUS_RESTARTING_CORE));
        let stop_res = self.stop_active_kernel(cfg, true, Some(running));
        on_status(None);
        match stop_res? {
            StopOutcome::Stopped => Ok(true),
            StopOutcome::DeclinedByUser => Ok(false),
            StopOutcome::AmbiguousExternal => {
                Err(ActionError::general(text::ERR_MULTIPLE_EXTERNAL_CORES))
            }
        }
    }

    pub fn stop_on_quit(
        &mut self,
        cfg: &RuntimeConfig,
        interactive: bool,
        has_managed_sysproxy: bool,
    ) -> Result<(), String> {
        match self.stop_active_kernel(cfg, interactive, None) {
            Ok(
                StopOutcome::Stopped | StopOutcome::DeclinedByUser | StopOutcome::AmbiguousExternal,
            ) => {
                disable_managed_system_proxy(has_managed_sysproxy);
                Ok(())
            }
            Err(err) if !interactive => {
                tracing::warn!("Failed to stop core during non-interactive shutdown: {err}");
                disable_managed_system_proxy(has_managed_sysproxy);
                Ok(())
            }
            Err(err) => {
                let msg = text::format_confirm_quit_anyway(&err);
                if platform::confirm_yes_no(text::TITLE_QUIT_ANYWAY, &msg) {
                    disable_managed_system_proxy(has_managed_sysproxy);
                    Ok(())
                } else {
                    Err(err)
                }
            }
        }
    }

    fn stop_active_kernel(
        &mut self,
        cfg: &RuntimeConfig,
        interactive: bool,
        prefetched: Option<Vec<RunningProcess>>,
    ) -> Result<StopOutcome, String> {
        if self.kernel.kill_managed_if_running(&cfg.home_dir) {
            self.cached_version.clear();
            return Ok(StopOutcome::Stopped);
        }

        let running = prefetched.unwrap_or_else(|| self.list_running(cfg));
        let active_pid = self.active_pid(cfg);
        let tracked_target =
            process::pick_target_process(&running, active_pid, cfg.known_kernel_path.as_deref())
                .cloned();
        let target = match tracked_target {
            Some(found) => found,
            None => {
                if running.is_empty() {
                    self.clear_running_tracking(&cfg.home_dir);
                    return Ok(StopOutcome::Stopped);
                }
                if running.len() > 1 {
                    return Ok(StopOutcome::AmbiguousExternal);
                }
                if !interactive {
                    return Ok(StopOutcome::DeclinedByUser);
                }
                let candidate = &running[0];
                let msg = text::format_confirm_stop_core(candidate.pid, candidate.exe.as_deref());
                if !platform::confirm_yes_no(text::TITLE_CONFIRM_STOP, &msg) {
                    return Ok(StopOutcome::DeclinedByUser);
                }
                candidate.clone()
            }
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
                let reason = process::helper_exit_reason(code).unwrap_or("unknown");
                tracing::error!(
                    "Elevated stop helper failed for pid {} (code {code}: {reason})",
                    target.pid
                );
                return Err(text::ERR_STOP_ELEVATION_REQUIRED.to_string());
            }
        }

        self.clear_running_tracking(&cfg.home_dir);
        Ok(StopOutcome::Stopped)
    }

    fn clear_running_tracking(&mut self, home_dir: &Path) {
        self.kernel.clear_tracking(home_dir);
        self.cached_version.clear();
    }

    fn wait_for_controller(
        &mut self,
        home_dir: &Path,
        budget: Duration,
        timeout_msg: &str,
        on_status: &dyn Fn(Option<&str>),
    ) -> Result<(), ActionError> {
        if self.client.is_none() {
            on_status(None);
            let err = self
                .last_controller_error
                .clone()
                .unwrap_or_else(|| text::ERR_NO_CONTROLLER.to_string());
            return Err(ActionError::general(err));
        }

        let start = Instant::now();
        let mut waiting_notified = false;
        let res = loop {
            if start.elapsed() >= budget {
                break Err(ActionError::transient_wait(timeout_msg));
            }
            if self.stop_requested.load(Ordering::Acquire) {
                break Err(ActionError::general(
                    text::ERR_WAIT_INTERRUPTED_FOR_SHUTDOWN,
                ));
            }
            if self.kernel.try_wait_managed_exited(home_dir) {
                self.cached_version.clear();
                break Err(ActionError::transient_wait(
                    text::ERR_CORE_STOPPED_UNEXPECTEDLY,
                ));
            }
            if let Some(client) = &self.client
                && client.probe().is_ok()
            {
                break Ok(());
            }
            if !waiting_notified && start.elapsed() >= Duration::from_secs(2) {
                on_status(Some(text::STATUS_WAITING_CONTROLLER));
                waiting_notified = true;
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        on_status(None);
        res
    }
}

fn disable_managed_system_proxy(has_managed_sysproxy: bool) {
    if has_managed_sysproxy
        && platform::proxy_is_enabled()
        && let Err(err) = platform::proxy_disable()
    {
        tracing::warn!("Failed to disable system proxy on shutdown: {err}");
    }
}

fn synthetic_argv(exe: Option<&Path>, launch_args: &[String]) -> Vec<String> {
    let exe_str = exe
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| platform::kernel_exe_name().to_string());
    let mut cmd = Vec::with_capacity(1 + launch_args.len());
    cmd.push(exe_str);
    cmd.extend(launch_args.iter().cloned());
    cmd
}

fn resolve_elevation_launch_args(
    is_tracked: bool,
    target_cmd: &[String],
    portable_root: &Path,
    settings: &TraySettings,
    home_dir: &Path,
    config_file: &Path,
) -> Result<Vec<String>, String> {
    if !is_tracked && !target_cmd.is_empty() {
        return Ok(target_cmd[1..].to_vec());
    }
    discover::build_launch_args_for_paths(portable_root, settings, home_dir, config_file)
}

fn verify_or_enable_tun_after_elevation(client: &Client) -> Result<CoreConfigs, String> {
    client
        .set_tun(true)
        .map_err(|err| text::format_tun_enable_after_elevation_failed(&err))?;
    let verify = client.get_configs()?;
    if !verify.tun_enable {
        return Err(text::ERR_TUN_FAILED_AFTER_ELEVATION.to_string());
    }
    Ok(verify)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::net::TcpListener;
    use std::path::PathBuf;

    use super::*;
    use crate::config::tray::TraySettings;
    use crate::core::test_http::{read_http_request, send_http_response};

    #[test]
    fn verify_or_enable_tun_after_elevation_patches_and_verifies_tun() {
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
                    send_http_response(&mut stream, "204 No Content", "");
                } else {
                    let body = format!(
                        r#"{{"mode":"rule","mixed-port":7890,"tun":{{"enable":{tun_on}}}}}"#
                    );
                    send_http_response(&mut stream, "200 OK", &body);
                }
            }
        });

        let client = Client::new(&format!("127.0.0.1:{port}"), "", 2000).unwrap();
        verify_or_enable_tun_after_elevation(&client).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn verify_or_enable_tun_after_elevation_errors_when_still_disabled() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let req = read_http_request(&mut stream);
                if req.starts_with("PATCH /configs ") {
                    send_http_response(&mut stream, "204 No Content", "");
                } else {
                    send_http_response(
                        &mut stream,
                        "200 OK",
                        r#"{"mode":"rule","mixed-port":7890,"tun":{"enable":false}}"#,
                    );
                }
            }
        });

        let client = Client::new(&format!("127.0.0.1:{port}"), "", 2000).unwrap();
        let err = verify_or_enable_tun_after_elevation(&client).unwrap_err();
        assert_eq!(err, text::ERR_TUN_FAILED_AFTER_ELEVATION);
        handle.join().unwrap();
    }

    #[test]
    fn wait_for_controller_fails_immediately_when_controller_unresolved() {
        let root = crate::config::test_temp_dir("worker_wait_no_client");
        let cfg = RuntimeConfig::new(
            root.clone(),
            crate::config::tray::settings_path(&root),
            TraySettings::default(),
            None,
        );
        let mut core = CoreLifecycle::new(Arc::new(AtomicBool::new(false)));
        core.last_controller_error = Some(text::ERR_NO_CONTROLLER.to_string());
        let note = RefCell::new(Some(text::STATUS_STARTING_CORE.to_string()));

        let start = Instant::now();
        let err = core
            .wait_for_controller(
                &cfg.home_dir,
                Duration::from_secs(6),
                text::ERR_CORE_NOT_RESPONDING,
                &|n| *note.borrow_mut() = n.map(ToString::to_string),
            )
            .unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(err.message, text::ERR_NO_CONTROLLER);
        assert!(!err.transient_wait);
        assert_eq!(*note.borrow(), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stop_active_kernel_reports_declined_and_ambiguous_external_processes() {
        let root = crate::config::test_temp_dir("worker_stop_outcome");
        let cfg = RuntimeConfig::new(
            root.clone(),
            crate::config::tray::settings_path(&root),
            TraySettings::default(),
            None,
        );
        let mut core = CoreLifecycle::new(Arc::new(AtomicBool::new(false)));

        let single_external = vec![RunningProcess {
            pid: 999_991,
            exe: Some(PathBuf::from("/external/mihomo")),
            cmd: Vec::new(),
        }];
        let declined = core
            .stop_active_kernel(&cfg, false, Some(single_external))
            .unwrap();
        assert_eq!(declined, StopOutcome::DeclinedByUser);

        let multi_external = vec![
            RunningProcess {
                pid: 999_991,
                exe: Some(PathBuf::from("/external/mihomo-1")),
                cmd: Vec::new(),
            },
            RunningProcess {
                pid: 999_992,
                exe: Some(PathBuf::from("/external/mihomo-2")),
                cmd: Vec::new(),
            },
        ];
        let ambiguous = core
            .stop_active_kernel(&cfg, true, Some(multi_external))
            .unwrap();
        assert_eq!(ambiguous, StopOutcome::AmbiguousExternal);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn elevation_launch_args_preserve_external_cmdline_when_untracked() {
        let root = crate::config::test_temp_dir("elevate_args");
        let settings = TraySettings::default();
        let home = root.join("config");
        let cfg = home.join("config.yaml");

        let external_cmd = vec![
            "mihomo.exe".to_string(),
            "-d".to_string(),
            "/ext/home".to_string(),
            "-ext-ctl".to_string(),
            "127.0.0.1:9099".to_string(),
        ];
        assert_eq!(
            resolve_elevation_launch_args(false, &external_cmd, &root, &settings, &home, &cfg)
                .unwrap(),
            vec!["-d", "/ext/home", "-ext-ctl", "127.0.0.1:9099"]
        );

        let managed_args =
            resolve_elevation_launch_args(true, &external_cmd, &root, &settings, &home, &cfg)
                .unwrap();
        assert!(managed_args.contains(&home.display().to_string()));
        assert!(!managed_args.contains(&"127.0.0.1:9099".to_string()));

        let _ = std::fs::remove_dir_all(&root);
    }
}
