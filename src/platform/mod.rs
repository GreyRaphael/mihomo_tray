//! Platform boundary isolating Windows-specific integrations from cross-platform core logic.

use std::path::{Path, PathBuf};

#[cfg(not(windows))]
mod stub;
#[cfg(windows)]
mod windows;

#[cfg(not(windows))]
use stub as imp;
#[cfg(windows)]
use windows as imp;

pub const ELEVATE_START_FLAG: &str = "--elevate-start";
pub const ELEVATE_STOP_FLAG: &str = "--elevate-stop";
pub const ELEVATE_REPLACE_FLAG: &str = "--elevate-replace";

/// Returns the directory containing the portable layout.
/// On Windows, this is the directory containing `mihomo-tray.exe`.
pub fn portable_root() -> PathBuf {
    imp::portable_root()
}

/// Default kernel executable filename for this OS (`mihomo.exe` on Windows, `mihomo` on Unix).
pub fn kernel_exe_name() -> &'static str {
    imp::kernel_exe_name()
}

/// Whether `file_name` matches a plausible mihomo kernel binary for auto-discovery.
pub fn is_kernel_candidate_name(file_name: &str) -> bool {
    imp::is_kernel_candidate_name(file_name)
}

/// Expands OS environment variables (`%VAR%` on Windows).
pub fn expand_env(value: &str) -> String {
    imp::expand_env(value)
}

/// Resolves Scoop `.shim` target on Windows; returns `None` elsewhere or if not a shim.
pub fn resolve_shim_target(path: &Path) -> Option<PathBuf> {
    imp::resolve_shim_target(path)
}

/// Whether the current process holds elevated administrator privileges.
pub fn is_elevated() -> bool {
    imp::is_elevated()
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
        .set_app_name("mihomo-tray")
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

/// Checks CLI arguments for internal elevated helper switches before GUI startup.
pub fn try_run_helper(
    stop_pid: fn(u32) -> i32,
    start_from_pid: fn(u32, &[String]) -> i32,
) -> Option<i32> {
    let args: Vec<String> = std::env::args().collect();
    let flag = args.get(1)?.as_str();
    match flag {
        ELEVATE_STOP_FLAG => {
            let pid = args.get(2).and_then(|s| s.parse::<u32>().ok())?;
            Some(stop_pid(pid))
        }
        ELEVATE_START_FLAG | ELEVATE_REPLACE_FLAG => {
            let pid = args.get(2).and_then(|s| s.parse::<u32>().ok())?;
            Some(start_from_pid(pid, &args[3..]))
        }
        _ => None,
    }
}

/// Displays a blocking modal notice/error box to the user on desktop platforms.
pub fn notify_error(title: &str, message: &str) {
    imp::notify_error(title, message);
}

/// Displays a blocking modal confirmation (Yes/No) returning `true` only on Yes.
pub fn confirm_yes_no(title: &str, message: &str) -> bool {
    imp::confirm_yes_no(title, message)
}

/// Platform hook invoked right after the main event loop initializes.
pub fn after_event_loop_ready() {
    imp::after_event_loop_ready();
}

/// Whether a native tray popup menu is currently open on the calling UI thread.
pub fn is_tray_menu_open() -> bool {
    imp::is_tray_menu_open()
}

/// Prompts the user with a simple modal text input box for a subscription URL.
pub fn prompt_subscription_url(current: Option<&str>) -> Option<String> {
    imp::prompt_subscription_url(current)
}

/// Quotes arguments following Microsoft `CommandLineToArgvW` rules so paths with spaces,
/// backslashes, and UNC prefixes survive `ShellExecuteExW` verbatim.
#[cfg(any(windows, test))]
pub fn quote_win32_args(args: &[String]) -> String {
    let mut out = String::new();
    for (idx, arg) in args.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        if !arg.is_empty() && !arg.chars().any(|c| matches!(c, ' ' | '\t' | '"')) {
            out.push_str(arg);
            continue;
        }
        out.push('"');
        let mut backslashes = 0usize;
        for ch in arg.chars() {
            if ch == '\\' {
                backslashes += 1;
            } else if ch == '"' {
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            } else {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(ch);
            }
        }
        for _ in 0..(backslashes * 2) {
            out.push('\\');
        }
        out.push('"');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_win32_arguments_without_corrupting_backslashes() {
        let args = vec![
            "--elevate-start".to_string(),
            "1234".to_string(),
            "-d".to_string(),
            r"C:\My Portable Tools\mihomo tray\config".to_string(),
            r"\\server\share name\config.yaml".to_string(),
            r"C:\Trailing Space\".to_string(),
            String::new(),
        ];
        let joined = quote_win32_args(&args);
        assert_eq!(
            joined,
            r#"--elevate-start 1234 -d "C:\My Portable Tools\mihomo tray\config" "\\server\share name\config.yaml" "C:\Trailing Space\\" """#
        );
    }
}
