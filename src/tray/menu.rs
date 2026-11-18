//! Cross-platform native tray menu builder built on `tray_icon::menu` (`muda 0.21`).

use std::time::{SystemTime, UNIX_EPOCH};

use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

use crate::state::{Group, ProxyMode, Snapshot};
use crate::text;

#[derive(Debug, Clone)]
pub enum MenuAction {
    OpenWebUi,
    SetSubscriptionUrl,
    SetMode(String),
    ToggleSystemProxy,
    ToggleTun,
    SelectProxy { group: String, member: String },
    UnfixGroup(String),
    TestGroupLatency(String),
    OpenTrayConfig,
    OpenMihomoConfig,
    OpenPortableDir,
    OpenUiDir,
    OpenLogFile,
    CopyEnvVars,
    RefreshSubscription,
    RestartCore,
    RestartApp,
    ToggleAutostart,
    ShowVersion,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuFingerprint {
    status_line: String,
    error: Option<String>,
    web_ui_url: Option<String>,
    mode: ProxyMode,
    controller_ok: bool,
    has_mixed_port: bool,
    sysproxy: bool,
    tun: bool,
    autostart: bool,
    groups: Vec<Group>,
    visible_latencies: Vec<Option<u32>>,
    update_subscription_label: String,
    has_resolved_config: bool,
    page_size: usize,
}

impl MenuFingerprint {
    pub fn from_snapshot(snap: &Snapshot, now_unix: i64) -> Self {
        let visible_latencies = if snap.controller_ok {
            snap.groups
                .iter()
                .flat_map(|g| g.members.iter())
                .map(|m| snap.latency_for(m))
                .collect()
        } else {
            Vec::new()
        };
        Self {
            status_line: snap.status_line(),
            error: snap.error().map(ToString::to_string),
            web_ui_url: snap.web_ui_url.clone(),
            mode: snap.proxy_mode(),
            controller_ok: snap.controller_ok,
            has_mixed_port: snap.mixed_port > 0,
            sysproxy: snap.sysproxy,
            tun: snap.tun,
            autostart: snap.autostart,
            groups: snap.groups.clone(),
            visible_latencies,
            update_subscription_label: text::format_update_subscription_label(
                snap.subscription_updated_at(),
                now_unix,
            ),
            has_resolved_config: snap.resolved_config.is_some(),
            page_size: snap.page_size,
        }
    }
}

pub fn current_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub struct BuiltMenu {
    pub menu: Menu,
    pub actions: Vec<MenuAction>,
}

/// Parses a generation-scoped menu item ID formatted as `act_{generation}_{index}`.
pub fn parse_action_id(id_str: &str) -> Option<(u64, usize)> {
    let (gen_str, idx_str) = id_str.strip_prefix("act_")?.split_once('_')?;
    let generation = gen_str.parse::<u64>().ok()?;
    let idx = idx_str.parse::<usize>().ok()?;
    Some((generation, idx))
}

struct ActionRecorder {
    generation: u64,
    actions: Vec<MenuAction>,
}

impl ActionRecorder {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            actions: Vec::new(),
        }
    }

    fn next_id(&mut self, action: MenuAction) -> String {
        let id = format!("act_{}_{}", self.generation, self.actions.len());
        self.actions.push(action);
        id
    }
}

trait MenuContainer {
    fn append_item(&self, item: &dyn tray_icon::menu::IsMenuItem);
}

impl MenuContainer for Menu {
    fn append_item(&self, item: &dyn tray_icon::menu::IsMenuItem) {
        let _ = self.append(item);
    }
}

impl MenuContainer for Submenu {
    fn append_item(&self, item: &dyn tray_icon::menu::IsMenuItem) {
        let _ = self.append(item);
    }
}

fn append_menu_item<C: MenuContainer>(
    container: &C,
    recorder: &mut ActionRecorder,
    label: &str,
    enabled: bool,
    action: Option<MenuAction>,
) {
    let item = match action {
        Some(act) => MenuItem::with_id(recorder.next_id(act), label, enabled, None),
        None => MenuItem::new(label, enabled, None),
    };
    container.append_item(&item);
}

fn append_check_item<C: MenuContainer>(
    container: &C,
    recorder: &mut ActionRecorder,
    label: &str,
    enabled: bool,
    checked: bool,
    action: MenuAction,
) {
    let item = CheckMenuItem::with_id(recorder.next_id(action), label, enabled, checked, None);
    container.append_item(&item);
}

