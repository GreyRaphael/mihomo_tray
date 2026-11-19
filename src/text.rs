//! Static English UI labels, status strings, and formatted error messages.

pub const APP_NAME: &str = "mihomo-tray";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const TRAY_FILE_NAME: &str = "tray.yml";
pub const MIHOMO_CONFIG_NAME: &str = "config.yaml";
pub const RUNTIME_LOCK_NAME: &str = ".runtime.json";
pub const STATE_FILE_NAME: &str = ".tray-state.json";

pub const MENU_OPEN_WEB_UI: &str = "Open WebUI";
pub const MENU_SET_SUBSCRIPTION: &str = "Set Subscription URL";
pub const MENU_MODE_RULE: &str = "Rule";
pub const MENU_MODE_GLOBAL: &str = "Global";
pub const MENU_MODE_DIRECT: &str = "Direct";
pub const MENU_SYSTEM_PROXY: &str = "System Proxy";
pub const MENU_TUN_MODE: &str = "TUN Mode";
pub const MENU_PROXY_GROUPS: &str = "Proxy Groups";
pub const MENU_TEST_LATENCY: &str = "Test Group Latency";
pub const MENU_AUTO_UNPIN: &str = "Auto (Unpin)";
pub const MENU_OPEN: &str = "Open";
pub const MENU_OPEN_TRAY_CONFIG: &str = "Open tray.yml";
pub const MENU_OPEN_MIHOMO_CONFIG: &str = "Open config.yaml";
pub const MENU_OPEN_PORTABLE_DIR: &str = "Open Portable Folder";
pub const MENU_OPEN_UI_DIR: &str = "Open WebUI Folder (ui/)";
pub const MENU_OPEN_LOG_FILE: &str = "Open Log File";
pub const MENU_MORE: &str = "More";
pub const MENU_COPY_ENV_VARS: &str = "Copy Environment Variables";
const MENU_REFRESH_SUBSCRIPTION: &str = "Update Subscription";
pub const MENU_RESTART_CORE: &str = "Restart Mihomo Core";
pub const MENU_RESTART_APP: &str = "Restart Application";
pub const MENU_AUTOSTART: &str = "Auto Start";
pub const MENU_QUIT: &str = "Quit";

pub const STATUS_STARTING_CORE: &str = "Starting mihomo core...";
pub const STATUS_WAITING_CONTROLLER: &str = "Waiting for mihomo controller...";
pub const STATUS_RESTARTING_CORE: &str = "Restarting mihomo core...";
pub const STATUS_ELEVATING_CORE: &str = "Waiting for elevation prompt...";
pub const STATUS_REFRESHING_SUBSCRIPTION: &str = "Refreshing subscription...";
pub const STATUS_UPDATING_SUBSCRIPTION: &str = "Updating subscription...";

pub const ERR_ALREADY_RUNNING: &str =
    "mihomo-tray is already running.\nCheck the system tray overflow area if the icon is hidden.";
pub const ERR_CORE_NOT_FOUND: &str = "No mihomo binary found beside mihomo-tray, in ./core, ./bin, or on PATH. Place mihomo.exe in ./core or set mihomo.path in tray.yml.";
pub const ERR_NO_CONTROLLER: &str = "No controller address found. Configure external-controller in config.yaml or controller.address in tray.yml.";
pub const ERR_CORE_STOPPED_UNEXPECTEDLY: &str = "mihomo exited unexpectedly.";
pub const ERR_CORE_NOT_RESPONDING: &str =
    "mihomo started, but its controller did not respond in time.";
pub const ERR_CORE_RESTART_TIMEOUT: &str =
    "mihomo restarted, but its controller did not respond in time.";
pub const ERR_TUN_ELEVATION_DECLINED: &str =
    "TUN mode requires administrator privileges (elevation prompt cancelled or failed).";
pub const ERR_TUN_FAILED_AFTER_ELEVATION: &str = "TUN mode remained disabled even after restarting mihomo elevated. Check config.yaml and Wintun availability.";
pub const ERR_STOP_ELEVATION_REQUIRED: &str =
    "The running mihomo instance requires administrator privileges to stop.";
pub const ERR_NO_MIXED_PORT_FOR_ENV: &str =
    "No mixed-port/port found in running core or config.yaml.";
pub const ERR_NO_MIXED_PORT_FOR_PROXY: &str =
    "mihomo mixed-port/port is 0 or not configured; cannot enable system proxy.";
