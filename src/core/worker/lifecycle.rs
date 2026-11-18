//! Kernel process lifecycle, controller probing, and TUN UAC elevation for [`super::Worker`].

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::Worker;
use crate::config::mihomo;
use crate::core::api::{Client, CoreConfigs};
use crate::core::discover;
use crate::core::process::{self, RunningProcess, RuntimeRecord};
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

impl Worker {
    pub(super) fn re_resolve_controller(&mut self, running: &[RunningProcess]) {
        match discover::resolve_controller(self.cfg.settings(), running, self.cfg.config_file()) {
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

    pub(super) fn active_pid(&mut self) -> Option<u32> {
        if let Some(ref mut managed) = self.managed {
            if matches!(managed.child.try_wait(), Ok(None)) {
                return Some(managed.pid());
            }
            self.managed = None;
        }
        if let Some(pid) = self.recorded_pid {
            if self
                .process_inspector
                .is_recorded_kernel_alive(pid, self.cfg.known_kernel_path())
            {
                return Some(pid);
            }
            self.recorded_pid = None;
            process::clear_runtime_record(self.cfg.home_dir());
        }
        None
    }

    pub(super) fn apply_tun_state(&mut self, want_enable: bool) -> Result<(), String> {
        let client = self.require_client()?.clone();
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
        let previous_mode = after.mode;
        if let Err(err) = &set_res {
            tracing::info!("Non-elevated set_tun(true) failed ({err}); attempting UAC elevation");
        }

        // TUN failed to activate -> elevate running kernel via hidden UAC self-helper.
        let running = self
            .process_inspector
            .list_running_mihomo(self.cfg.known_kernel_path());
        let target =
            process::pick_target_process(&running, self.active_pid(), self.cfg.known_kernel_path())
                .ok_or_else(|| text::ERR_LOCATE_ELEVATE_PROCESS.to_string())?;

        let kernel_path = target
            .exe
            .clone()
            .or_else(|| self.cfg.known_kernel_path().map(ToOwned::to_owned));
        let launch_args = discover::build_launch_args(
            self.cfg.portable_root(),
            self.cfg.settings(),
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
            Ok(c) => {
                let reason = process::helper_exit_reason(c);
                tracing::error!(
                    "Elevated helper failed while starting mihomo (code {c}: {})",
                    reason.unwrap_or("unknown")
                );
                return Err(text::format_elevated_helper_failed(c, reason));
            }
            Err(_) => return Err(text::ERR_TUN_ELEVATION_DECLINED.to_string()),
        };

        self.managed = None;
        self.recorded_pid = Some(new_pid);
        self.cached_version.clear();
        if let Some(ref kernel) = kernel_path {
            process::save_runtime_record(
                self.cfg.home_dir(),
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
        tun_res.map(|_| ())
    }

    pub(super) fn start_core_internal(&mut self) -> Result<(), String> {
        self.reload_paths_and_runtime();
        let running = self
            .process_inspector
            .list_running_mihomo(self.cfg.known_kernel_path());
        if process::pick_target_process(&running, self.active_pid(), self.cfg.known_kernel_path())
            .is_some()
        {
            self.re_resolve_controller(&running);
            return Ok(());
        }

        let kernel_path = match (self.cfg.known_kernel_path(), self.cfg.kernel_path_error()) {
            (Some(path), _) => path.to_path_buf(),
            (None, Some(err)) => return Err(err.to_string()),
            (None, None) => return Err(text::ERR_CORE_NOT_FOUND.to_string()),
        };

        let _ = std::fs::create_dir_all(self.cfg.home_dir());
        mihomo::ensure_default_config(self.cfg.config_file());

        let args = discover::build_launch_args_for_paths(
            self.cfg.portable_root(),
            self.cfg.settings(),
            self.cfg.home_dir(),
            self.cfg.config_file(),
        )?;

        self.set_status_note(Some(text::STATUS_STARTING_CORE));
        let child = match process::spawn_normal(&kernel_path, &args) {
            Ok(child) => child,
            Err(err) => {
                self.set_status_note(None);
                return Err(err);
            }
        };
        let pid = child.pid();
        self.managed = Some(child);
        self.recorded_pid = Some(pid);
        self.cached_version.clear();
        process::save_runtime_record(
            self.cfg.home_dir(),
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
        self.wait_for_controller(KERNEL_STARTUP_BUDGET, text::ERR_CORE_NOT_RESPONDING)
    }

    pub(super) fn force_restart_core(&mut self) -> Result<(), String> {
        let saved = self.capture_live_or_persisted_state();
        self.sub_tasks.invalidate();
        self.state_sync.arm_ready_restore(saved);
        self.reload_paths_and_runtime();
        let running = self
            .process_inspector
            .list_running_mihomo(self.cfg.known_kernel_path());
        if self.managed.is_some() || !running.is_empty() {
            self.set_status_note(Some(text::STATUS_RESTARTING_CORE));
            let stop_res = self.stop_active_kernel(true, Some(running));
            self.set_status_note(None);
            if !self.should_start_after_stop(stop_res?)? {
                return Ok(());
            }
        }
        self.start_core_internal()
    }

    pub(super) fn should_start_after_stop(&mut self, outcome: StopOutcome) -> Result<bool, String> {
        match outcome {
            StopOutcome::Stopped => Ok(true),
            StopOutcome::DeclinedByUser => {
                self.state_sync.clear_pending();
                Ok(false)
            }
            StopOutcome::AmbiguousExternal => {
                self.state_sync.clear_pending();
                Err(text::ERR_MULTIPLE_EXTERNAL_CORES.to_string())
            }
        }
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
        // First try managed non-elevated child directly.
        if let Some(mut managed) = self.managed.take() {
            if matches!(managed.child.try_wait(), Ok(None)) {
                if managed.child.kill().is_ok() {
                    let _ = managed.child.wait();
                    self.clear_running_tracking();
                    return Ok(StopOutcome::Stopped);
                }
            } else {
                self.clear_running_tracking();
                return Ok(StopOutcome::Stopped);
            }
        }

        let running = prefetched.unwrap_or_else(|| {
            self.process_inspector
                .list_running_mihomo(self.cfg.known_kernel_path())
        });
        let tracked_target =
            process::pick_target_process(&running, self.active_pid(), self.cfg.known_kernel_path())
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
        self.recorded_pid = None;
        self.cached_version.clear();
        process::clear_runtime_record(self.cfg.home_dir());
    }

    fn wait_for_controller(&mut self, budget: Duration, timeout_msg: &str) -> Result<(), String> {
        let start = Instant::now();
        let mut waiting_notified = false;
        let res = loop {
            if start.elapsed() >= budget {
                break Err(timeout_msg.to_string());
            }
            if self.stop_requested.load(Ordering::Acquire) {
                break Err(text::ERR_WAIT_INTERRUPTED_FOR_SHUTDOWN.to_string());
            }
            if let Some(ref mut managed) = self.managed
                && let Ok(Some(_)) = managed.child.try_wait()
            {
                self.managed = None;
                self.clear_running_tracking();
                break Err(text::ERR_CORE_STOPPED_UNEXPECTEDLY.to_string());
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

pub(super) fn verify_or_enable_tun_after_elevation(client: &Client) -> Result<CoreConfigs, String> {
    client
        .set_tun(true)
        .map_err(|err| text::format_tun_enable_after_elevation_failed(&err))?;
    let verify = client.get_configs()?;
    if !verify.tun_enable {
        return Err(text::ERR_TUN_FAILED_AFTER_ELEVATION.to_string());
    }
    Ok(verify)
}
