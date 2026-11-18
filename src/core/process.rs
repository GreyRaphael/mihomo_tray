//! Portable process tracking (`.runtime.json` + `sysinfo 0.40`) and UAC helper entrypoints.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::config::tray;
use crate::core::discover;
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
}

#[derive(Debug)]
pub struct ManagedChild {
    pub child: Child,
    pub pid: u32,
}

pub fn runtime_record_path(home_dir: &Path) -> PathBuf {
    home_dir.join(RUNTIME_LOCK_NAME)
}

pub fn save_runtime_record(home_dir: &Path, record: &RuntimeRecord) {
    let path = runtime_record_path(home_dir);
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(
            "Failed to create runtime directory {}: {err}",
            parent.display()
        );
        return;
    }
    match serde_json::to_string_pretty(record) {
        Ok(json) => {
            if let Err(err) = std::fs::write(&path, json) {
                tracing::warn!("Failed to write {}: {err}", path.display());
            }
        }
        Err(err) => tracing::warn!("Failed to serialize runtime record: {err}"),
    }
}

pub fn load_runtime_record(home_dir: &Path) -> Option<RuntimeRecord> {
    let path = runtime_record_path(home_dir);
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn clear_runtime_record(home_dir: &Path) {
    let path = runtime_record_path(home_dir);
    if let Err(err) = std::fs::remove_file(&path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("Failed to remove {}: {err}", path.display());
    }
}

/// Reusable `sysinfo::System` wrapper so periodic process checks avoid reallocating buffers.
#[derive(Default)]
pub struct ProcessInspector {
    sys: Option<System>,
}

impl ProcessInspector {
    pub fn new() -> Self {
        Self {
            sys: System::new().ok(),
        }
    }

    fn system_mut(&mut self) -> Option<&mut System> {
        if self.sys.is_none() {
            self.sys = System::new().ok();
        }
        self.sys.as_mut()
    }

    pub fn list_running_mihomo(&mut self, expected_kernel: Option<&Path>) -> Vec<RunningProcess> {
        let Some(sys) = self.system_mut() else {
            return Vec::new();
        };
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_exe(UpdateKind::Always)
                .with_cmd(UpdateKind::Always),
        );

        let mut out = Vec::new();
        for (pid, proc_info) in sys.processes() {
            let name = proc_info.name().to_string_lossy();
            let exe = proc_info.exe().map(Path::to_path_buf);

            let name_matches = platform::is_kernel_candidate_name(&name);
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

    /// Checks whether `pid` is still alive and belongs to a mihomo kernel process
    /// (guarding against OS PID reuse after crashes or reboots).
    pub fn is_recorded_kernel_alive(&mut self, pid: u32, expected_kernel: Option<&Path>) -> bool {
        let Some(sys) = self.system_mut() else {
            return false;
        };
        let sys_pid = Pid::from_u32(pid);
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[sys_pid]),
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
        );
        let Some(proc_info) = sys.process(sys_pid) else {
            return false;
        };
        let name = proc_info.name().to_string_lossy();
        matches_recorded_kernel(&name, proc_info.exe(), expected_kernel)
    }

    pub fn is_pid_alive(&mut self, pid: u32) -> bool {
        let Some(sys) = self.system_mut() else {
            return false;
        };
        let sys_pid = Pid::from_u32(pid);
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[sys_pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        sys.process(sys_pid).is_some()
    }
}

fn matches_recorded_kernel(name: &str, exe: Option<&Path>, expected_kernel: Option<&Path>) -> bool {
    if let (Some(want), Some(got)) = (expected_kernel, exe) {
        return paths_match(want, got);
    }
    if platform::is_kernel_candidate_name(name) {
        return true;
    }
    expected_kernel
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|want_name| want_name.eq_ignore_ascii_case(name))
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
    Ok(ManagedChild { child, pid })
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
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[sys_pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        if sys.process(sys_pid).is_none() {
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

/// Elevated helper entrypoint (`--elevate-start <pid> <args...>`).
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
    let mut sys = System::new().ok()?;
    let sys_pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[sys_pid]),
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    let proc_info = sys.process(sys_pid)?;
    let name = proc_info.name().to_string_lossy();
    let exe = proc_info.exe()?.to_path_buf();
    is_authorized_helper_exe(&name, &exe, &platform::portable_root()).then_some(exe)
}

fn is_authorized_helper_exe(name: &str, exe: &Path, portable_root: &Path) -> bool {
    if platform::is_kernel_candidate_name(name) {
        return true;
    }
    let settings = tray::load(&tray::settings_path(portable_root)).unwrap_or_default();
    let declared = tray::resolve_portable_path(portable_root, &settings.mihomo.path);
    if declared
        .as_deref()
        .is_some_and(|path| paths_match(exe, path))
    {
        return true;
    }
    let (home_dir, _) =
        discover::resolve_home_and_config(portable_root, &settings, declared.as_deref());
    load_runtime_record(&home_dir).is_some_and(|rec| paths_match(exe, &rec.kernel_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_reused_non_kernel_pid_and_accepts_matching_kernel() {
        let mut inspector = ProcessInspector::new();
        assert!(!inspector.is_recorded_kernel_alive(std::process::id(), None));
        assert!(!matches_recorded_kernel(
            "notepad.exe",
            Some(Path::new(r"C:\Windows\System32\notepad.exe")),
            Some(Path::new(r"C:\Tools\mihomo\core\mihomo.exe")),
        ));
        assert!(!matches_recorded_kernel(
            "mihomo.exe",
            Some(Path::new(r"C:\Other\mihomo.exe")),
            Some(Path::new(r"C:\Tools\mihomo\core\mihomo.exe")),
        ));
        assert!(matches_recorded_kernel(
            "mihomo-windows-amd64",
            None,
            Some(Path::new("/opt/core/mihomo-windows-amd64")),
        ));
        assert!(!matches_recorded_kernel("mihomo-tray", None, None,));
    }

    #[test]
    fn deserializes_legacy_runtime_record_with_elevated_field() {
        let json = r#"{"pid":1234,"kernel_path":"/opt/mihomo","elevated":true}"#;
        let rec: RuntimeRecord = serde_json::from_str(json).expect("legacy record should parse");
        assert_eq!(rec.pid, 1234);
        assert_eq!(rec.kernel_path, PathBuf::from("/opt/mihomo"));
    }

    #[test]
    fn authorizes_custom_declared_and_recorded_helper_executables() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("mihomo_tray_helper_auth_{nanos}"));
        std::fs::create_dir_all(root.join("custom")).unwrap();
        let custom_exe = root.join("custom").join("my-core.exe");
        std::fs::write(&custom_exe, b"core").unwrap();

        std::fs::write(
            tray::settings_path(&root),
            "mihomo:\n  path: ./custom/my-core.exe\n",
        )
        .unwrap();
        assert!(is_authorized_helper_exe("my-core.exe", &custom_exe, &root));
        assert!(!is_authorized_helper_exe(
            "notepad.exe",
            Path::new("/Windows/System32/notepad.exe"),
            &root,
        ));

        // Also authorize via .runtime.json when tray.yml has no explicit mihomo.path.
        std::fs::remove_file(tray::settings_path(&root)).unwrap();
        let home_dir = root.join("config");
        save_runtime_record(
            &home_dir,
            &RuntimeRecord {
                pid: 99,
                kernel_path: custom_exe.clone(),
            },
        );
        assert!(is_authorized_helper_exe("my-core.exe", &custom_exe, &root));
        let _ = std::fs::remove_dir_all(&root);
    }
}
