//! Non-Windows host scaffold (enables Linux host testing and future Unix ports).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub fn portable_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn kernel_exe_name() -> &'static str {
    "mihomo"
}

pub fn is_kernel_candidate_name(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower == "mihomo" || lower.starts_with("mihomo-") || lower.starts_with("mihomo_")
}

pub fn expand_env(value: &str) -> String {
    value.to_string()
}

pub fn resolve_shim_target(_path: &Path) -> Option<PathBuf> {
    None
}

pub fn is_elevated() -> bool {
    false
}

pub fn proxy_is_enabled() -> bool {
    false
}

pub fn proxy_enable(_port: u16, _extra_bypass: &[String]) -> Result<(), String> {
    Err("system proxy is not yet implemented for this OS".to_string())
}

pub fn proxy_disable() -> Result<(), String> {
    Err("system proxy is not yet implemented for this OS".to_string())
}

pub fn spawn_hidden(exe: &Path, args: &[String], cwd: Option<&Path>) -> std::io::Result<Child> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.spawn()
}

pub fn run_elevated_self(_args: &[String]) -> Result<i32, String> {
    Err("elevated self-helper is Windows-only".to_string())
}

pub fn notify_error(title: &str, message: &str) {
    tracing::error!("{title}: {message}");
}

pub fn confirm_yes_no(_title: &str, _message: &str) -> bool {
    false
}

pub fn after_event_loop_ready() {}