pub fn build(snapshot: &Snapshot, generation: u64, now_unix: i64) -> BuiltMenu {
    let mut recorder = ActionRecorder::new(generation);
    let menu = Menu::new();
    let page_size = snapshot.page_size;

    // 1. Mihomo status + optional error line (transient `status_note` is shown in tray tooltip).
    append_menu_item(&menu, &mut recorder, &snapshot.status_line(), false, None);
    if let Some(err) = snapshot.error() {
        let short = truncate_label(err, 90);
        append_menu_item(&menu, &mut recorder, &format!("! {short}"), false, None);
    }

    // 2. WebUI & subscription onboarding
    append_menu_item(
        &menu,
        &mut recorder,
        text::MENU_OPEN_WEB_UI,
        snapshot.web_ui_url.is_some(),
        Some(MenuAction::OpenWebUi),
    );
    append_menu_item(
        &menu,
        &mut recorder,
        text::MENU_SET_SUBSCRIPTION,
        true,
        Some(MenuAction::SetSubscriptionUrl),
    );
    let _ = menu.append(&PredefinedMenuItem::separator());

    // 3. Proxy routing modes
    let active_mode = snapshot.proxy_mode();
    let mode_enabled = snapshot.controller_ok;
    for (label, mode_variant) in [
        (text::MENU_MODE_RULE, ProxyMode::Rule),
        (text::MENU_MODE_GLOBAL, ProxyMode::Global),
        (text::MENU_MODE_DIRECT, ProxyMode::Direct),
    ] {
        append_check_item(
            &menu,
            &mut recorder,
            label,
            mode_enabled,
            active_mode == mode_variant,
            MenuAction::SetMode(mode_variant.as_api_str().to_string()),
        );
    }
    let _ = menu.append(&PredefinedMenuItem::separator());

    // 4. System Proxy & TUN toggles
    let can_toggle_sysproxy =
        snapshot.sysproxy || (snapshot.controller_ok && snapshot.mixed_port > 0);
    append_check_item(
        &menu,
        &mut recorder,
        text::MENU_SYSTEM_PROXY,
        can_toggle_sysproxy,
        snapshot.sysproxy,
        MenuAction::ToggleSystemProxy,
    );
    append_check_item(
        &menu,
        &mut recorder,
        text::MENU_TUN_MODE,
        snapshot.controller_ok,
        snapshot.tun,
        MenuAction::ToggleTun,
    );
    let _ = menu.append(&PredefinedMenuItem::separator());

    // 5. Proxy Groups submenu (only shown when groups exist)
    if snapshot.controller_ok && !snapshot.groups.is_empty() {
        let groups_sub = Submenu::new(text::MENU_PROXY_GROUPS, true);
        for group in &snapshot.groups {
            let sub = build_group_submenu(group, snapshot, page_size, &mut recorder);
            let _ = groups_sub.append(&sub);
        }
        let _ = menu.append(&groups_sub);
        let _ = menu.append(&PredefinedMenuItem::separator());
    }

    // 6. Open & More submenus + Quit
    let open_sub = Submenu::new(text::MENU_OPEN, true);
    append_open_items(&open_sub, snapshot, &mut recorder);
    let _ = menu.append(&open_sub);

    let more_sub = Submenu::new(text::MENU_MORE, true);
    append_more_items(&more_sub, snapshot, now_unix, &mut recorder);
    let _ = menu.append(&more_sub);

    append_menu_item(
        &menu,
        &mut recorder,
        text::MENU_QUIT,
        true,
        Some(MenuAction::Quit),
    );

    BuiltMenu {
        menu,
        actions: recorder.actions,
    }
}