pub const ERR_NO_MIXED_PORT_FOR_RESTORE: &str =
    "mihomo mixed-port/port is 0 or not configured; cannot restore system proxy.";
pub const ERR_LOCATE_ELEVATE_PROCESS: &str =
    "Could not locate the running mihomo process to elevate.";
pub const ERR_MULTIPLE_EXTERNAL_CORES: &str =
    "Multiple external mihomo processes are running; stop them before restarting.";
pub const ERR_NO_SUBSCRIPTION_PROVIDER: &str =
    "No subscription provider configured. Use Set Subscription URL first.";
pub const ERR_SUBSCRIPTION_TASK_BUSY: &str =
    "A subscription update is already in progress; try again shortly.";
pub const ERR_INVALID_PROFILE_NAME: &str =
    "Profile name cannot be empty or contain invalid filename characters.";
pub const ERR_INVALID_SUBSCRIPTION_URL: &str =
    "Subscription URL must start with http:// or https://";
pub const ERR_WAIT_INTERRUPTED_FOR_SHUTDOWN: &str =
    "Shutdown requested while waiting for controller.";
pub const ERR_TUN_STILL_ENABLED: &str = "TUN mode remained enabled after disabling.";
pub const ERR_UNSUPPORTED_PROXY_MODE: &str = "unsupported proxy mode";
pub const ERR_CONFLICTING_EXT_UI_ARG: &str =
    "Do not pass -ext-ui in mihomo.args when ui.external_ui is configured in tray.yml.";

pub const TITLE_VERSION: &str = "Version";
pub const TITLE_CONFIRM_STOP: &str = "Confirm Stop Core";
pub const TITLE_QUIT_ANYWAY: &str = "Core Still Running";

pub const PROMPT_SUBSCRIPTION_TITLE: &str = "Set Subscription URL";
pub const PROMPT_SUBSCRIPTION_HINT: &str =
    "Enter your subscription link (auto-updates every 12 hours):";

pub fn format_confirm_quit_anyway(err: &str) -> String {
    format!(
        "Could not stop the running mihomo core:\n\n{err}\n\nExit mihomo-tray anyway? (System Proxy will be disabled)"
    )
}

pub fn format_confirm_stop_core(pid: u32, exe: Option<&std::path::Path>) -> String {
    let exe_label = exe
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<elevated or unreadable path>".to_string());
    format!("Stop running mihomo process?\n\nPID: {pid}\nExecutable: {exe_label}")
}

pub fn format_version_dialog(
    core_version: &str,
    portable_root: &std::path::Path,
    config_path: Option<&std::path::Path>,
) -> String {
    let core_line = if core_version.is_empty() {
        "not running"
    } else {
        core_version
    };
    let cfg_line = config_path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "none".to_string());
    format!(
        "{APP_NAME} v{APP_VERSION}\nMihomo Core: {core_line}\nPortable Folder: {portable}\nConfig File: {cfg_line}",
        portable = portable_root.display(),
    )
}

pub fn format_open_target_failed(target: &str, err: &dyn std::fmt::Display) -> String {
    format!("Failed to open '{target}': {err}")
}

pub fn format_elevated_helper_failed(code: i32, reason: Option<&str>) -> String {
    match reason {
        Some(detail) => {
            format!("Elevated helper failed while restarting mihomo (code {code}: {detail}).")
        }
        None => format!("Elevated helper failed while restarting mihomo (code {code})."),
    }
}

pub fn format_tun_enable_after_elevation_failed(err: &str) -> String {
    format!("Failed to enable TUN after elevation: {err}")
}

pub fn format_declared_path_missing(path: &std::path::Path) -> String {
    format!("Declared mihomo.path does not exist: {}", path.display())
}

pub fn format_disallowed_mihomo_arg(flag: &str, owner: &str) -> String {
    format!("Do not pass -{flag} in mihomo.args; configure {owner} instead.")
}

pub fn format_unsupported_controller_family(config_path: &std::path::Path) -> String {
    format!(
        "{} configures only TLS/unix/pipe external-controller, which is unsupported.",
        config_path.display()
    )
}

pub fn format_event_loop_init_failed(err: &dyn std::fmt::Display) -> String {
    format!("Failed to initialize event loop: {err}")
}

pub fn format_single_instance_failed(err: &dyn std::fmt::Display) -> String {
    format!("Single-instance check failed: {err}")
}

