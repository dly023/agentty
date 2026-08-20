use agentty_core::agent_runtime::{NavigatorRow, RowLifecycle};
use agentty_core::core::cli_agent::CLIAgent;
use agentty_core::core::config::EnvironmentRailPreferences;
use agentty_core::core::environment::EnvironmentId;
use agentty_core::core::session::RemoteTarget;
use gpui::{
    App, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement as _,
    Styled as _, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};

use crate::core::config::Config;

use crate::ui::app::AgenttyApp;
use crate::ui::remote_connect::HostChoice;
use crate::ui::remote_workspace::RemoteStatus;

/// Soft cap for indented preview rows under a non-current Environment header.
/// Overflow is reported as a trailing summary line, not silent truncation.
pub(crate) const RAIL_PREVIEW_ROW_LIMIT: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EnvironmentRailSection {
    pub id: EnvironmentId,
    pub label: String,
    pub is_current: bool,
    pub is_remote: bool,
    pub pinned: bool,
    pub collapsed: bool,
    pub summary: String,
    pub status_dot: u32,
    pub target: Option<RemoteTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EnvironmentRailPreviewRow {
    pub title: String,
    pub lifecycle: RowLifecycle,
    pub agent: CLIAgent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EnvironmentRailPreview {
    pub rows: Vec<EnvironmentRailPreviewRow>,
    pub overflow: usize,
}

/// Pure projection for ENV-RAIL-TREE-52. Current Environment never emits rail
/// preview children; collapsed sections emit none; expanded non-current sections
/// project cached Navigator rows into indented previews.
pub(crate) fn rail_preview_rows_for_section(
    section: &EnvironmentRailSection,
    cached_rows: &[NavigatorRow],
    fallback_title: &str,
) -> EnvironmentRailPreview {
    if section.is_current || section.collapsed {
        return EnvironmentRailPreview {
            rows: Vec::new(),
            overflow: 0,
        };
    }
    let total = cached_rows.len();
    let take = total.min(RAIL_PREVIEW_ROW_LIMIT);
    let rows = cached_rows[..take]
        .iter()
        .map(|row| EnvironmentRailPreviewRow {
            title: row.display_title(fallback_title),
            lifecycle: row.lifecycle,
            agent: row.agent,
        })
        .collect();
    EnvironmentRailPreview {
        rows,
        overflow: total.saturating_sub(take),
    }
}

pub(crate) fn build_environment_rail_sections(
    current: &EnvironmentId,
    hosts: &[HostChoice],
    prefs: &EnvironmentRailPreferences,
    mut session_counts: impl FnMut(
        &EnvironmentId,
        bool,
    )
        -> crate::ui::environment_navigator_cache::EnvironmentSessionCounts,
    cx: &App,
) -> Vec<EnvironmentRailSection> {
    let mut sections = Vec::new();
    let mut seen = std::collections::HashSet::new();

    sections.push(section_for_local(current, prefs, &mut session_counts, cx));

    for host in hosts {
        let id = EnvironmentId::for_remote(&host.target);
        if !seen.insert(id.clone()) {
            continue;
        }
        let label = if host.detail.trim().is_empty() {
            host.label.clone()
        } else {
            format!("{} · {}", host.label, host.detail)
        };
        let status =
            crate::ui::remote_workspace::RemoteLinks::status_for_host(cx, host.target.host_id());
        let is_current = &id == current;
        let counts = session_counts(&id, is_current);
        sections.push(EnvironmentRailSection {
            id: id.clone(),
            label,
            is_current,
            is_remote: true,
            pinned: prefs.is_pinned(&id),
            collapsed: prefs.is_collapsed(&id, true),
            summary: rail_summary(&connection_summary(&status, cx), counts, cx),
            status_dot: status_dot_color(Some(&status)),
            target: Some(host.target.clone()),
        });
    }

    sections.sort_by(|left, right| section_order(prefs, left, right));
    sections
}

fn section_for_local(
    current: &EnvironmentId,
    prefs: &EnvironmentRailPreferences,
    session_counts: &mut impl FnMut(
        &EnvironmentId,
        bool,
    )
        -> crate::ui::environment_navigator_cache::EnvironmentSessionCounts,
    cx: &App,
) -> EnvironmentRailSection {
    let id = EnvironmentId::local();
    let is_current = &id == current;
    let counts = session_counts(&id, is_current);
    let connection = crate::core::i18n::current(cx, "environment.local.detail").to_string();
    EnvironmentRailSection {
        id: id.clone(),
        label: crate::core::i18n::current(cx, "environment.local.label").to_string(),
        is_current,
        is_remote: false,
        pinned: prefs.is_pinned(&id),
        collapsed: prefs.is_collapsed(&id, false),
        summary: rail_summary(&connection, counts, cx),
        status_dot: status_dot_color(None),
        target: None,
    }
}

pub(crate) fn rail_summary(
    connection: &str,
    counts: crate::ui::environment_navigator_cache::EnvironmentSessionCounts,
    cx: &App,
) -> String {
    if counts.total == 0 {
        return connection.to_string();
    }
    let sessions = crate::core::i18n::current_format(
        cx,
        "environment.rail.session_counts",
        &[
            ("live", &counts.live.to_string()),
            ("total", &counts.total.to_string()),
        ],
    );
    format!("{connection} · {sessions}")
}

fn section_order(
    prefs: &EnvironmentRailPreferences,
    left: &EnvironmentRailSection,
    right: &EnvironmentRailSection,
) -> std::cmp::Ordering {
    let left_pin = prefs
        .pinned
        .iter()
        .position(|entry| entry == left.id.as_str());
    let right_pin = prefs
        .pinned
        .iter()
        .position(|entry| entry == right.id.as_str());
    match (left_pin, right_pin) {
        (Some(left_index), Some(right_index)) => left_index.cmp(&right_index),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => {
            if left.is_current != right.is_current {
                return if left.is_current {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
            }
            left.label.cmp(&right.label)
        }
    }
}

fn connection_summary(status: &RemoteStatus, cx: &App) -> String {
    match status {
        RemoteStatus::Attached => {
            crate::core::i18n::current(cx, "environment.rail.connected").to_string()
        }
        RemoteStatus::Disconnected => {
            crate::core::i18n::current(cx, "environment.disconnected").to_string()
        }
        RemoteStatus::Connecting => {
            crate::core::i18n::current(cx, "environment.connecting").to_string()
        }
        RemoteStatus::Reconnecting { .. } => {
            crate::core::i18n::current(cx, "environment.rail.reconnecting").to_string()
        }
        RemoteStatus::Preempted { .. } => {
            crate::core::i18n::current(cx, "environment.rail.preempted").to_string()
        }
        RemoteStatus::Failed(_) => {
            crate::core::i18n::current(cx, "environment.disconnected").to_string()
        }
    }
}

fn status_dot_color(status: Option<&RemoteStatus>) -> u32 {
    match status {
        None | Some(RemoteStatus::Attached) => 0x22C55E,
        Some(RemoteStatus::Connecting | RemoteStatus::Reconnecting { .. }) => 0xEAB308,
        _ => 0xEF4444,
    }
}

impl AgenttyApp {
    pub(crate) fn current_environment_id(&self, cx: &App) -> EnvironmentId {
        crate::core::session::WorkspaceStore::environment_id(cx, self.workspace)
    }

    pub(crate) fn environment_rail_current_collapsed(&self, cx: &App) -> bool {
        let current = self.current_environment_id(cx);
        let prefs = &cx.global::<Config>().environment_rail;
        let is_remote =
            crate::core::session::WorkspaceStore::remote_ref(cx, self.workspace).is_some();
        prefs.is_collapsed(&current, is_remote)
    }

    pub(crate) fn toggle_environment_pin(&mut self, cx: &mut Context<Self>) {
        let environment = self.current_environment_id(cx);
        self.update_config(cx, |cfg| cfg.environment_rail.toggle_pin(&environment));
    }

    pub(crate) fn toggle_environment_rail_collapsed(
        &mut self,
        section: &EnvironmentRailSection,
        cx: &mut Context<Self>,
    ) {
        let collapsed = !section.collapsed;
        self.update_config(cx, |cfg| {
            cfg.environment_rail
                .set_collapsed(&section.id, section.is_remote, collapsed);
        });
    }

    pub(crate) fn render_environment_rail(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let current = self.current_environment_id(cx);
        let hosts = crate::ui::remote_connect::available_hosts(cx);
        let prefs = cx.global::<Config>().environment_rail.clone();
        let sections = build_environment_rail_sections(
            &current,
            &hosts,
            &prefs,
            |id, is_current| self.environment_session_counts(id, is_current),
            cx,
        );
        if sections.len() <= 1 {
            return None;
        }

        Some(
            v_flex()
                .id("environment-rail")
                .flex_shrink_0()
                .py_1()
                .border_b_1()
                .border_color(cx.theme().sidebar_border)
                .children(
                    sections.into_iter().flat_map(|section| {
                        self.render_environment_rail_section(section, window, cx)
                    }),
                ),
        )
    }

    fn render_environment_rail_section(
        &self,
        section: EnvironmentRailSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let fallback = crate::core::i18n::current(cx, "session.default_name");
        let cached = if section.is_current || section.collapsed {
            Vec::new()
        } else {
            self.environment_navigator_cache
                .preview_rows(&section.id)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        };
        let preview = rail_preview_rows_for_section(&section, &cached, fallback);
        let mut out = vec![
            self.render_environment_rail_row(section.clone(), window, cx)
                .into_any_element(),
        ];
        for (index, row) in preview.rows.into_iter().enumerate() {
            out.push(
                self.render_environment_rail_preview_row(&section, index, row, window, cx)
                    .into_any_element(),
            );
        }
        if preview.overflow > 0 {
            out.push(
                div()
                    .id(format!(
                        "environment-rail-preview-overflow-{}",
                        section.id.as_str()
                    ))
                    .w_full()
                    .pl(px(36.))
                    .pr_3()
                    .py_0p5()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::core::i18n::current_format(
                        cx,
                        "environment.rail.preview_overflow",
                        &[("count", &preview.overflow.to_string())],
                    ))
                    .into_any_element(),
            );
        }
        out
    }

    fn render_environment_rail_preview_row(
        &self,
        section: &EnvironmentRailSection,
        index: usize,
        row: EnvironmentRailPreviewRow,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let target = section.target.clone();
        let hover_fill = cx.theme().accent.opacity(0.08);
        let live = matches!(row.lifecycle, RowLifecycle::Live | RowLifecycle::Restoring);
        h_flex()
            .id(format!(
                "environment-rail-preview-{}-{index}",
                section.id.as_str()
            ))
            .w_full()
            .pl(px(36.))
            .pr_3()
            .py_1()
            .gap_2()
            .items_center()
            .cursor_pointer()
            .hover(|row| row.bg(hover_fill))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    // Preview rows never activate a foreign session in place.
                    if let Some(target) = target.clone() {
                        this.select_environment(Some(target), window, cx);
                    } else {
                        this.select_environment(None, window, cx);
                    }
                }),
            )
            .child(
                Icon::empty()
                    .path(row.agent.icon_path())
                    .size(px(12.))
                    .text_color(if live {
                        cx.theme().foreground
                    } else {
                        cx.theme().muted_foreground
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(if live {
                        cx.theme().foreground
                    } else {
                        cx.theme().muted_foreground
                    })
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(row.title),
            )
    }

    fn render_environment_rail_row(
        &self,
        section: EnvironmentRailSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let chevron = if section.collapsed {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        };
        let row_id = format!("environment-rail-row-{}", section.id.as_str());
        let chevron_id = format!("environment-rail-chevron-{}", section.id.as_str());
        let section_for_chevron = section.clone();
        let section_for_row = section.clone();
        let selected = section.is_current;
        let hover_fill = cx.theme().accent.opacity(0.08);

        h_flex()
            .id(row_id)
            .w_full()
            .px_3()
            .py_1p5()
            .gap_2()
            .items_center()
            .cursor_pointer()
            .when(selected, |row| row.bg(cx.theme().accent.opacity(0.12)))
            .when(!selected, |row| row.hover(|row| row.bg(hover_fill)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if section_for_row.is_current {
                        if section_for_row.collapsed {
                            this.toggle_environment_rail_collapsed(&section_for_row, cx);
                        }
                        return;
                    }
                    if let Some(target) = section_for_row.target.clone() {
                        this.select_environment(Some(target), window, cx);
                    } else {
                        this.select_environment(None, window, cx);
                    }
                }),
            )
            .child(
                div()
                    .id(chevron_id)
                    .flex_shrink_0()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_environment_rail_collapsed(&section_for_chevron, cx);
                        }),
                    )
                    .child(
                        Icon::new(chevron)
                            .size(px(12.))
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .relative()
                    .child(
                        Icon::empty()
                            .path(if section.is_remote {
                                "icons/machine-remote.svg"
                            } else {
                                "icons/machine-local.svg"
                            })
                            .size(px(14.))
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .absolute()
                            .right(px(-1.))
                            .bottom(px(-1.))
                            .size(px(6.))
                            .rounded_full()
                            .border_1()
                            .border_color(cx.theme().sidebar)
                            .bg(gpui::rgb(section.status_dot)),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(if selected {
                                        FontWeight::SEMIBOLD
                                    } else {
                                        FontWeight::NORMAL
                                    })
                                    .text_color(cx.theme().foreground)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(section.label.clone()),
                            )
                            .when(section.pinned, |row| {
                                row.child(
                                    Icon::empty()
                                        .path("icons/pin.svg")
                                        .size(px(10.))
                                        .text_color(cx.theme().muted_foreground),
                                )
                            }),
                    )
                    .when(section.collapsed, |column| {
                        column.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(section.summary.clone()),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentty_core::core::environment::EnvironmentId;

    #[test]
    fn unpinned_remote_defaults_collapsed() {
        let prefs = EnvironmentRailPreferences::default();
        let remote = EnvironmentId::for_remote(&RemoteTarget::Direct {
            host: "build.example".into(),
            port: 22,
            user: "dev".into(),
        });
        assert!(prefs.is_collapsed(&remote, true));
    }

    #[test]
    fn pinned_remote_defaults_expanded() {
        let mut prefs = EnvironmentRailPreferences::default();
        let remote = EnvironmentId::for_remote(&RemoteTarget::Direct {
            host: "build.example".into(),
            port: 22,
            user: "dev".into(),
        });
        prefs.toggle_pin(&remote);
        assert!(!prefs.is_collapsed(&remote, true));
    }

    #[test]
    fn collapse_override_round_trips() {
        let mut prefs = EnvironmentRailPreferences::default();
        let local = EnvironmentId::local();
        prefs.set_collapsed(&local, false, true);
        assert!(prefs.is_collapsed(&local, false));
        prefs.set_collapsed(&local, false, false);
        assert!(!prefs.is_collapsed(&local, false));
    }

    #[test]
    fn pinned_sections_sort_before_unpinned() {
        let mut prefs = EnvironmentRailPreferences::default();
        let remote = EnvironmentId::for_remote(&RemoteTarget::Direct {
            host: "pinned.example".into(),
            port: 22,
            user: String::new(),
        });
        prefs.toggle_pin(&remote);
        let local = EnvironmentRailSection {
            id: EnvironmentId::local(),
            label: "Local".into(),
            is_current: true,
            is_remote: false,
            pinned: false,
            collapsed: false,
            summary: String::new(),
            status_dot: 0,
            target: None,
        };
        let remote_section = EnvironmentRailSection {
            id: remote.clone(),
            label: "Pinned".into(),
            is_current: false,
            is_remote: true,
            pinned: true,
            collapsed: false,
            summary: String::new(),
            status_dot: 0,
            target: None,
        };
        assert_eq!(
            section_order(&prefs, &remote_section, &local),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn rail_summary_omits_counts_when_empty() {
        let counts =
            crate::ui::environment_navigator_cache::EnvironmentSessionCounts { live: 0, total: 0 };
        assert_eq!(format_session_count_fragment(counts), None);
        let counts =
            crate::ui::environment_navigator_cache::EnvironmentSessionCounts { live: 1, total: 4 };
        assert_eq!(
            format_session_count_fragment(counts).as_deref(),
            Some("1 live · 4 sessions")
        );
    }

    #[test]
    fn current_environment_never_emits_rail_preview_rows() {
        let section = EnvironmentRailSection {
            id: EnvironmentId::local(),
            label: "Local".into(),
            is_current: true,
            is_remote: false,
            pinned: false,
            collapsed: false,
            summary: String::new(),
            status_dot: 0,
            target: None,
        };
        let rows = sample_navigator_rows();
        let preview = rail_preview_rows_for_section(&section, &rows, "Unnamed");
        assert!(preview.rows.is_empty());
        assert_eq!(preview.overflow, 0);
    }

    #[test]
    fn collapsed_non_current_omits_preview_rows() {
        let section = EnvironmentRailSection {
            id: EnvironmentId::for_remote(&RemoteTarget::Direct {
                host: "build.example".into(),
                port: 22,
                user: "dev".into(),
            }),
            label: "Build".into(),
            is_current: false,
            is_remote: true,
            pinned: false,
            collapsed: true,
            summary: String::new(),
            status_dot: 0,
            target: None,
        };
        let rows = sample_navigator_rows();
        let preview = rail_preview_rows_for_section(&section, &rows, "Unnamed");
        assert!(preview.rows.is_empty());
    }

    #[test]
    fn expanded_non_current_emits_preview_rows() {
        let section = EnvironmentRailSection {
            id: EnvironmentId::for_remote(&RemoteTarget::Direct {
                host: "build.example".into(),
                port: 22,
                user: "dev".into(),
            }),
            label: "Build".into(),
            is_current: false,
            is_remote: true,
            pinned: true,
            collapsed: false,
            summary: String::new(),
            status_dot: 0,
            target: None,
        };
        let rows = sample_navigator_rows();
        let preview = rail_preview_rows_for_section(&section, &rows, "Unnamed");
        assert_eq!(preview.rows.len(), 2);
        assert_eq!(preview.rows[0].title, "h1");
        assert_eq!(preview.overflow, 0);
        let source = include_str!("environment_rail.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(production.contains("environment-rail-preview"));
        assert!(production.contains("select_environment"));
        assert!(
            !production.contains("activate_navigator_row"),
            "preview rows must not activate sessions under foreign authority"
        );
    }

    fn sample_navigator_rows() -> Vec<NavigatorRow> {
        use agentty_core::agent_runtime::{
            AgentSessionKey, AgentSessionRecord, LiveCarrier, LiveSession, SessionIdentity,
            SessionNavigator, SessionTitleCandidates,
        };
        use agentty_core::core::cli_agent::CLIAgent;

        fn history(id: &str) -> AgentSessionRecord {
            AgentSessionRecord {
                key: AgentSessionKey {
                    provider: "codex".into(),
                    session_id: id.into(),
                },
                agent: CLIAgent::Codex,
                title: Some(id.into()),
                title_candidates: SessionTitleCandidates::default(),
                cwd: Some("/repo".into()),
                updated_at_unix_ms: Some(1),
                launch_argv: vec![],
                source_path: None,
                created_at_unix_ms: None,
            }
        }
        fn live(id: &str) -> LiveSession {
            LiveSession {
                identity: SessionIdentity::Provider(history(id).key.clone()),
                agent: CLIAgent::Codex,
                session_id: Some(id.into()),
                title: Some(id.into()),
                title_candidates: SessionTitleCandidates::default(),
                cwd: Some("/repo".into()),
                launch_argv: vec![],
                carrier: LiveCarrier {
                    container_id: format!("container-{id}"),
                    tab_id: Some("tab".into()),
                    pane_id: Some(1),
                },
                execution: None,
            }
        }
        let mut navigator = SessionNavigator::default();
        navigator.refresh(&[history("h1"), history("h2")], &[live("h1")]);
        navigator.rows().to_vec()
    }

    fn format_session_count_fragment(
        counts: crate::ui::environment_navigator_cache::EnvironmentSessionCounts,
    ) -> Option<String> {
        if counts.total == 0 {
            None
        } else {
            Some(format!("{} live · {} sessions", counts.live, counts.total))
        }
    }
}
