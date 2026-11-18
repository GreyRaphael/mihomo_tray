//! Platform boundary isolating Windows-specific integrations from cross-platform core logic.

use std::path::{Path, PathBuf};

#[cfg(not(windows))]
mod stub;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_prompt;

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
            let Some(pid) = args.get(2).and_then(|s| s.parse::<u32>().ok()) else {
                return Some(-1);
            };
            Some(stop_pid(pid))
        }
        ELEVATE_START_FLAG => {
            let Some(pid) = args.get(2).and_then(|s| s.parse::<u32>().ok()) else {
                return Some(-1);
            };
            Some(start_from_pid(pid, &args[3..]))
        }
        _ => None,
    }
}

/// Displays a blocking modal notice/error box to the user on desktop platforms.
pub fn notify_error(title: &str, message: &str) {
    imp::notify_error(title, message);
}

/// Displays a blocking modal informational box to the user on desktop platforms.
pub fn notify_info(title: &str, message: &str) {
    imp::notify_info(title, message);
}

/// Displays a blocking modal confirmation (Yes/No) returning `true` only on Yes.
pub fn confirm_yes_no(title: &str, message: &str) -> bool {
    imp::confirm_yes_no(title, message)
}

/// Writes UTF-8 text to the OS clipboard.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    imp::copy_to_clipboard(text)
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
    imp::prompt_subscription_url(current)
}

/// Focuses an already-open subscription prompt window if one exists.
pub fn focus_subscription_prompt() {
    imp::focus_subscription_prompt();
}

/// Requests an open subscription prompt window to close cleanly (used during app shutdown).
pub fn close_subscription_prompt() {
    imp::close_subscription_prompt();
}

/// Extracts the target executable path from a Scoop `.shim` file (`path = "..."`).
#[cfg(any(windows, test))]
pub fn parse_shim_path(content: &str) -> Option<PathBuf> {
    content
        .strip_prefix('\u{feff}')
        .unwrap_or(content)
        .lines()
        .find_map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') || trimmed.is_empty() {
                return None;
            }
            let (k, v) = trimmed.split_once('=')?;
            if !k.trim().eq_ignore_ascii_case("path") {
                return None;
            }
            let val = v.trim_start();
            let raw_path = if let Some(quoted) = val.strip_prefix('"') {
                quoted.split('"').next()?
            } else {
                val.split('#').next()?.trim()
            };
            (!raw_path.is_empty()).then(|| PathBuf::from(raw_path))
        })
}

/// Computes an updated Windows `ProxyOverride` value if needed.
/// - When `extra_bypass` is empty and an entry already exists, returns `None` to preserve user settings.
/// - When `extra_bypass` is non-empty (or no entry exists yet), merges deduplicated entries and ensures `<local>` is included.
#[cfg(any(windows, test))]
pub fn merge_proxy_override(existing: Option<&str>, extra_bypass: &[String]) -> Option<String> {
    let extras: Vec<&str> = extra_bypass
        .iter()
        .flat_map(|item| item.split(';'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    if extras.is_empty() {
        return existing.is_none().then(|| "<local>".to_string());
    }

    let mut entries: Vec<String> = Vec::new();
    let mut push_unique = |entry: &str| {
        if entry.eq_ignore_ascii_case("<local>") {
            return;
        }
        if !entries.iter().any(|e| e.eq_ignore_ascii_case(entry)) {
            entries.push(entry.to_string());
        }
    };

    if let Some(raw) = existing {
        for part in raw.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            push_unique(part);
        }
    }
    for part in extras {
        push_unique(part);
    }
    entries.push("<local>".to_string());

    let merged = entries.join(";");
    if existing == Some(merged.as_str()) {
        None
    } else {
        Some(merged)
    }
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

    #[test]
    fn merges_proxy_override_only_when_seeding_or_bypass_configured() {
        assert_eq!(merge_proxy_override(None, &[]), Some("<local>".to_string()));
        assert_eq!(
            merge_proxy_override(Some("localhost;127.*;<local>"), &[]),
            None
        );
        assert_eq!(
            merge_proxy_override(
                Some("localhost;127.*;<local>"),
                &[" 10.* ".to_string(), "*.corp;127.*".to_string()]
            ),
            Some("localhost;127.*;10.*;*.corp;<local>".to_string())
        );
        assert_eq!(
            merge_proxy_override(
                Some("localhost;127.*;10.*;*.corp;<local>"),
                &["10.*".to_string(), "*.corp".to_string()]
            ),
            None
        );
    }

    #[test]
    fn parses_scoop_shim_paths_preserving_hash_inside_quotes() {
        let content = "\u{feff}# scoop shim\npath = \"C:\\Users\\me#1\\scoop\\mihomo.exe\" # note\nargs = -d .\n";
        assert_eq!(
            parse_shim_path(content),
            Some(PathBuf::from(r"C:\Users\me#1\scoop\mihomo.exe"))
        );
        assert_eq!(
            parse_shim_path("path = C:\\scoop\\mihomo.exe # comment\n"),
            Some(PathBuf::from(r"C:\scoop\mihomo.exe"))
        );
    }
}