pub fn format_status_running(version: &str, mixed_port: u16) -> String {
    let version = version.trim();
    let prefix = if version.is_empty() {
        "Mihomo".to_string()
    } else {
        format!("Mihomo {version}")
    };
    if mixed_port > 0 {
        format!("{prefix} Running({mixed_port})")
    } else {
        format!("{prefix} Running")
    }
}

pub fn format_status_unreachable(running: bool) -> &'static str {
    if running {
        "Mihomo: Controller Unreachable"
    } else {
        "Mihomo: Stopped"
    }
}

pub fn format_group_label(name: &str, pinned: bool) -> String {
    if pinned {
        format!("{name} · Pinned")
    } else {
        name.to_string()
    }
}

pub fn format_node_label(name: &str, delay: Option<u32>) -> String {
    match delay {
        Some(0) => format!("{name} (timeout)"),
        Some(ms) => format!("{name} ({ms}ms)"),
        None => name.to_string(),
    }
}

pub fn format_page_label(start: usize, end: usize) -> String {
    format!("{start}-{end}")
}

pub fn format_version_label(version: &str) -> String {
    format!("Version: v{version}")
}

/// Coarse relative age bucket for `Update Subscription` so tray menus only
/// rebuild when the displayed label actually changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubAgeBucket {
    None,
    JustNow,
    Minutes(i64),
    Hours(i64),
    Days(i64),
}

pub fn subscription_age_bucket(updated_at: Option<i64>, now_unix: i64) -> SubAgeBucket {
    let Some(ts) = updated_at else {
        return SubAgeBucket::None;
    };
    let elapsed = now_unix.saturating_sub(ts);
    if elapsed < 60 {
        SubAgeBucket::JustNow
    } else if elapsed < 3_600 {
        SubAgeBucket::Minutes(elapsed / 60)
    } else if elapsed < 86_400 {
        SubAgeBucket::Hours(elapsed / 3_600)
    } else {
        SubAgeBucket::Days(elapsed / 86_400)
    }
}

/// Formats `Update Subscription` with a coarse relative age bucket so tray
/// menus only rebuild when the displayed minute/hour/day label changes.
pub fn format_update_subscription_label(updated_at: Option<i64>, now_unix: i64) -> String {
    match subscription_age_bucket(updated_at, now_unix) {
        SubAgeBucket::None => MENU_REFRESH_SUBSCRIPTION.to_string(),
        SubAgeBucket::JustNow => format!("{MENU_REFRESH_SUBSCRIPTION} (just now)"),
        SubAgeBucket::Minutes(m) => format!("{MENU_REFRESH_SUBSCRIPTION} ({m}m ago)"),
        SubAgeBucket::Hours(h) => format!("{MENU_REFRESH_SUBSCRIPTION} ({h}h ago)"),
        SubAgeBucket::Days(d) => format!("{MENU_REFRESH_SUBSCRIPTION} ({d}d ago)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_status_running_with_version_and_port() {
        assert_eq!(
            format_status_running("v1.19.34", 7890),
            "Mihomo v1.19.34 Running(7890)"
        );
        assert_eq!(
            format_status_running(" v1.19.34 ", 0),
            "Mihomo v1.19.34 Running"
        );
        assert_eq!(format_status_running("", 7890), "Mihomo Running(7890)");
        assert_eq!(format_status_running("", 0), "Mihomo Running");
    }

    #[test]
    fn formats_update_subscription_label_buckets() {
        let now = 1_700_000_000;
        assert_eq!(
            format_update_subscription_label(None, now),
            "Update Subscription"
        );
        // Clock skew slightly in the future should still read cleanly.
        assert_eq!(
            format_update_subscription_label(Some(now + 10), now),
            "Update Subscription (just now)"
        );
        assert_eq!(
            format_update_subscription_label(Some(now - 59), now),
            "Update Subscription (just now)"
        );
        assert_eq!(
            format_update_subscription_label(Some(now - 60), now),
            "Update Subscription (1m ago)"
        );
        assert_eq!(
            format_update_subscription_label(Some(now - 3_599), now),
            "Update Subscription (59m ago)"
        );
        assert_eq!(
            format_update_subscription_label(Some(now - 3_600), now),
            "Update Subscription (1h ago)"
        );
        assert_eq!(
            format_update_subscription_label(Some(now - 86_399), now),
            "Update Subscription (23h ago)"
        );
        assert_eq!(
            format_update_subscription_label(Some(now - 86_400), now),
            "Update Subscription (1d ago)"
        );
    }
}
