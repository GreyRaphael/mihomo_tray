//! Cross-platform native tray menu builder built on `tray_icon::menu` (`muda 0.21`).

use std::time::{SystemTime, UNIX_EPOCH};

use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

use crate::state::{Group, ProxyMode, Snapshot};
use crate::text;

#[derive(Debug, Clone)]
pub enum MenuAction {
    OpenWebUi,
    SetMode(String),
    ToggleSystemProxy,
    ToggleTun,
    SelectProxy { group: String, member: String },
    UnfixGroup(String),
    TestGroupLatency(String),
    RefreshProvider(String),
    StartCore,
    StopCore,
    RestartCore,
    ForceRestartCore,
    ReloadConfig,
    CloseConnections,
    RefreshNow,
    OpenTrayConfig,
    OpenMihomoConfig,
    OpenPortableDir,
    OpenUiDir,
    OpenLogFile,
    ToggleAutostart,
    Quit { stop_core: bool },
}

pub struct BuiltMenu {
    pub menu: Menu,
    pub actions: Vec<MenuAction>,
}

fn next_action_id(actions: &mut Vec<MenuAction>, action: MenuAction) -> String {
    let id = format!("act_{}", actions.len());
    actions.push(action);
    id
}

fn append_root_item(
    menu: &Menu,
    actions: &mut Vec<MenuAction>,
    label: &str,
    enabled: bool,
    action: Option<MenuAction>,
) {
    let item = match action {
        Some(act) => MenuItem::with_id(next_action_id(actions, act), label, enabled, None),
        None => MenuItem::new(label, enabled, None),
    };
    let _ = menu.append(&item);
}

fn append_root_check(
    menu: &Menu,
    actions: &mut Vec<MenuAction>,
    label: &str,
    enabled: bool,
    checked: bool,
    action: MenuAction,
) {
    let item = CheckMenuItem::with_id(
        next_action_id(actions, action),
        label,
        enabled,
        checked,
        None,
    );
    let _ = menu.append(&item);
}

