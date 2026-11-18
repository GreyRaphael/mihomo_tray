//! Shared runtime state, worker commands, and UI wakeup events.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tray_icon::TrayIconEvent;
use tray_icon::menu::MenuEvent;

/// Normalized proxy routing mode shown on the tray icon and menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProxyMode {
    #[default]
    Rule,
    Global,
    Direct,
    Unknown,
}

impl ProxyMode {
    fn from_api(raw: &str) -> Self {
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
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
    pub config_error: Option<String>,
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
        self.config_error
            .as_deref()
            .or(self.action_error.as_deref())
            .or(self.controller_error.as_deref())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsyncTaskKind {
    LatencyTest { id: u64 },
    RefreshSubscriptions { id: u64 },
    ApplySubscription { id: u64, reload_ok: bool },
}

#[derive(Debug, Clone)]
pub enum WorkerCommand {
    SetMode(ProxyMode),
    ToggleSystemProxy,
    ToggleTun,
    SelectProxy {
        group: String,
        member: String,
    },
    UnfixGroup(String),
    TestGroupLatency(String),
    RefreshSubscriptions,
    SetSubscriptionUrl(String),
    CopyEnvVars,
    RestartCore,
    ToggleAutostart,
    AsyncDone {
        kind: AsyncTaskKind,
        error: Option<String>,
    },
    Quit {
        interactive: bool,
    },
}

#[derive(Debug)]
pub enum AppEvent {
    TrayIcon(TrayIconEvent),
    Menu(MenuEvent),
    SnapshotUpdated { rebuild_menu: bool },
    QuitCompleted,
    QuitCancelled,
}

pub fn order_and_filter_groups(
    groups: Vec<Group>,
    include: &[String],
    exclude: &[String],
    order: &[String],
) -> Vec<Group> {
    let mut filtered: Vec<Group> = groups
        .into_iter()
        .filter(|g| {
            if !include.is_empty() && !include.iter().any(|x| x.eq_ignore_ascii_case(&g.name)) {
                return false;
            }
            if exclude.iter().any(|x| x.eq_ignore_ascii_case(&g.name)) {
                return false;
            }
            true
        })
        .collect();

    filtered.sort_by(|a, b| {
        if a.name == "GLOBAL" && b.name != "GLOBAL" {
            return std::cmp::Ordering::Less;
        }
        if b.name == "GLOBAL" && a.name != "GLOBAL" {
            return std::cmp::Ordering::Greater;
        }
        let pos_a = order.iter().position(|x| x.eq_ignore_ascii_case(&a.name));
        let pos_b = order.iter().position(|x| x.eq_ignore_ascii_case(&b.name));
        match (pos_a, pos_b) {
            (Some(ia), Some(ib)) => ia.cmp(&ib),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a
                .name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase()),
        }
    });
    filtered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(name: &str) -> Group {
        Group {
            name: name.to_string(),
            kind: "Selector".to_string(),
            switchable: true,
            ..Default::default()
        }
    }

    #[test]
    fn keeps_global_first_and_applies_order_and_filters() {
        let order = vec!["Streaming".to_string(), "Proxy".to_string()];
        let exclude = vec!["HiddenOne".to_string()];

        let input = vec![
            g("Alpha"),
            g("Proxy"),
            g("HiddenOne"),
            g("GLOBAL"),
            g("Streaming"),
        ];
        let out: Vec<String> = order_and_filter_groups(input, &[], &exclude, &order)
            .into_iter()
            .map(|x| x.name)
            .collect();
        assert_eq!(out, vec!["GLOBAL", "Streaming", "Proxy", "Alpha"]);
    }

    #[test]
    fn snapshot_error_follows_config_then_action_then_controller_priority() {
        let mut snap = Snapshot {
            controller_error: Some("controller unreachable".to_string()),
            ..Default::default()
        };
        assert_eq!(snap.error(), Some("controller unreachable"));

        snap.action_error = Some("action failed".to_string());
        assert_eq!(snap.error(), Some("action failed"));

        snap.config_error = Some("invalid tray.yml".to_string());
        assert_eq!(snap.error(), Some("invalid tray.yml"));
    }
}
