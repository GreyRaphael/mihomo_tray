//! Kernel process lifecycle, controller probing, and TUN UAC elevation for [`super::Worker`].

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::Worker;
use crate::config::mihomo;
use crate::core::api::Client;
use crate::core::discover;
use crate::core::process::{self, RunningProcess, RuntimeRecord};
use crate::platform;
use crate::text;

const KERNEL_STARTUP_BUDGET: Duration = Duration::from_secs(6);
const KERNEL_RESTART_BUDGET: Duration = Duration::from_secs(10);

impl Worker {
    pub(super) fn re_resolve_controller(&mut self, running: &[RunningProcess]) {
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
                .is_recorded_kernel_alive(pid, self.cfg.known_kernel_path.as_deref())
            {
                return Some(pid);
            }
            self.recorded_pid = None;
            process::clear_runtime_record(&self.cfg.home_dir);
        }
        None
    }

    pub(super) fn apply_tun_state(&mut self, want_enable: bool) -> Result<(), String> {
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

    pub(super) fn start_core_internal(&mut self) -> Result<(), String> {
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

        let kernel_path = match (&self.cfg.known_kernel_path, &self.cfg.kernel_path_error) {
            (Some(path), _) => path.clone(),
            (None, Some(err)) => return Err(err.clone()),
            (None, None) => return Err(text::ERR_CORE_NOT_FOUND.to_string()),
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
        let child = process::spawn_normal(&kernel_path, &args)?;
        let pid = child.pid();
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

    pub(super) fn force_restart_core(&mut self) -> Result<(), String> {
        let saved = self.capture_live_or_persisted_state();
        self.sub_tasks.invalidate();
        self.state_sync.set_ready_restore(saved);
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

    pub(super) fn stop_core_internal(&mut self, interactive: bool) -> Result<(), String> {
        match self.stop_active_kernel(interactive, None) {
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

    fn disable_managed_system_proxy(&self) {
        if self.state_sync.has_persisted_system_proxy()
            && platform::proxy_is_enabled()
            && let Err(err) = platform::proxy_disable()
        {
            tracing::warn!("Failed to disable system proxy on shutdown: {err}");
        }
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
}

pub(super) fn verify_or_enable_tun_after_elevation(client: &Client) -> Result<(), String> {
    client
        .set_tun(true)
        .map_err(|err| text::format_tun_enable_after_elevation_failed(&err))?;
    let verify = client.get_configs()?;
    if !verify.tun_enable {
        return Err(text::ERR_TUN_FAILED_AFTER_ELEVATION.to_string());
    }
    Ok(())
}
