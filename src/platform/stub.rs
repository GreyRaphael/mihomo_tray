//! Non-Windows host scaffold (enables Linux host testing and future Unix ports).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub fn kernel_exe_name() -> &'static str {
    "mihomo"
}

pub fn is_kernel_candidate_name(lower_name: &str) -> bool {
    lower_name == "mihomo" || lower_name.starts_with("mihomo-") || lower_name.starts_with("mihomo_")
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

/// Guards `rustydialogs`'s Linux backend `LazyLock`, which panics if no
/// `kdialog`/`zenity` executable is available or when running headless.
pub fn can_show_dialog() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(not(target_os = "macos"))]
    {
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return false;
        }
        if let Ok(backend) = std::env::var("RUSTY_DIALOGS_BACKEND") {
            return matches!(backend.as_str(), "kdialog" | "zenity")
                && which::which(&backend).is_ok();
        }
        which::which("kdialog").is_ok() || which::which("zenity").is_ok()
    }
}

pub fn with_dialog_icon<T>(f: impl FnOnce() -> T) -> T {
    f()
}
