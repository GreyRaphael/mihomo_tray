//! Portable process tracking (`.runtime.json` + `sysinfo 0.40`) and UAC helper entrypoints.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind};

use crate::platform;
use crate::text::RUNTIME_LOCK_NAME;

const STOP_WAIT_BUDGET: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct RunningProcess {
    pub pid: u32,
    pub exe: Option<PathBuf>,
    pub cmd: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeRecord {
    pub pid: u32,
    pub kernel_path: PathBuf,
    pub elevated: bool,
}

#[derive(Debug)]
pub struct ManagedChild {
    pub child: Option<Child>,
    pub pid: u32,
    pub elevated: bool,
}

pub fn runtime_record_path(home_dir: &Path) -> PathBuf {
    home_dir.join(RUNTIME_LOCK_NAME)
}

pub fn save_runtime_record(home_dir: &Path, record: &RuntimeRecord) {
    let path = runtime_record_path(home_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(record) {
        let _ = std::fs::write(path, json);
    }
}

pub fn load_runtime_record(home_dir: &Path) -> Option<RuntimeRecord> {
    let path = runtime_record_path(home_dir);
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn clear_runtime_record(home_dir: &Path) {
    let _ = std::fs::remove_file(runtime_record_path(home_dir));
}

pub fn list_running_mihomo(expected_kernel: Option<&Path>) -> Vec<RunningProcess> {
    let Ok(mut sys) = System::new_with_specifics(
        RefreshKind::nothing().with_processes(
            ProcessRefreshKind::nothing()
                .with_exe(UpdateKind::Always)
                .with_cmd(UpdateKind::Always),
        ),
    ) else {
        return Vec::new();
    };
    sys.refresh_processes(ProcessesToUpdate::All, true);

    let mut out = Vec::new();
    for (pid, proc_info) in sys.processes() {
        let name = proc_info.name().to_string_lossy();
        let exe = proc_info.exe().map(Path::to_path_buf);

        let name_matches = platform::is_kernel_candidate_name(&name)
            && !name.to_ascii_lowercase().starts_with("mihomo-tray")
            && !name.to_ascii_lowercase().starts_with("mihomo_tray");
        let path_matches = expected_kernel
            .zip(exe.as_deref())
            .is_some_and(|(want, got)| paths_match(want, got));

        if name_matches || path_matches {
            let cmd = proc_info
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            out.push(RunningProcess {
                pid: pid.as_u32(),
                exe,
                cmd,
            });
        }
    }
    out
}

pub fn is_pid_alive(pid: u32) -> bool {
    let Ok(mut sys) = System::new() else {
        return false;
    };
    sys.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(pid)]), true);
    sys.process(Pid::from_u32(pid)).is_some()
}

pub fn pick_target_process<'a>(
    running: &'a [RunningProcess],
    recorded_pid: Option<u32>,
    expected_kernel: Option<&Path>,
) -> Option<&'a RunningProcess> {
    if let Some(pid) = recorded_pid
        && let Some(found) = running.iter().find(|p| p.pid == pid)
    {
        return Some(found);
    }
    if let Some(expected) = expected_kernel
        && let Some(found) = running
            .iter()
            .find(|p| p.exe.as_deref().is_some_and(|e| paths_match(e, expected)))
    {
        return Some(found);
    }
    None
}

pub fn spawn_normal(exe: &Path, args: &[String]) -> Result<ManagedChild, String> {
    let cwd = exe.parent();
    let child = platform::spawn_hidden(exe, args, cwd)
        .map_err(|e| format!("failed to start {}: {e}", exe.display()))?;
    let pid = child.id();
    Ok(ManagedChild {
        child: Some(child),
        pid,
        elevated: platform::is_elevated(),
    })
}

pub fn terminate_pid_and_wait(pid: u32) -> Result<(), String> {
    let mut sys = System::new().map_err(|e| format!("failed to inspect process list: {e}"))?;
    let sys_pid = Pid::from_u32(pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[sys_pid]), true);
    let Some(proc_info) = sys.process(sys_pid) else {
        return Ok(());
    };
    if !proc_info.kill() {
        return Err(format!(
            "could not kill process {pid} (elevation may be required)"
        ));
    }

    let deadline = Instant::now() + STOP_WAIT_BUDGET;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(80));
        if !is_pid_alive(pid) {
            return Ok(());
        }
    }
    Err(format!("process {pid} did not exit within timeout"))
}

pub fn paths_match(a: &Path, b: &Path) -> bool {
    if let (Ok(ca), Ok(cb)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        return eq_path(&ca, &cb);
    }
    eq_path(a, b)
}

fn eq_path(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

/// Elevated helper entrypoint (`--elevate-stop <pid>`).
pub fn helper_stop_pid(pid: u32) -> i32 {
    if pid == 0 {
        return -1;
    }
    if process_exe_for_pid(pid).is_none() {
        return -2;
    }
    match terminate_pid_and_wait(pid) {
        Ok(()) => 0,
        Err(_) => -3,
    }
}

/// Elevated helper entrypoint (`--elevate-start <pid> <args...>` / `--elevate-replace <pid> <args...>`).
/// Reads the real executable image from `<pid>` so the caller never injects an arbitrary executable path.
pub fn helper_start_from_pid(pid: u32, args: &[String]) -> i32 {
    let Some(exe) = process_exe_for_pid(pid) else {
        return -2;
    };
    if terminate_pid_and_wait(pid).is_err() {
        return -3;
    }
    let cwd = exe.parent();
    match platform::spawn_hidden(&exe, args, cwd) {
        Ok(child) => child.id() as i32,
        Err(_) => -4,
    }
}

fn process_exe_for_pid(pid: u32) -> Option<PathBuf> {
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing()
            .with_processes(ProcessRefreshKind::nothing().with_exe(UpdateKind::Always)),
    )
    .ok()?;
    let sys_pid = Pid::from_u32(pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[sys_pid]), true);
    let proc_info = sys.process(sys_pid)?;
    let name = proc_info.name().to_string_lossy();
    let exe = proc_info.exe()?.to_path_buf();
    if platform::is_kernel_candidate_name(&name)
        && !name.to_ascii_lowercase().starts_with("mihomo-tray")
        && !name.to_ascii_lowercase().starts_with("mihomo_tray")
    {
        Some(exe)
    } else {
        None
    }
}