pub fn build(snapshot: &Snapshot, page_size: usize) -> BuiltMenu {
    let mut actions = Vec::new();
    let menu = Menu::new();

    // 1. Status + optional error line (disabled informational rows)
    append_root_item(&menu, &mut actions, &snapshot.status_line(), false, None);
    if let Some(note) = &snapshot.status_note {
        append_root_item(&menu, &mut actions, note, false, None);
    } else if let Some(err) = snapshot.error() {
        let short = truncate_label(err, 90);
        append_root_item(&menu, &mut actions, &format!("! {short}"), false, None);
    }

    // 2. WebUI action
    append_root_item(
        &menu,
        &mut actions,
        text::MENU_OPEN_WEB_UI,
        snapshot.web_ui_url.is_some(),
        Some(MenuAction::OpenWebUi),
    );
    let _ = menu.append(&PredefinedMenuItem::separator());

    // 3. Proxy mode selection
    let active_mode = snapshot.proxy_mode();
    let mode_enabled = snapshot.controller_ok;
    for (label, mode_variant) in [
        (text::MENU_MODE_RULE, ProxyMode::Rule),
        (text::MENU_MODE_GLOBAL, ProxyMode::Global),
        (text::MENU_MODE_DIRECT, ProxyMode::Direct),
    ] {
        append_root_check(
            &menu,
            &mut actions,
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
    append_root_check(
        &menu,
        &mut actions,
        text::MENU_SYSTEM_PROXY,
        can_toggle_sysproxy,
        snapshot.sysproxy,
        MenuAction::ToggleSystemProxy,
    );
    append_root_check(
        &menu,
        &mut actions,
        text::MENU_TUN_MODE,
        snapshot.controller_ok,
        snapshot.tun,
        MenuAction::ToggleTun,
    );

    // 5. Proxy groups submenu
    if snapshot.controller_ok && !snapshot.groups.is_empty() {
        let _ = menu.append(&PredefinedMenuItem::separator());
        let groups_sub = Submenu::new(text::MENU_PROXY_GROUPS, true);
        for group in &snapshot.groups {
            let sub = build_group_submenu(group, snapshot, page_size, &mut actions);
            let _ = groups_sub.append(&sub);
        }
        let _ = menu.append(&groups_sub);
    }

    let _ = menu.append(&PredefinedMenuItem::separator());

    // 6. Core control submenu (supports stop-and-replace workflow)
    let core_sub = Submenu::new(text::MENU_CORE, true);
    append_core_items(&core_sub, snapshot, &mut actions);
    let _ = menu.append(&core_sub);

    // 7. Open portable files/folders submenu
    let open_sub = Submenu::new(text::MENU_OPEN, true);
    append_open_items(&open_sub, snapshot, &mut actions);
    let _ = menu.append(&open_sub);

    // 8. Start with Windows
    append_root_check(
        &menu,
        &mut actions,
        text::MENU_AUTOSTART,
        true,
        snapshot.autostart,
        MenuAction::ToggleAutostart,
    );

    let _ = menu.append(&PredefinedMenuItem::separator());

    // 9. Quit actions
    append_root_item(
        &menu,
        &mut actions,
        text::MENU_QUIT_AND_STOP,
        true,
        Some(MenuAction::Quit { stop_core: true }),
    );
    append_root_item(
        &menu,
        &mut actions,
        text::MENU_QUIT_ONLY,
        true,
        Some(MenuAction::Quit { stop_core: false }),
    );

    BuiltMenu { menu, actions }
}

fn build_group_submenu(
    group: &Group,
    snapshot: &Snapshot,
    page_size: usize,
    actions: &mut Vec<MenuAction>,
) -> Submenu {
    let is_pinned =
        !group.fixed.is_empty() && matches!(group.kind.as_str(), "URLTest" | "Fallback");
    let sub = Submenu::new(text::format_group_label(&group.name, is_pinned), true);

    if group.switchable && !group.members.is_empty() {
        let test_item = MenuItem::with_id(
            next_action_id(actions, MenuAction::TestGroupLatency(group.name.clone())),
            text::MENU_TEST_LATENCY,
            true,
            None,
        );
        let _ = sub.append(&test_item);

        if is_pinned {
            let unfix_item = MenuItem::with_id(
                next_action_id(actions, MenuAction::UnfixGroup(group.name.clone())),
                text::MENU_AUTO_UNPIN,
                true,
                None,
            );
            let _ = sub.append(&unfix_item);
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
            append_member_item(&sub, group, member, delay, actions);
        }
    } else {
        for (idx, chunk) in members.chunks(effective_page_size).enumerate() {
            let start = idx * effective_page_size + 1;
            let end = start + chunk.len() - 1;
            let page_sub = Submenu::new(text::format_page_label(start, end), true);
            for &(member, delay) in chunk {
                append_member_item(&page_sub, group, member, delay, actions);
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
    actions: &mut Vec<MenuAction>,
) {
    let label = text::format_node_label(member, delay);
    let selected = member == group.now;
    let id = next_action_id(
        actions,
        MenuAction::SelectProxy {
            group: group.name.clone(),
            member: member.to_string(),
        },
    );
    let item = CheckMenuItem::with_id(id, label, group.switchable, selected, None);
    let _ = target.append(&item);
}

fn append_sub_item(
    sub: &Submenu,
    actions: &mut Vec<MenuAction>,
    label: &str,
    enabled: bool,
    action: MenuAction,
) {
    let item = MenuItem::with_id(next_action_id(actions, action), label, enabled, None);
    let _ = sub.append(&item);
}

fn append_core_items(sub: &Submenu, snapshot: &Snapshot, actions: &mut Vec<MenuAction>) {
    if snapshot.kernel_running {
        append_sub_item(
            sub,
            actions,
            text::MENU_CORE_STOP,
            true,
            MenuAction::StopCore,
        );
    } else {
        append_sub_item(
            sub,
            actions,
            text::MENU_CORE_START,
            true,
            MenuAction::StartCore,
        );
    }
    append_sub_item(
        sub,
        actions,
        text::MENU_CORE_RESTART,
        snapshot.controller_ok,
        MenuAction::RestartCore,
    );
    append_sub_item(
        sub,
        actions,
        text::MENU_CORE_FORCE_RESTART,
        true,
        MenuAction::ForceRestartCore,
    );

    let _ = sub.append(&PredefinedMenuItem::separator());

    append_sub_item(
        sub,
        actions,
        text::MENU_CORE_RELOAD_CONFIG,
        snapshot.controller_ok,
        MenuAction::ReloadConfig,
    );
    append_sub_item(
        sub,
        actions,
        text::MENU_CORE_CLOSE_CONNECTIONS,
        snapshot.controller_ok,
        MenuAction::CloseConnections,
    );
    append_sub_item(
        sub,
        actions,
        text::MENU_REFRESH_STATE,
        true,
        MenuAction::RefreshNow,
    );

    let refreshable: Vec<_> = snapshot
        .providers
        .iter()
        .filter(|p| p.refreshable())
        .collect();
    if snapshot.controller_ok && !refreshable.is_empty() {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let prov_sub = Submenu::new(text::MENU_REFRESH_SUBSCRIPTIONS, true);
        for provider in refreshable {
            let age = text::format_relative_age(provider.updated_at, now_secs);
            let label = text::format_provider_label(&provider.name, &age);
            append_sub_item(
                &prov_sub,
                actions,
                &label,
                true,
                MenuAction::RefreshProvider(provider.name.clone()),
            );
        }
        let _ = sub.append(&prov_sub);
    }
}

fn append_open_items(sub: &Submenu, snapshot: &Snapshot, actions: &mut Vec<MenuAction>) {
    append_sub_item(
        sub,
        actions,
        text::MENU_OPEN_TRAY_CONFIG,
        true,
        MenuAction::OpenTrayConfig,
    );
    append_sub_item(
        sub,
        actions,
        text::MENU_OPEN_MIHOMO_CONFIG,
        snapshot.resolved_config.is_some(),
        MenuAction::OpenMihomoConfig,
    );
    append_sub_item(
        sub,
        actions,
        text::MENU_OPEN_PORTABLE_DIR,
        true,
        MenuAction::OpenPortableDir,
    );
    append_sub_item(
        sub,
        actions,
        text::MENU_OPEN_UI_DIR,
        true,
        MenuAction::OpenUiDir,
    );
    append_sub_item(
        sub,
        actions,
        text::MENU_OPEN_LOG_FILE,
        true,
        MenuAction::OpenLogFile,
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

    #[test]
    fn builds_actions_without_id_collisions() {
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

        let built = build(&snap, 0);
        assert!(built.actions.len() >= 10);
        assert!(matches!(built.actions[0], MenuAction::OpenWebUi));
        let has_select = built.actions.iter().any(|a| {
            matches!(
                a,
                MenuAction::SelectProxy { group, member } if group == "Proxy_Group" && member == "Node_A"
            )
        });
        assert!(has_select);
    }

    #[test]
    fn paginates_large_groups_cleanly() {
        let members: Vec<String> = (1..=5).map(|i| format!("Node-{i}")).collect();
        let snap = Snapshot {
            controller_ok: true,
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

        let built = build(&snap, 2);
        let selected_count = built
            .actions
            .iter()
            .filter(|a| matches!(a, MenuAction::SelectProxy { .. }))
            .count();
        assert_eq!(selected_count, 5);
    }
}
