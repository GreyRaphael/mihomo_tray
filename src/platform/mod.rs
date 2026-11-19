//! Platform boundary isolating Windows-specific integrations from cross-platform core logic.

use std::path::{Path, PathBuf};

#[cfg(not(windows))]
mod stub;
#[cfg(windows)]
mod win_dialog;
#[cfg(any(windows, test))]
mod win_helpers;
#[cfg(windows)]
mod windows;

#[cfg(not(windows))]
use stub as imp;
#[cfg(windows)]
use windows as imp;

pub const ELEVATE_START_FLAG: &str = "--elevate-start";
pub const ELEVATE_STOP_FLAG: &str = "--elevate-stop";
pub const AFTER_RESTART_FLAG: &str = "--after-restart";

/// Returns the directory containing the portable layout.
/// On Windows, this is the directory containing `mihomo-tray.exe`.
pub fn portable_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Default kernel executable filename for this OS (`mihomo.exe` on Windows, `mihomo` on Unix).
pub fn kernel_exe_name() -> &'static str {
    imp::kernel_exe_name()
}

/// Whether `file_name` matches a plausible mihomo kernel binary for auto-discovery,
/// excluding our own tray executable name.
pub fn is_kernel_candidate_name(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    if lower.starts_with("mihomo-tray") || lower.starts_with("mihomo_tray") {
        return false;
    }
    imp::is_kernel_candidate_name(&lower)
}

/// Expands OS environment variables (`%VAR%` on Windows).
pub fn expand_env(value: &str) -> String {
    imp::expand_env(value)
}

/// Resolves Scoop `.shim` target on Windows; returns `None` elsewhere or if not a shim.
pub fn resolve_shim_target(path: &Path) -> Option<PathBuf> {
    imp::resolve_shim_target(path)
}

/// Reads whether the OS system proxy is currently enabled.
pub fn proxy_is_enabled() -> bool {
    imp::proxy_is_enabled()
}

/// Enables the OS system proxy pointing to `127.0.0.1:<port>`.
pub fn proxy_enable(port: u16, extra_bypass: &[String]) -> Result<(), String> {
    imp::proxy_enable(port, extra_bypass)
}

/// Disables the OS system proxy.
pub fn proxy_disable() -> Result<(), String> {
    imp::proxy_disable()
}

/// Checks whether launch-at-login is currently registered for this portable executable.
pub fn autostart_is_enabled() -> bool {
    build_auto_launch()
        .and_then(|al| al.is_enabled().map_err(|e| e.to_string()))
        .unwrap_or(false)
}

/// Enables or disables launch-at-login for this portable executable.
pub fn autostart_set(enabled: bool) -> Result<(), String> {
    let al = build_auto_launch()?;
    if enabled {
        al.enable()
            .map_err(|e| format!("failed to enable startup entry: {e}"))
    } else {
        al.disable()
            .map_err(|e| format!("failed to disable startup entry: {e}"))
    }
}

fn build_auto_launch() -> Result<auto_launch::AutoLaunch, String> {
    let exe = std::env::current_exe().map_err(|e| format!("failed to get executable path: {e}"))?;
    let exe_str = exe.to_string_lossy().into_owned();
    auto_launch::AutoLaunchBuilder::new()
        .set_app_name(crate::text::APP_NAME)
        .set_app_path(&exe_str)
        .build()
        .map_err(|e| format!("failed to initialize auto-launch: {e}"))
}

/// Spawns a detached background child process without creating a console window.
pub fn spawn_hidden(
    exe: &Path,
    args: &[String],
    cwd: Option<&Path>,
) -> std::io::Result<std::process::Child> {
    imp::spawn_hidden(exe, args, cwd)
}

/// Runs an elevated self-helper (`--elevate-*`) and returns its exit code.
pub fn run_elevated_self(args: &[String]) -> Result<i32, String> {
    imp::run_elevated_self(args)
}

/// Displays a blocking modal notice/error box to the user on desktop platforms.
pub fn notify_error(title: &str, message: &str) {
    tracing::error!("{title}: {message}");
    if !cfg!(test) {
        imp::notify_error(title, message);
    }
}

/// Displays a blocking modal informational box to the user on desktop platforms.
pub fn notify_info(title: &str, message: &str) {
    tracing::info!("{title}: {message}");
    if !cfg!(test) {
        imp::notify_info(title, message);
    }
}

/// Displays a blocking modal confirmation (Yes/No) returning `true` only on Yes.
pub fn confirm_yes_no(title: &str, message: &str) -> bool {
    if cfg!(test) {
        return false;
    }
    imp::confirm_yes_no(title, message)
}

/// Writes UTF-8 text to the OS clipboard.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("failed to access clipboard: {e}"))?;
    clipboard
        .set_text(text)
        .map_err(|e| format!("failed to copy text to clipboard: {e}"))
}

/// Formats shell commands that configure HTTP/HTTPS/ALL proxy environment variables.
pub fn format_env_vars_command(port: u16) -> String {
    imp::format_env_vars_command(port)
}

/// Spawns a fresh instance of the current executable detached from this process.
pub fn spawn_detached_self(args: &[String]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("failed to locate self: {e}"))?;
    imp::spawn_hidden(&exe, args, None)
        .map(|_| ())
        .map_err(|e| format!("failed to relaunch application: {e}"))
}

/// Whether a native tray popup menu is currently open on the calling UI thread.
pub fn is_tray_menu_open() -> bool {
    imp::is_tray_menu_open()
}

/// Prompts the user with a simple modal text input box for a subscription URL.
pub fn prompt_subscription_url(current: Option<&str>) -> Option<String> {
    if cfg!(test) {
        return None;
    }

    imp::prompt_inputs(
        crate::text::PROMPT_SUBSCRIPTION_TITLE,
        &[(crate::text::PROMPT_SUBSCRIPTION_HINT, current.unwrap_or(""))],
    )
    .and_then(|mut values| values.pop())
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}
