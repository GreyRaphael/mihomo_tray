//! Static English UI labels, status strings, and formatted error messages.

pub const APP_NAME: &str = "mihomo-tray";
pub const TRAY_FILE_NAME: &str = "tray.yml";
pub const MIHOMO_CONFIG_NAME: &str = "config.yaml";
pub const RUNTIME_LOCK_NAME: &str = ".runtime.json";

pub const MENU_OPEN_WEB_UI: &str = "Open WebUI";
pub const MENU_MODE_RULE: &str = "Rule";
pub const MENU_MODE_GLOBAL: &str = "Global";
pub const MENU_MODE_DIRECT: &str = "Direct";
pub const MENU_SYSTEM_PROXY: &str = "System Proxy";
pub const MENU_TUN_MODE: &str = "TUN Mode";
pub const MENU_PROXY_GROUPS: &str = "Proxy Groups";
pub const MENU_TEST_LATENCY: &str = "Test Group Latency";
pub const MENU_AUTO_UNPIN: &str = "Auto (Unpin)";
pub const MENU_CORE: &str = "Core";
pub const MENU_CORE_START: &str = "Start Core";
pub const MENU_CORE_STOP: &str = "Stop Core";
pub const MENU_CORE_RESTART: &str = "Restart Core";
pub const MENU_CORE_FORCE_RESTART: &str = "Force Restart / Rescan Core";
pub const MENU_CORE_RELOAD_CONFIG: &str = "Reload Config";
pub const MENU_CORE_CLOSE_CONNECTIONS: &str = "Close All Connections";
pub const MENU_REFRESH_STATE: &str = "Refresh Status";
pub const MENU_REFRESH_SUBSCRIPTIONS: &str = "Refresh Subscriptions";
pub const MENU_OPEN: &str = "Open";
pub const MENU_OPEN_TRAY_CONFIG: &str = "Open tray.yml";
pub const MENU_OPEN_MIHOMO_CONFIG: &str = "Open config.yaml";
pub const MENU_OPEN_PORTABLE_DIR: &str = "Open Portable Folder";
pub const MENU_OPEN_UI_DIR: &str = "Open WebUI Folder (ui/)";
pub const MENU_OPEN_LOG_FILE: &str = "Open Log File";
pub const MENU_AUTOSTART: &str = "Start with Windows";
pub const MENU_QUIT_AND_STOP: &str = "Quit and Stop Core";
pub const MENU_QUIT_ONLY: &str = "Quit Tray Only";

pub const STATUS_STARTING_CORE: &str = "Starting mihomo core...";
pub const STATUS_WAITING_CONTROLLER: &str = "Waiting for mihomo controller...";
pub const STATUS_RESTARTING_CORE: &str = "Restarting mihomo core...";
pub const STATUS_ELEVATING_CORE: &str = "Waiting for elevation prompt...";
pub const STATUS_STOPPING_CORE: &str = "Stopping mihomo core...";

pub const ERR_ALREADY_RUNNING: &str = "mihomo-tray is already running for this portable folder.\nCheck the system tray overflow area if the icon is hidden.";
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

pub fn format_status_running(mode: &str, version: &str) -> String {
    let mode_label = if mode.is_empty() { "unknown" } else { mode };
    if version.is_empty() {
        format!("Mihomo: Running ({mode_label})")
    } else {
        format!("Mihomo {version}: Running ({mode_label})")
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

pub fn format_provider_label(name: &str, age_text: &str) -> String {
    format!("{name} ({age_text})")
}

pub fn format_page_label(start: usize, end: usize) -> String {
    format!("{start}-{end}")
}

pub fn format_relative_age(updated_at_secs: Option<i64>, now_secs: i64) -> String {
    let Some(ts) = updated_at_secs else {
        return "never updated".to_string();
    };
    let delta = now_secs.saturating_sub(ts);
    if delta < 60 {
        "just now".to_string()
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3600)
    } else {
        format!("{}d ago", delta / 86_400)
    }
}
