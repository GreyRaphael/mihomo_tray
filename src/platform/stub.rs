//! Non-Windows host scaffold (enables Linux host testing and future Unix ports).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

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

pub fn format_env_vars_command(port: u16) -> String {
    format!(
        "export http_proxy=http://127.0.0.1:{port} https_proxy=http://127.0.0.1:{port} all_proxy=socks5://127.0.0.1:{port}"
    )
}

pub fn is_tray_menu_open() -> bool {
    false
}
