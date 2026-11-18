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

pub const HELPER_EXIT_INVALID_ARGS: i32 = -1;
const HELPER_EXIT_UNAUTHORIZED: i32 = -2;
const HELPER_EXIT_STOP_FAILED: i32 = -3;
const HELPER_EXIT_SPAWN_FAILED: i32 = -4;
const HELPER_EXIT_PID_OVERFLOW: i32 = -5;

/// Translates an internal elevated helper exit code into a concise diagnostic phrase.
pub fn helper_exit_reason(code: i32) -> Option<&'static str> {
    match code {
        HELPER_EXIT_INVALID_ARGS => Some("invalid helper arguments"),
        HELPER_EXIT_UNAUTHORIZED => {
            Some("target process is missing or not an authorized mihomo binary")
        }
        HELPER_EXIT_STOP_FAILED => Some("failed to terminate existing mihomo process"),
        HELPER_EXIT_SPAWN_FAILED => Some("failed to launch elevated mihomo executable"),
        HELPER_EXIT_PID_OVERFLOW => Some("spawned PID exceeds supported i32 range"),
        _ => None,
    }
}

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
}

impl ManagedChild {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

fn runtime_record_path(home_dir: &Path) -> PathBuf {
    home_dir.join(RUNTIME_LOCK_NAME)
}

pub fn save_runtime_record(home_dir: &Path, record: &RuntimeRecord) {
    crate::config::write_json_pretty(&runtime_record_path(home_dir), record);
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

        let expected_matcher = expected_kernel.map(ExpectedKernelMatcher::new);
        let self_pid = std::process::id();
        let mut out = Vec::new();
        for (pid, proc_info) in sys.processes() {
            if pid.as_u32() == self_pid || proc_info.thread_kind().is_some() {
                continue;
            }
            let name = proc_info.name().to_string_lossy();
            let exe = proc_info.exe().map(Path::to_path_buf);

            let matches = platform::is_kernel_candidate_name(&name)
                || expected_matcher
                    .as_ref()
                    .zip(exe.as_deref())
                    .is_some_and(|(matcher, got)| matcher.matches(got));

            if matches {
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
        if proc_info.thread_kind().is_some() {
            return false;
        }
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
    Ok(ManagedChild { child })
}

pub fn terminate_pid_and_wait(pid: u32) -> Result<(), String> {
    let mut sys = System::new().map_err(|e| format!("failed to inspect process list: {e}"))?;
    let sys_pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[sys_pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
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

struct ExpectedKernelMatcher {
    raw: PathBuf,
    canonical: Option<PathBuf>,
}

impl ExpectedKernelMatcher {
    fn new(raw: &Path) -> Self {
        Self {
            raw: raw.to_path_buf(),
            canonical: std::fs::canonicalize(raw).ok(),
        }
    }

    fn matches(&self, exe: &Path) -> bool {
        if eq_path(&self.raw, exe)
            || self
                .canonical
                .as_deref()
                .is_some_and(|canon| eq_path(canon, exe))
        {
            return true;
        }
        let Some(got_name) = exe.file_name() else {
            return false;
        };
        let same_filename = [
            self.raw.file_name(),
            self.canonical.as_deref().and_then(Path::file_name),
        ]
        .into_iter()
        .flatten()
        .any(|want_name| eq_path(Path::new(want_name), Path::new(got_name)));
        if !same_filename {
            return false;
        }
        match (self.canonical.as_deref(), std::fs::canonicalize(exe)) {
            (Some(ca), Ok(cb)) => eq_path(ca, &cb),
            _ => false,
        }
    }
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
/// Failures are reported via negative exit codes (translated by [`helper_exit_reason`]
/// in the main process) instead of initializing log rotation in the transient child.
pub fn helper_stop_pid(pid: u32) -> i32 {
    if pid == 0 {
        return HELPER_EXIT_INVALID_ARGS;
    }
    if process_exe_for_pid(pid).is_none() {
        return HELPER_EXIT_UNAUTHORIZED;
    }
    match terminate_pid_and_wait(pid) {
        Ok(()) => 0,
        Err(_) => HELPER_EXIT_STOP_FAILED,
    }
}

/// Elevated helper entrypoint (`--elevate-start <pid> <args...>`).
/// Reads the real executable image from `<pid>` so the caller never injects an arbitrary executable path.
pub fn helper_start_from_pid(pid: u32, args: &[String]) -> i32 {
    if pid == 0 {
        return HELPER_EXIT_INVALID_ARGS;
    }
    let Some(exe) = process_exe_for_pid(pid) else {
        return HELPER_EXIT_UNAUTHORIZED;
    };
    if terminate_pid_and_wait(pid).is_err() {
        return HELPER_EXIT_STOP_FAILED;
    }
    let cwd = exe.parent();
    match platform::spawn_hidden(&exe, args, cwd) {
        Ok(child) => match i32::try_from(child.id()) {
            Ok(id) if id > 0 => id,
            _ => HELPER_EXIT_PID_OVERFLOW,
        },
        Err(_) => HELPER_EXIT_SPAWN_FAILED,
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
    let settings = tray::load(&tray::settings_path(portable_root)).unwrap_or_default();
    let declared = tray::resolve_portable_path(portable_root, &settings.mihomo.path);
    let discovered = declared.or_else(|| {
        discover::find_kernel(portable_root, &settings)
            .ok()
            .flatten()
    });
    if discovered
        .as_deref()
        .is_some_and(|path| paths_match(exe, path))
    {
        return true;
    }
    let (home_dir, _) =
        discover::resolve_home_and_config(portable_root, &settings, discovered.as_deref());
    if load_runtime_record(&home_dir).is_some_and(|rec| paths_match(exe, &rec.kernel_path)) {
        return true;
    }
    platform::is_kernel_candidate_name(name) && is_under_dir(exe, portable_root)
}

fn is_under_dir(path: &Path, dir: &Path) -> bool {
    match (std::fs::canonicalize(path), std::fs::canonicalize(dir)) {
        (Ok(cp), Ok(cd)) => cp.starts_with(&cd),
        _ => path.starts_with(dir),
    }
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
        let root = crate::config::test_temp_dir("helper_auth");
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
        let portable_kernel = root.join("core").join(platform::kernel_exe_name());
        std::fs::create_dir_all(portable_kernel.parent().unwrap()).unwrap();
        std::fs::write(&portable_kernel, b"core").unwrap();
        assert!(is_authorized_helper_exe(
            platform::kernel_exe_name(),
            &portable_kernel,
            &root
        ));
        assert!(!is_authorized_helper_exe(
            platform::kernel_exe_name(),
            Path::new("/tmp/untrusted/mihomo-evil"),
            &root
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn expected_kernel_matcher_handles_canonical_and_unrelated_paths() {
        let root = crate::config::test_temp_dir("kernel_matcher");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let real_exe = sub.join("custom-proxy");
        std::fs::write(&real_exe, b"bin").unwrap();

        let unnormalized = root.join("sub").join("..").join("sub").join("custom-proxy");
        let matcher = ExpectedKernelMatcher::new(&unnormalized);
        assert!(matcher.matches(&real_exe));
        assert!(matcher.matches(&unnormalized));
        assert!(!matcher.matches(&root.join("sub").join("other-proc")));

        #[cfg(unix)]
        {
            let link = root.join("proxy-link");
            std::os::unix::fs::symlink(&real_exe, &link).unwrap();
            let link_matcher = ExpectedKernelMatcher::new(&link);
            assert!(link_matcher.matches(&real_exe));
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn helper_entrypoints_validate_pid_zero_and_describe_exit_codes() {
        assert_eq!(helper_stop_pid(0), HELPER_EXIT_INVALID_ARGS);
        assert_eq!(helper_start_from_pid(0, &[]), HELPER_EXIT_INVALID_ARGS);
        for code in [
            HELPER_EXIT_INVALID_ARGS,
            HELPER_EXIT_UNAUTHORIZED,
            HELPER_EXIT_STOP_FAILED,
            HELPER_EXIT_SPAWN_FAILED,
            HELPER_EXIT_PID_OVERFLOW,
        ] {
            assert!(helper_exit_reason(code).is_some());
        }
        assert_eq!(helper_exit_reason(0), None);
    }
}