fn build_group_submenu(
    group: &Group,
    snapshot: &Snapshot,
    page_size: usize,
    recorder: &mut ActionRecorder,
) -> Submenu {
    let is_pinned =
        !group.fixed.is_empty() && matches!(group.kind.as_str(), "URLTest" | "Fallback");
    let sub = Submenu::new(text::format_group_label(&group.name, is_pinned), true);

    if group.switchable && !group.members.is_empty() {
        append_menu_item(
            &sub,
            recorder,
            text::MENU_TEST_LATENCY,
            true,
            Some(MenuAction::TestGroupLatency(group.name.clone())),
        );

        if is_pinned {
            append_menu_item(
                &sub,
                recorder,
                text::MENU_AUTO_UNPIN,
                true,
                Some(MenuAction::UnfixGroup(group.name.clone())),
            );
        }
        let _ = sub.append(&PredefinedMenuItem::separator());
    }

    let mut members: Vec<(&str, Option<u32>)> = group
        .members
        .iter()
        .map(|m| (m.as_str(), snapshot.latency_for(m)))
        .collect();

    // Keep measured positive latencies first, sorted fastest-first; timeouts/untested follow.
    members.sort_by(|(name_a, delay_a), (name_b, delay_b)| {
        let rank = |d: Option<u32>| match d {
            Some(ms) if ms > 0 => (0u8, ms),
            Some(_) => (1u8, u32::MAX),
            None => (2u8, u32::MAX),
        };
        rank(*delay_a).cmp(&rank(*delay_b)).then_with(|| {
            name_a
                .to_ascii_lowercase()
                .cmp(&name_b.to_ascii_lowercase())
        })
    });

    let effective_page_size = if page_size > 0 && members.len() > page_size {
        page_size
    } else {
        0
    };

    if effective_page_size == 0 {
        for (member, delay) in members {
            append_member_item(&sub, group, member, delay, recorder);
        }
    } else {
        for (idx, chunk) in members.chunks(effective_page_size).enumerate() {
            let start = idx * effective_page_size + 1;
            let end = start + chunk.len() - 1;
            let page_sub = Submenu::new(text::format_page_label(start, end), true);
            for &(member, delay) in chunk {
                append_member_item(&page_sub, group, member, delay, recorder);
            }
            let _ = sub.append(&page_sub);
        }
    }

    sub
}

fn append_member_item(
    target: &Submenu,
    group: &Group,
    member: &str,
    delay: Option<u32>,
    recorder: &mut ActionRecorder,
) {
    let label = text::format_node_label(member, delay);
    let selected = member == group.now;
    append_check_item(
        target,
        recorder,
        &label,
        group.switchable,
        selected,
        MenuAction::SelectProxy {
            group: group.name.clone(),
            member: member.to_string(),
        },
    );
}

fn append_open_items(sub: &Submenu, snapshot: &Snapshot, recorder: &mut ActionRecorder) {
    append_menu_item(
        sub,
        recorder,
        text::MENU_OPEN_TRAY_CONFIG,
        true,
        Some(MenuAction::OpenTrayConfig),
    );
    append_menu_item(
        sub,
        recorder,
        text::MENU_OPEN_MIHOMO_CONFIG,
        snapshot.resolved_config.is_some(),
        Some(MenuAction::OpenMihomoConfig),
    );
    append_menu_item(
        sub,
        recorder,
        text::MENU_OPEN_PORTABLE_DIR,
        true,
        Some(MenuAction::OpenPortableDir),
    );
    append_menu_item(
        sub,
        recorder,
        text::MENU_OPEN_UI_DIR,
        true,
        Some(MenuAction::OpenUiDir),
    );
    append_menu_item(
        sub,
        recorder,
        text::MENU_OPEN_LOG_FILE,
        true,
        Some(MenuAction::OpenLogFile),
    );
}

fn append_more_items(
    sub: &Submenu,
    snapshot: &Snapshot,
    now_unix: i64,
    recorder: &mut ActionRecorder,
) {
    append_menu_item(
        sub,
        recorder,
        text::MENU_COPY_ENV_VARS,
        true,
        Some(MenuAction::CopyEnvVars),
    );
    let update_label =
        text::format_update_subscription_label(snapshot.subscription_updated_at(), now_unix);
    append_menu_item(
        sub,
        recorder,
        &update_label,
        snapshot.controller_ok,
        Some(MenuAction::RefreshSubscription),
    );
    append_menu_item(
        sub,
        recorder,
        text::MENU_RESTART_CORE,
        true,
        Some(MenuAction::RestartCore),
    );
    append_menu_item(
        sub,
        recorder,
        text::MENU_RESTART_APP,
        true,
        Some(MenuAction::RestartApp),
    );
    append_check_item(
        sub,
        recorder,
        text::MENU_AUTOSTART,
        true,
        snapshot.autostart,
        MenuAction::ToggleAutostart,
    );
    append_menu_item(
        sub,
        recorder,
        &text::format_version_label(text::APP_VERSION),
        true,
        Some(MenuAction::ShowVersion),
    );
}

