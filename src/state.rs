//! Shared runtime state, worker commands, and UI wakeup events.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tray_icon::TrayIconEvent;
use tray_icon::menu::MenuEvent;

/// Normalized proxy routing mode shown on the tray icon and menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyMode {
    #[default]
    Rule,
    Global,
    Direct,
    Unknown,
}

impl ProxyMode {
    pub fn from_api(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "rule" => Self::Rule,
            "global" => Self::Global,
            "direct" => Self::Direct,
            _ => Self::Unknown,
        }
    }

    pub fn as_api_str(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Global => "global",
            Self::Direct => "direct",
            Self::Unknown => "",
        }
    }
}

/// High-level operational state encoded into the tray icon's outer ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OperationalState {
    #[default]
    Unreachable,
    Running,
    SysProxy,
    Tun,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub kind: String,
    pub switchable: bool,
    pub now: String,
    pub fixed: String,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provider {
    pub name: String,
    pub vehicle: String,
    pub updated_at: Option<i64>,
}

impl Provider {
    pub fn refreshable(&self) -> bool {
        matches!(self.vehicle.as_str(), "HTTP" | "File")
    }
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub controller_ok: bool,
    pub version: String,
    pub mode: String,
    pub mixed_port: u16,
    pub tun: bool,
    pub sysproxy: bool,
    pub autostart: bool,
    pub kernel_running: bool,
    pub groups: Vec<Group>,
    pub node_latency: BTreeMap<String, Option<u32>>,
    pub providers: Vec<Provider>,
    pub status_note: Option<String>,
    pub action_error: Option<String>,
    pub controller_error: Option<String>,
    pub web_ui_url: Option<String>,
    pub resolved_config: Option<PathBuf>,
    pub page_size: usize,
}

impl Snapshot {
    pub fn operational_state(&self) -> OperationalState {
        if !self.controller_ok {
            OperationalState::Unreachable
        } else if self.tun {
            OperationalState::Tun
        } else if self.sysproxy {
            OperationalState::SysProxy
        } else {
            OperationalState::Running
        }
    }

    pub fn proxy_mode(&self) -> ProxyMode {
        if !self.controller_ok {
            ProxyMode::Unknown
        } else {
            ProxyMode::from_api(&self.mode)
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.controller_error
            .as_deref()
            .or(self.action_error.as_deref())
    }

    pub fn latency_for(&self, node: &str) -> Option<u32> {
        self.node_latency.get(node).copied().flatten()
    }

    pub fn subscription_updated_at(&self) -> Option<i64> {
        if !self.controller_ok {
            return None;
        }
        self.providers
            .iter()
            .filter(|p| p.refreshable())
            .filter_map(|p| p.updated_at)
            .max()
    }

    pub fn tooltip(&self) -> String {
        let mut lines = vec![self.status_line()];
        if let Some(note) = &self.status_note {
            lines.push(note.clone());
        } else if let Some(err) = self.error() {
            lines.push(format!("Error: {err}"));
        }
        if self.controller_ok {
            let mut flags = Vec::new();
            if self.sysproxy {
                flags.push("System Proxy");
            }
            if self.tun {
                flags.push("TUN");
            }
            if !flags.is_empty() {
                lines.push(flags.join(" + "));
            }
        }
        lines.join("\n")
    }

    pub fn status_line(&self) -> String {
        if self.controller_ok {
            crate::text::format_status_running(&self.mode, &self.version)
        } else {
            crate::text::format_status_unreachable(self.kernel_running).to_string()
        }
    }
}

/// Shared snapshot protected by a mutex so UI renders never block on HTTP calls.
pub type SharedSnapshot = Arc<Mutex<Snapshot>>;

#[derive(Debug, Clone)]
pub enum WorkerCommand {
    SetMode(String),
    ToggleSystemProxy,
    ToggleTun,
    SelectProxy { group: String, member: String },
    UnfixGroup(String),
    TestGroupLatency(String),
    RefreshSubscriptions,
    SetSubscriptionUrl(String),
    RestartCore,
    ToggleAutostart,
    Quit { interactive: bool },
}

#[derive(Debug)]
pub enum AppEvent {
    TrayIcon(TrayIconEvent),
    Menu(MenuEvent),
    SnapshotUpdated { rebuild_menu: bool },
    QuitCompleted,
}
