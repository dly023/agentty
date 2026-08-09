mod icon;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native;
#[cfg(target_os = "linux")]
mod sni;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use native::Backend;
#[cfg(target_os = "linux")]
use sni::Backend;

use crate::core::cli_agent::AgentStatus;
use crate::core::config::{Config, NotifyMode};
use crate::core::i18n::ResolveLocale;
use gpui::App;
use std::sync::Mutex;

const POLL: std::time::Duration = std::time::Duration::from_millis(1000);
const DISPATCH_CAPACITY: usize = 64;
static SENDER: Mutex<Option<smol::channel::Sender<TrayAction>>> = Mutex::new(None);

pub(crate) fn sender() -> Option<smol::channel::Sender<TrayAction>> {
    SENDER.lock().ok()?.clone()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TrayAction {
    ShowWindow,
    RevealLeaf {
        leaf_id: u64,
    },
    RevealPane {
        target: crate::ui::composer::PaneIdentity,
    },
    SetNotifyMode(NotifyMode),
    OpenSettings,
    CheckForUpdates,
    Quit,
    QuitStopSessions,
}

pub(crate) fn urgency(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Waiting => 3,
        AgentStatus::Working => 2,
        AgentStatus::Done => 1,
        AgentStatus::Idle => 0,
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AgentRow {
    pub leaf_id: u64,
    pub agent: crate::core::cli_agent::CLIAgent,
    pub status: AgentStatus,
    pub detail: String,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct TraySnapshot {
    pub agents: Vec<AgentRow>,
    pub notify_mode: NotifyMode,
    pub locale: crate::core::i18n::Locale,
}

impl TraySnapshot {
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub(crate) fn attention(&self) -> bool {
        self.agents.iter().any(|a| a.status == AgentStatus::Waiting)
    }

    pub(crate) fn tooltip(&self) -> String {
        let count = |s: AgentStatus| self.agents.iter().filter(|a| a.status == s).count();
        let locale = self.locale;
        let mut parts = Vec::new();
        for (n, key) in [
            (count(AgentStatus::Waiting), "tray.tooltip.waiting"),
            (count(AgentStatus::Working), "tray.tooltip.working"),
            (count(AgentStatus::Done), "tray.tooltip.done"),
        ] {
            if n > 0 {
                let word = crate::core::i18n::tr(locale, key);
                parts.push(format!("{n} {word}"));
            }
        }
        if parts.is_empty() {
            "agentty".to_string()
        } else {
            format!("agentty — {}", parts.join(", "))
        }
    }
}

pub(crate) enum SpecItem {
    Item {
        id: String,
        label: String,
        checked: Option<bool>,
        avatar: Option<(crate::core::cli_agent::CLIAgent, AgentStatus)>,
    },
    Separator,
    Submenu {
        label: String,
        items: Vec<SpecItem>,
    },
}

pub(crate) fn menu_spec(snap: &TraySnapshot) -> Vec<SpecItem> {
    let locale = snap.locale;
    let t = |key| crate::core::i18n::tr(locale, key).to_string();
    let item = |id: &str, label: String| SpecItem::Item {
        id: id.to_string(),
        label,
        checked: None,
        avatar: None,
    };
    let mut items = vec![item("show", t("tray.show")), SpecItem::Separator];
    for a in &snap.agents {
        let state = match a.status {
            AgentStatus::Waiting => format!(
                " — {}",
                crate::core::i18n::tr(locale, "tray.status.waiting")
            ),
            AgentStatus::Working => format!(
                " — {}",
                crate::core::i18n::tr(locale, "tray.status.working")
            ),
            AgentStatus::Done => {
                format!(" — {}", crate::core::i18n::tr(locale, "tray.status.done"))
            }
            AgentStatus::Idle => String::new(),
        };
        items.push(SpecItem::Item {
            id: format!("agent:{}", a.leaf_id),
            label: format!("{} · {}{state}", a.agent.display_name(), a.detail),
            checked: None,
            avatar: Some((a.agent, a.status)),
        });
    }
    if !snap.agents.is_empty() {
        items.push(SpecItem::Separator);
    }
    let notify = |id: &str, label: String, mode: NotifyMode| SpecItem::Item {
        id: id.to_string(),
        label,
        checked: Some(snap.notify_mode == mode),
        avatar: None,
    };
    items.push(SpecItem::Submenu {
        label: t("tray.notifications"),
        items: vec![
            notify("notify:never", t("opt.never"), NotifyMode::Never),
            notify(
                "notify:unfocused",
                t("opt.when_unfocused"),
                NotifyMode::Unfocused,
            ),
            notify("notify:always", t("opt.always"), NotifyMode::Always),
        ],
    });
    items.push(item("settings", t("palette.cmd.settings")));
    items.push(item("updates", t("palette.cmd.check_updates")));
    items.push(SpecItem::Separator);
    items.push(item("quit", t("palette.cmd.quit")));
    items.push(item("quit-stop", t("tray.quit_stop")));
    items
}

pub(crate) fn action_from_id(id: &str) -> Option<TrayAction> {
    match id {
        "show" => Some(TrayAction::ShowWindow),
        "settings" => Some(TrayAction::OpenSettings),
        "updates" => Some(TrayAction::CheckForUpdates),
        "quit" => Some(TrayAction::Quit),
        "quit-stop" => Some(TrayAction::QuitStopSessions),
        "notify:always" => Some(TrayAction::SetNotifyMode(NotifyMode::Always)),
        "notify:unfocused" => Some(TrayAction::SetNotifyMode(NotifyMode::Unfocused)),
        "notify:never" => Some(TrayAction::SetNotifyMode(NotifyMode::Never)),
        _ => {
            let leaf_id = id.strip_prefix("agent:")?.parse().ok()?;
            Some(TrayAction::RevealLeaf { leaf_id })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentty_core::core::environment::EnvironmentDescriptor;

    fn snapshot_with_agent(status: AgentStatus) -> TraySnapshot {
        TraySnapshot {
            agents: vec![AgentRow {
                leaf_id: 42,
                agent: crate::core::cli_agent::CLIAgent::Claude,
                status,
                detail: "agentty @ main".into(),
            }],
            notify_mode: NotifyMode::Unfocused,
            locale: crate::core::i18n::Locale::EnUs,
        }
    }

    #[test]
    fn every_menu_id_decodes_to_an_action() {
        fn check(items: &[SpecItem]) {
            for item in items {
                match item {
                    SpecItem::Item { id, label, .. } => assert!(
                        action_from_id(id).is_some(),
                        "menu item {label:?} has undecodable id {id:?}"
                    ),
                    SpecItem::Separator => {}
                    SpecItem::Submenu { items, .. } => check(items),
                }
            }
        }
        check(&menu_spec(&snapshot_with_agent(AgentStatus::Waiting)));
        check(&menu_spec(&TraySnapshot::default()));
    }

    #[test]
    fn agent_rows_decode_to_reveal_with_their_leaf_id() {
        assert_eq!(
            action_from_id("agent:42"),
            Some(TrayAction::RevealLeaf { leaf_id: 42 })
        );
        assert_eq!(action_from_id("agent:nope"), None);
        assert_eq!(action_from_id("bogus"), None);
    }

    #[test]
    fn notification_reveal_targets_environment_and_pane_not_entity_id() {
        let target = crate::ui::composer::PaneIdentity {
            environment: EnvironmentDescriptor::local().id,
            pane_id: 42,
        };
        assert_eq!(
            notification_reveal_action(target.clone()),
            TrayAction::RevealPane { target }
        );
    }

    #[test]
    fn stale_notification_target_is_a_noop() {
        assert_eq!(dispatch_target_for_reveal(false, true), None);
    }

    #[test]
    fn attention_follows_waiting_and_tooltip_counts() {
        assert!(snapshot_with_agent(AgentStatus::Waiting).attention());
        assert!(!snapshot_with_agent(AgentStatus::Working).attention());
        assert!(!snapshot_with_agent(AgentStatus::Done).attention());
        assert_eq!(
            snapshot_with_agent(AgentStatus::Waiting).tooltip(),
            "agentty — 1 waiting"
        );
        assert_eq!(TraySnapshot::default().tooltip(), "agentty");
    }

    #[test]
    fn menu_spec_shape() {
        let empty = menu_spec(&TraySnapshot::default());
        let labels: Vec<_> = empty
            .iter()
            .filter_map(|i| match i {
                SpecItem::Item { label, .. } => Some(label.as_str()),
                SpecItem::Submenu { label, .. } => Some(label.as_str()),
                SpecItem::Separator => None,
            })
            .collect();
        assert_eq!(
            labels,
            [
                "Show agentty",
                "Notifications",
                "Settings…",
                "Check for Updates…",
                "Quit agentty",
                "Quit and Stop Server…"
            ]
        );
        assert!(
            !empty
                .windows(2)
                .any(|w| matches!(w, [SpecItem::Separator, SpecItem::Separator]))
        );

        let with_agent = menu_spec(&snapshot_with_agent(AgentStatus::Waiting));
        assert!(with_agent.iter().any(|i| matches!(
            i,
            SpecItem::Item { id, avatar: Some(_), .. } if id == "agent:42"
        )));
    }
}

fn app_snapshot(cx: &mut App) -> TraySnapshot {
    let windows = crate::ui::windows::WindowRegistry::open_windows(cx);
    let mut agents = Vec::new();
    for (_, weak) in windows {
        let Some(app) = weak.upgrade() else { continue };
        agents.extend(app.read(cx).agent_rows(cx));
    }
    agents.sort_by_key(|a| std::cmp::Reverse(urgency(a.status)));
    TraySnapshot {
        agents,
        notify_mode: cx.global::<Config>().notify_on_command_finish,
        locale: cx.global::<Config>().locale.resolve(),
    }
}

fn dispatch(action: TrayAction, cx: &mut App) {
    use crate::ui::windows::WindowRegistry;

    let target = match action {
        TrayAction::RevealLeaf { leaf_id } => WindowRegistry::open_windows(cx)
            .into_iter()
            .find(|(_, weak)| {
                weak.upgrade()
                    .is_some_and(|app| app.read(cx).owns_leaf(leaf_id))
            })
            .map(|(workspace, _)| workspace),
        TrayAction::RevealPane { ref target } => WindowRegistry::open_windows(cx)
            .into_iter()
            .find(|(workspace, weak)| {
                crate::core::session::WorkspaceStore::environment_id(cx, *workspace)
                    == target.environment
                    && weak
                        .upgrade()
                        .is_some_and(|app| app.read(cx).owns_pane_identity(target, cx))
            })
            .map(|(workspace, _)| workspace),
        _ => None,
    }
    .or_else(|| {
        (!matches!(action, TrayAction::RevealPane { .. }))
            .then(|| WindowRegistry::most_recent(cx))
            .flatten()
    });

    let Some(workspace) = target else {
        if matches!(action, TrayAction::Quit) {
            cx.quit();
        }
        return;
    };
    let (Some(handle), Some(weak)) = (
        WindowRegistry::window_for(cx, workspace),
        WindowRegistry::app_for(cx, workspace),
    ) else {
        return;
    };
    let _ = handle.update(cx, |_, window, cx| {
        if let Some(app) = weak.upgrade() {
            app.update(cx, |app, cx| app.handle_tray_action(action, window, cx));
        }
    });
}

pub(crate) fn init(cx: &mut App) {
    let (tx, rx) = smol::channel::bounded::<TrayAction>(DISPATCH_CAPACITY);
    if let Ok(mut sender) = SENDER.lock() {
        *sender = Some(tx.clone());
    }

    cx.spawn(async move |cx| {
        while let Ok(action) = rx.recv().await {
            cx.update(|cx| dispatch(action, cx));
        }
    })
    .detach();

    cx.spawn(async move |cx| {
        let mut backend: Option<Backend> = None;
        let mut shown: Option<TraySnapshot> = None;
        const MAX_ATTEMPTS: u32 = 10;
        const RETRY_EVERY: u32 = 30;
        let mut attempts = 0u32;
        let mut cooldown = 0u32;
        loop {
            cx.background_executor().timer(POLL).await;
            let (enabled, snap) =
                cx.update(|cx| (cx.global::<Config>().show_tray_icon, app_snapshot(cx)));
            if !enabled {
                backend = None;
                shown = None;
                attempts = 0;
                cooldown = 0;
                continue;
            }
            if backend.is_none() && attempts < MAX_ATTEMPTS {
                if cooldown > 0 {
                    cooldown -= 1;
                    continue;
                }
                attempts += 1;
                backend = Backend::create(tx.clone(), cx).await;
                if backend.is_none() {
                    cooldown = RETRY_EVERY;
                    if attempts == MAX_ATTEMPTS {
                        log::warn!(
                            "tray icon unavailable after {MAX_ATTEMPTS} attempts; \
                             running without one"
                        );
                    }
                }
                shown = None;
            }
            if let Some(backend) = backend.as_mut()
                && shown.as_ref() != Some(&snap)
            {
                backend.update(&snap);
                shown = Some(snap);
            }
        }
    })
    .detach();
}

fn notification_reveal_action(target: crate::ui::composer::PaneIdentity) -> TrayAction {
    TrayAction::RevealPane { target }
}

#[cfg(test)]
fn dispatch_target_for_reveal(found: bool, has_recent: bool) -> Option<()> {
    found.then_some(()).or_else(|| {
        let _ = has_recent;
        None
    })
}