fn truncate_label(raw: &str, max_chars: usize) -> String {
    let first_line = raw.lines().next().unwrap_or(raw).trim();
    let mut out = String::new();
    for (idx, ch) in first_line.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_texts(items: Vec<tray_icon::menu::MenuItemKind>) -> Vec<String> {
        items
            .into_iter()
            .map(|item| match item {
                tray_icon::menu::MenuItemKind::MenuItem(i) => i.text(),
                tray_icon::menu::MenuItemKind::Check(i) => i.text(),
                tray_icon::menu::MenuItemKind::Submenu(i) => i.text(),
                tray_icon::menu::MenuItemKind::Predefined(_) => "---".to_string(),
                tray_icon::menu::MenuItemKind::Icon(i) => i.text(),
            })
            .collect()
    }

    #[test]
    fn builds_requested_hierarchy_without_id_collisions() {
        let snap = Snapshot {
            controller_ok: true,
            mode: "rule".to_string(),
            mixed_port: 7890,
            web_ui_url: Some("http://127.0.0.1:9090/ui/".to_string()),
            groups: vec![Group {
                name: "Proxy_Group".to_string(),
                kind: "Selector".to_string(),
                switchable: true,
                now: "Node_A".to_string(),
                fixed: String::new(),
                members: vec!["Node_A".to_string(), "Node_B".to_string()],
            }],
            ..Default::default()
        };

        let built = build(&snap, 1, 1_700_000_000);
        assert!(matches!(built.actions[0], MenuAction::OpenWebUi));
        assert!(matches!(built.actions[1], MenuAction::SetSubscriptionUrl));
        assert!(
            built
                .actions
                .iter()
                .any(|a| matches!(a, MenuAction::CopyEnvVars))
        );
        assert!(
            built
                .actions
                .iter()
                .any(|a| matches!(a, MenuAction::RefreshSubscription))
        );
        assert!(
            built
                .actions
                .iter()
                .any(|a| matches!(a, MenuAction::RestartCore))
        );
        assert!(
            built
                .actions
                .iter()
                .any(|a| matches!(a, MenuAction::RestartApp))
        );
        assert!(
            built
                .actions
                .iter()
                .any(|a| matches!(a, MenuAction::ShowVersion))
        );
        assert!(matches!(built.actions.last(), Some(MenuAction::Quit)));

        let root_texts = item_texts(built.menu.items());
        assert_eq!(
            root_texts,
            vec![
                "Mihomo: Running (rule)",
                "Open WebUI",
                "Set Subscription URL",
                "---",
                "Rule",
                "Global",
                "Direct",
                "---",
                "System Proxy",
                "TUN Mode",
                "---",
                "Proxy Groups",
                "---",
                "Open",
                "More",
                "Quit",
            ]
        );
    }

    #[test]
    fn paginates_large_groups_cleanly() {
        let members: Vec<String> = (1..=5).map(|i| format!("Node-{i}")).collect();
        let snap = Snapshot {
            controller_ok: true,
            page_size: 2,
            groups: vec![Group {
                name: "Large".to_string(),
                kind: "Selector".to_string(),
                switchable: true,
                now: "Node-1".to_string(),
                fixed: String::new(),
                members,
            }],
            ..Default::default()
        };

        let built = build(&snap, 1, 1_700_000_000);
        let selected_count = built
            .actions
            .iter()
            .filter(|a| matches!(a, MenuAction::SelectProxy { .. }))
            .count();
        assert_eq!(selected_count, 5);
    }

    #[test]
    fn menu_fingerprint_tracks_visible_changes_only() {
        let s1 = Snapshot {
            controller_ok: true,
            mode: "rule".to_string(),
            ..Default::default()
        };
        let now = 1_700_000_000;
        let mut s2 = s1.clone();
        assert_eq!(
            MenuFingerprint::from_snapshot(&s1, now),
            MenuFingerprint::from_snapshot(&s2, now)
        );
        s2.mode = "global".to_string();
        assert_ne!(
            MenuFingerprint::from_snapshot(&s1, now),
            MenuFingerprint::from_snapshot(&s2, now)
        );
    }

    #[test]
    fn update_subscription_menu_tracks_age_buckets_without_churn() {
        let now = 1_700_000_000;
        let make_snap = |controller_ok: bool, vehicle: &str, updated_at: Option<i64>| Snapshot {
            controller_ok,
            providers: vec![crate::state::Provider {
                name: "subscription".to_string(),
                vehicle: vehicle.to_string(),
                updated_at,
            }],
            ..Default::default()
        };

        // Within the same "just now" (<60s) bucket, fingerprint must not churn.
        let s_5s = make_snap(true, "HTTP", Some(now - 5));
        let s_45s = make_snap(true, "HTTP", Some(now - 45));
        assert_eq!(
            MenuFingerprint::from_snapshot(&s_5s, now),
            MenuFingerprint::from_snapshot(&s_45s, now)
        );

        // Crossing into the minute bucket updates fingerprint and More submenu item text.
        let s_2m = make_snap(true, "HTTP", Some(now - 120));
        assert_ne!(
            MenuFingerprint::from_snapshot(&s_45s, now),
            MenuFingerprint::from_snapshot(&s_2m, now)
        );

        let built = build(&s_2m, 1, now);
        let more_sub = built
            .menu
            .items()
            .into_iter()
            .find_map(|item| match item {
                tray_icon::menu::MenuItemKind::Submenu(sub) if sub.text() == text::MENU_MORE => {
                    Some(sub)
                }
                _ => None,
            })
            .expect("More submenu missing");
        let more_texts = item_texts(more_sub.items());
        assert!(
            more_texts
                .iter()
                .any(|t| t == "Update Subscription (2m ago)")
        );

        // Non-refreshable Compatible groups and offline controller states fall back cleanly.
        assert_eq!(
            make_snap(true, "Compatible", Some(now - 120)).subscription_updated_at(),
            None
        );
        assert_eq!(
            make_snap(false, "HTTP", Some(now - 120)).subscription_updated_at(),
            None
        );
    }

    #[test]
    fn parses_generation_scoped_action_ids() {
        assert_eq!(parse_action_id("act_7_12"), Some((7, 12)));
        assert_eq!(parse_action_id("act_0_0"), Some((0, 0)));
        assert_eq!(parse_action_id("act_12"), None);
        assert_eq!(parse_action_id("bad_1_2"), None);

        let snap = Snapshot::default();
        let built = build(&snap, 42, 1_700_000_000);
        let last_id = built
            .menu
            .items()
            .last()
            .map(|item| match item {
                tray_icon::menu::MenuItemKind::MenuItem(i) => i.id().as_ref().to_string(),
                _ => String::new(),
            })
            .unwrap();
        assert_eq!(
            parse_action_id(&last_id),
            Some((42, built.actions.len() - 1))
        );
    }

    #[test]
    fn menu_fingerprint_tracks_visible_node_latencies_only() {
        let group = Group {
            name: "Proxy".to_string(),
            kind: "Selector".to_string(),
            switchable: true,
            now: "Node-A".to_string(),
            fixed: String::new(),
            members: vec!["Node-A".to_string(), "Node-B".to_string()],
        };
        let mut s1 = Snapshot {
            controller_ok: true,
            groups: vec![group.clone()],
            ..Default::default()
        };
        s1.node_latency.insert("Node-B".to_string(), Some(120));
        s1.node_latency.insert("Node-A".to_string(), Some(45));
        s1.node_latency.insert("Hidden-Node".to_string(), Some(30));

        let mut s2 = Snapshot {
            controller_ok: true,
            groups: vec![group],
            ..Default::default()
        };
        s2.node_latency.insert("Node-A".to_string(), Some(45));
        s2.node_latency.insert("Node-B".to_string(), Some(120));
        s2.node_latency.insert("Hidden-Node".to_string(), Some(999));

        assert_eq!(
            MenuFingerprint::from_snapshot(&s1, 1_700_000_000),
            MenuFingerprint::from_snapshot(&s2, 1_700_000_000)
        );

        s2.node_latency.insert("Node-A".to_string(), Some(25));
        assert_ne!(
            MenuFingerprint::from_snapshot(&s1, 1_700_000_000),
            MenuFingerprint::from_snapshot(&s2, 1_700_000_000)
        );
    }
}
