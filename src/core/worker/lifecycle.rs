//! Kernel process lifecycle, controller probing, and TUN UAC elevation for [`super::Worker`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::{ActionError, Worker};
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

pub(super) struct KernelRuntime {
    inspector: ProcessInspector,
    managed: Option<ManagedChild>,
    recorded_pid: Option<u32>,
}

impl KernelRuntime {
    pub fn new() -> Self {
        Self {
            inspector: ProcessInspector::new(),
            managed: None,
            recorded_pid: None,
        }
    }

    pub fn has_managed(&self) -> bool {
        self.managed.is_some()
    }

    pub fn list_running(&mut self, expected_kernel: Option<&Path>) -> Vec<RunningProcess> {
        self.inspector.list_running_mihomo(expected_kernel)
    }

    pub fn active_pid(&mut self, expected_kernel: Option<&Path>, home_dir: &Path) -> Option<u32> {
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

    pub fn sync_runtime_record(&mut self, home_dir: &Path) -> Option<PathBuf> {
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

impl Worker {
    pub(super) fn re_resolve_controller(&mut self, running: &[RunningProcess]) {
        match discover::resolve_controller(&self.cfg.settings, running, &self.cfg.config_file) {
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

    pub(super) fn apply_tun_state(&mut self, want_enable: bool) -> Result<(), ActionError> {
        let client = self.require_client()?.clone();
        let set_res = client.set_tun(want_enable);
        let after = match client.get_configs() {
            Ok(cfg) => cfg,
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
        let previous_mode = after.mode;
        if let Err(err) = &set_res {
            tracing::info!("Non-elevated set_tun(true) failed ({err}); attempting UAC elevation");
        }

        // TUN failed to activate -> elevate running kernel via hidden UAC self-helper.
        let running = self
            .kernel
            .list_running(self.cfg.known_kernel_path.as_deref());
        let tracked_pid = self
            .kernel
            .active_pid(self.cfg.known_kernel_path.as_deref(), &self.cfg.home_dir);
        let target = process::pick_target_process(
            &running,
            tracked_pid,
            self.cfg.known_kernel_path.as_deref(),
        )
        .ok_or_else(|| ActionError::general(text::ERR_LOCATE_ELEVATE_PROCESS))?
        .clone();

        let kernel_path = target
            .exe
            .clone()
            .or_else(|| self.cfg.known_kernel_path.clone());
        let launch_args = resolve_elevation_launch_args(
            tracked_pid == Some(target.pid),
            &target.cmd,
            &self.cfg.portable_root,
            &self.cfg.settings,
            &self.cfg.home_dir,
            &self.cfg.config_file,
        )?;
        let before_pids: HashSet<u32> = running.iter().map(|p| p.pid).collect();

        self.set_status_note(Some(text::STATUS_ELEVATING_CORE));
        let mut helper_args = vec![
            platform::ELEVATE_START_FLAG.to_string(),
            target.pid.to_string(),
        ];
        helper_args.extend(launch_args.iter().cloned());

        let code = platform::run_elevated_self(&helper_args);
        self.set_status_note(None);

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
        let after_running = self.kernel.list_running(
            kernel_path
                .as_deref()
                .or(self.cfg.known_kernel_path.as_deref()),
        );
        let Some(spawned) =
            process::pick_spawned_process(&after_running, &before_pids, kernel_path.as_deref())
                .cloned()
        else {
            self.kernel.clear_dead(&self.cfg.home_dir);
            return Err(ActionError::transient_wait(
                text::ERR_CORE_STOPPED_UNEXPECTEDLY,
            ));
        };

        let resolved_exe = spawned.exe.clone().or(kernel_path);
        self.kernel
            .record_elevated(spawned.pid, &self.cfg.home_dir, resolved_exe.as_deref());

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
        self.re_resolve_controller(&elevated);
        self.wait_for_controller(KERNEL_RESTART_BUDGET, text::ERR_CORE_RESTART_TIMEOUT)?;
        let elevated_client = self.require_client()?.clone();
        let tun_res = verify_or_enable_tun_after_elevation(&elevated_client);
        let needs_mode_restore = !previous_mode.is_empty()
            && match &tun_res {
                Ok(cfg) => !cfg.mode.eq_ignore_ascii_case(&previous_mode),
                Err(_) => true,
            };
        if needs_mode_restore && let Err(err) = elevated_client.set_mode(&previous_mode) {
            tracing::warn!(
                "Failed to restore proxy mode '{previous_mode}' after TUN elevation: {err}"
            );
        }
        tun_res.map(|_| ()).map_err(ActionError::general)
    }

    pub(super) fn start_core_internal(&mut self) -> Result<(), ActionError> {
        self.reload_paths_and_runtime();
        let running = self
            .kernel
            .list_running(self.cfg.known_kernel_path.as_deref());
        let active_pid = self
            .kernel
            .active_pid(self.cfg.known_kernel_path.as_deref(), &self.cfg.home_dir);
        if process::pick_target_process(
            &running,
            active_pid,
            self.cfg.known_kernel_path.as_deref(),
        )
        .is_some()
        {
            self.re_resolve_controller(&running);
            return Ok(());
        }

        let kernel_path = match (
            self.cfg.known_kernel_path.as_ref(),
            self.cfg.kernel_path_error.as_deref(),
        ) {
            (Some(path), _) => path.clone(),
            (None, Some(err)) => return Err(ActionError::general(err)),
            (None, None) => return Err(ActionError::general(text::ERR_CORE_NOT_FOUND)),
        };

        let _ = std::fs::create_dir_all(&self.cfg.home_dir);
        mihomo::ensure_default_config(&self.cfg.config_file);

        let args = discover::build_launch_args_for_paths(
            &self.cfg.portable_root,
            &self.cfg.settings,
            &self.cfg.home_dir,
            &self.cfg.config_file,
        )?;

        self.set_status_note(Some(text::STATUS_STARTING_CORE));
        let child = match process::spawn_normal(&kernel_path, &args) {
            Ok(child) => child,
            Err(err) => {
                self.set_status_note(None);
                return Err(ActionError::general(err));
            }
        };
        let pid = self
            .kernel
            .record_managed(child, &self.cfg.home_dir, &kernel_path);
        self.cached_version.clear();

        let cmd = synthetic_argv(Some(&kernel_path), &args);
        let spawned = [RunningProcess {
            pid,
            exe: Some(kernel_path),
            cmd,
        }];
        self.re_resolve_controller(&spawned);
        self.wait_for_controller(KERNEL_STARTUP_BUDGET, text::ERR_CORE_NOT_RESPONDING)
    }

    pub(super) fn force_restart_core(&mut self) -> Result<(), ActionError> {
        let running = self
            .kernel
            .list_running(self.cfg.known_kernel_path.as_deref());
        self.force_restart_with_running(running)
    }

    pub(super) fn force_restart_with_running(
        &mut self,
        running: Vec<RunningProcess>,
    ) -> Result<(), ActionError> {
        let saved = self.capture_live_or_persisted_state();
        if self.kernel.has_managed() || !running.is_empty() {
            self.set_status_note(Some(text::STATUS_RESTARTING_CORE));
            let stop_res = self.stop_active_kernel(true, Some(running));
            self.set_status_note(None);
            match stop_res? {
                StopOutcome::Stopped => {}
                StopOutcome::DeclinedByUser => return Ok(()),
                StopOutcome::AmbiguousExternal => {
                    return Err(ActionError::general(text::ERR_MULTIPLE_EXTERNAL_CORES));
                }
            }
        }
        self.invalidate_async_tasks();
        self.state_sync.arm_ready_restore(saved);
        self.start_core_internal()
    }

    pub(super) fn stop_core_internal(&mut self, interactive: bool) -> Result<(), String> {
        match self.stop_active_kernel(interactive, None) {
            Ok(
                StopOutcome::Stopped | StopOutcome::DeclinedByUser | StopOutcome::AmbiguousExternal,
            ) => {
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

    fn disable_managed_system_proxy(&self) {
        if self.state_sync.has_persisted_system_proxy()
            && platform::proxy_is_enabled()
            && let Err(err) = platform::proxy_disable()
        {
            tracing::warn!("Failed to disable system proxy on shutdown: {err}");
        }
    }

    pub(super) fn stop_active_kernel(
        &mut self,
        interactive: bool,
        prefetched: Option<Vec<RunningProcess>>,
    ) -> Result<StopOutcome, String> {
        if self.kernel.kill_managed_if_running(&self.cfg.home_dir) {
            self.cached_version.clear();
            return Ok(StopOutcome::Stopped);
        }

        let running = prefetched.unwrap_or_else(|| {
            self.kernel
                .list_running(self.cfg.known_kernel_path.as_deref())
        });
        let active_pid = self
            .kernel
            .active_pid(self.cfg.known_kernel_path.as_deref(), &self.cfg.home_dir);
        let tracked_target = process::pick_target_process(
            &running,
            active_pid,
            self.cfg.known_kernel_path.as_deref(),
        )
        .cloned();
        let target = match tracked_target {
            Some(found) => found,
            None => {
                if running.is_empty() {
                    self.clear_running_tracking();
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

        self.clear_running_tracking();
        Ok(StopOutcome::Stopped)
    }

    fn clear_running_tracking(&mut self) {
        self.kernel.clear_tracking(&self.cfg.home_dir);
        self.cached_version.clear();
    }

    fn wait_for_controller(
        &mut self,
        budget: Duration,
        timeout_msg: &str,
    ) -> Result<(), ActionError> {
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
            if self.kernel.try_wait_managed_exited(&self.cfg.home_dir) {
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
                self.set_status_note(Some(text::STATUS_WAITING_CONTROLLER));
                waiting_notified = true;
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        self.set_status_note(None);
        res
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
    use std::io::Write;
    use std::net::TcpListener;
    use std::path::PathBuf;

    use super::*;
    use crate::config::tray::TraySettings;
    use crate::core::worker::tests::{read_http_request, test_worker};

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

    #[test]
    fn stop_active_kernel_reports_declined_and_ambiguous_external_processes() {
        let root = crate::config::test_temp_dir("worker_stop_outcome");
        let (mut worker, _rx, _shared) = test_worker(&root, TraySettings::default(), None);

        let single_external = vec![RunningProcess {
            pid: 999_991,
            exe: Some(PathBuf::from("/external/mihomo")),
            cmd: Vec::new(),
        }];
        let declined = worker
            .stop_active_kernel(false, Some(single_external))
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
        let ambiguous = worker
            .stop_active_kernel(true, Some(multi_external))
            .unwrap();
        assert_eq!(ambiguous, StopOutcome::AmbiguousExternal);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn force_restart_declining_external_stop_preserves_deferred_restore_and_tasks() {
        let root = crate::config::test_temp_dir("worker_restart_preserve");
        let (mut worker, _rx, _shared) = test_worker(&root, TraySettings::default(), None);
        let saved = crate::config::state_store::PersistedState {
            system_proxy: true,
            mode: Some("global".to_string()),
            tun: Some(false),
        };
        worker.state_sync.defer_restore(7, saved);
        worker.sub_tasks.invalidate();
        worker.latency_tasks.begin();

        // Declining external stop in force_restart_with_running must preserve deferred restore,
        // active latency task, and current config paths.
        worker
            .force_restart_with_running(vec![RunningProcess {
                pid: 999_991,
                exe: Some(PathBuf::from("/external/mihomo")),
                cmd: Vec::new(),
            }])
            .unwrap();
        assert!(worker.state_sync.has_deferred_restore());
        assert!(worker.latency_tasks.is_active());
        assert_eq!(worker.cfg.home_dir, root.join("config"));

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
