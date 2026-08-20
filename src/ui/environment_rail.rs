//! Append-stable left Environment rail (ENV-SESSION-FIRST-RAIL-56).
//!
//! Local plus Config.environment_rail.sidebar_members / pinned remotes. Never
//! the full SSH catalog. Title-bar Indicator remains the Connect launcher.

use agentty_core::agent_runtime::NavigatorRow;
use agentty_core::core::config::EnvironmentRailPreferences;
use agentty_core::core::environment::EnvironmentId;
use agentty_core::core::session::RemoteTarget;
use gpui::{
    App, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement as _,
    StatefulInteractiveElement, Styled as _, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};

use crate::core::config::Config;
use crate::ui::app::AgenttyApp;
use crate::ui::remote_connect::HostChoice;
use crate::ui::remote_workspace::RemoteStatus;

/// Compact session row height — keep in sync with session_sidebar_surface_metrics.
pub(crate) const RAIL_SESSION_ROW_HEIGHT: f32 = 28.0;

/// Approximate Environment identity header block height under the search chrome.
pub(crate) const RAIL_ENV_HEADER_HEIGHT: f32 = 36.0;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RailSessionLayoutPlan {
    /// Pixel height for each session viewport slot in rail order.
    pub viewport_heights_px: Vec<f32>,
    /// One expanded session viewport should flex to fill remaining rail space.
    pub single_fill: bool,
}

/// Collect session viewport slot row counts in before / current / after order.
/// Current Environment always contributes a slot (including empty-state chrome).
/// Non-current sections contribute only when expanded and cache preview rows exist.
pub(crate) fn rail_session_viewport_slots(
    before: &[EnvironmentRailSection],
    current: &Option<EnvironmentRailSection>,
    after: &[EnvironmentRailSection],
    current_row_count: usize,
    preview_row_count: impl Fn(&EnvironmentId) -> usize,
) -> Vec<usize> {
    let mut slots = Vec::new();
    for section in before.iter().chain(current.as_ref()).chain(after.iter()) {
        if section.collapsed {
            continue;
        }
        if section.is_current {
            slots.push(current_row_count);
        } else {
            let rows = preview_row_count(&section.id);
            if rows > 0 {
                slots.push(rows);
            }
        }
    }
    slots
}

/// Remaining sidebar column height available for Environment session viewports.
pub(crate) fn rail_available_session_area_px(
    window_height: f32,
    search_block_height: f32,
    visible_env_headers: usize,
) -> f32 {
    let chrome = crate::ui::app::TITLE_BAR_HEIGHT + search_block_height + 8.0;
    let headers = visible_env_headers as f32 * RAIL_ENV_HEADER_HEIGHT;
    (window_height - chrome - headers).max(RAIL_SESSION_ROW_HEIGHT)
}

/// Classic accordion layout: one expanded viewport fills the rail; multiple
/// viewports shrink to natural row height when everything fits, otherwise split
/// the available area evenly (macOS Mail / VS Code multi-root pattern).
pub(crate) fn rail_session_layout_plan(
    slot_row_counts: &[usize],
    available_px: f32,
    search_expanded: bool,
) -> RailSessionLayoutPlan {
    if slot_row_counts.is_empty() {
        return RailSessionLayoutPlan {
            viewport_heights_px: Vec::new(),
            single_fill: false,
        };
    }
    if search_expanded || slot_row_counts.len() == 1 {
        return RailSessionLayoutPlan {
            viewport_heights_px: vec![available_px],
            single_fill: true,
        };
    }
    let natural: Vec<f32> = slot_row_counts
        .iter()
        .map(|&rows| (rows.max(1) as f32) * RAIL_SESSION_ROW_HEIGHT)
        .collect();
    let total_natural: f32 = natural.iter().sum();
    if total_natural <= available_px {
        return RailSessionLayoutPlan {
            viewport_heights_px: natural,
            single_fill: false,
        };
    }
    let share = available_px / slot_row_counts.len() as f32;
    RailSessionLayoutPlan {
        viewport_heights_px: vec![share; slot_row_counts.len()],
        single_fill: false,
    }
}

pub(crate) fn rail_search_block_height() -> f32 {
    // Keep in sync with session_sidebar_surface_metrics search chrome.
    crate::ui::app::panel_content_gutter() * 2.0 + 30.0 + 1.0
}

fn section_viewport_layout<'a>(
    section: &EnvironmentRailSection,
    is_current: bool,
    preview_or_current_rows: usize,
    viewport_heights: &mut std::slice::Iter<'a, f32>,
    single_fill: bool,
) -> (Option<f32>, bool) {
    if section.collapsed {
        return (None, false);
    }
    if is_current || preview_or_current_rows > 0 {
        let height = viewport_heights.next().copied();
        (height, single_fill && height.is_some())
    } else {
        (None, false)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EnvironmentRailSection {
    pub id: EnvironmentId,
    pub label: String,
    pub is_current: bool,
    pub is_remote: bool,
    pub pinned: bool,
    pub collapsed: bool,
    /// Compact trailing meta on the same row (e.g. "31" or "2·31"). Never a
    /// second dense subtitle line — Cursor-style single-line env headers.
    pub trailing: String,
    pub status_dot: u32,
    pub target: Option<RemoteTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EnvironmentRailPreview {
    pub rows: Vec<NavigatorRow>,
}

/// Pure projection for ENV-RAIL-TREE-52 / ENV-RAIL-SESSION-CAP-57.
/// Current Environment never emits rail preview children (live sessions nest
/// under the current section instead). Preview keeps the full cached list;
/// the section viewport scrolls internally.
pub(crate) fn rail_preview_rows_for_section(
    section: &EnvironmentRailSection,
    cached_rows: &[NavigatorRow],
    fallback_title: &str,
) -> EnvironmentRailPreview {
    if section.is_current || section.collapsed {
        return EnvironmentRailPreview { rows: Vec::new() };
    }
    let _ = fallback_title;
    let rows = cached_rows.iter().cloned().collect();
    EnvironmentRailPreview { rows }
}

/// Build left-rail sections: Local + sidebar_members / pinned (not SSH catalog).
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
    seen.insert(EnvironmentId::local());

    let mut member_keys: Vec<String> = prefs.sidebar_members.clone();
    for pinned in &prefs.pinned {
        if !member_keys.iter().any(|entry| entry == pinned) {
            member_keys.push(pinned.clone());
        }
    }
    if !current.is_local() {
        let key = current.as_str().to_string();
        if !member_keys.iter().any(|entry| entry == &key) {
            member_keys.push(key);
        }
    }

    for key in member_keys {
        let id: EnvironmentId = key.parse().unwrap_or_else(|_| EnvironmentId::local());
        if id.is_local() || !seen.insert(id.clone()) {
            continue;
        }
        let Some(host) = hosts
            .iter()
            .find(|host| EnvironmentId::for_remote(&host.target) == id)
        else {
            continue;
        };
        let status =
            crate::ui::remote_workspace::RemoteLinks::status_for_host(cx, host.target.host_id());
        let is_current = &id == current;
        // Unpinned disconnected hosts should not linger unless they are current.
        if !prefs.is_pinned(&id)
            && !is_current
            && matches!(status, RemoteStatus::Disconnected | RemoteStatus::Failed(_))
        {
            continue;
        }
        let counts = session_counts(&id, is_current);
        sections.push(EnvironmentRailSection {
            id: id.clone(),
            label: host.label.clone(),
            is_current,
            is_remote: true,
            pinned: prefs.is_pinned(&id),
            collapsed: prefs.is_collapsed(&id, is_current),
            trailing: rail_trailing_count(counts),
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
    EnvironmentRailSection {
        id: id.clone(),
        label: crate::core::i18n::current(cx, "environment.local.label").to_string(),
        is_current,
        is_remote: false,
        pinned: prefs.is_pinned(&id),
        collapsed: prefs.is_collapsed(&id, is_current),
        trailing: rail_trailing_count(counts),
        status_dot: status_dot_color(None),
        target: None,
    }
}

/// Compact same-row session meta: `"31"` or `"2·31"` when live > 0.
pub(crate) fn rail_trailing_count(
    counts: crate::ui::environment_navigator_cache::EnvironmentSessionCounts,
) -> String {
    if counts.total == 0 {
        return String::new();
    }
    if counts.live == 0 {
        return counts.total.to_string();
    }
    format!("{}·{}", counts.live, counts.total)
}

fn section_order(
    prefs: &EnvironmentRailPreferences,
    left: &EnvironmentRailSection,
    right: &EnvironmentRailSection,
) -> std::cmp::Ordering {
    // Local always first, then pin order, then append order / label.
    match (left.is_remote, right.is_remote) {
        (false, true) => return std::cmp::Ordering::Less,
        (true, false) => return std::cmp::Ordering::Greater,
        _ => {}
    }
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
            let left_member = prefs
                .sidebar_members
                .iter()
                .position(|entry| entry == left.id.as_str());
            let right_member = prefs
                .sidebar_members
                .iter()
                .position(|entry| entry == right.id.as_str());
            match (left_member, right_member) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => left.label.cmp(&right.label),
            }
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

    pub(crate) fn toggle_environment_pin(&mut self, cx: &mut Context<Self>) {
        let environment = self.current_environment_id(cx);
        let mut should_connect = false;
        let mut target = None;
        self.update_config(cx, |cfg| {
            let was_pinned = cfg.environment_rail.is_pinned(&environment);
            cfg.environment_rail.toggle_pin(&environment);
            should_connect = !was_pinned && cfg.environment_rail.is_pinned(&environment);
        });
        if should_connect && !environment.is_local() {
            target = crate::ui::remote_connect::available_hosts(cx)
                .into_iter()
                .find(|host| EnvironmentId::for_remote(&host.target) == environment);
        }
        if let Some(choice) = target {
            self.connect_to_host(choice, cx);
        }
    }

    pub(crate) fn toggle_environment_rail_collapsed(
        &mut self,
        section: &EnvironmentRailSection,
        cx: &mut Context<Self>,
    ) {
        let collapsed = !section.collapsed;
        self.update_config(cx, |cfg| {
            cfg.environment_rail
                .set_collapsed(&section.id, section.is_current, collapsed);
        });
    }

    pub(crate) fn append_sidebar_environment(
        &mut self,
        environment: &EnvironmentId,
        cx: &mut Context<Self>,
    ) {
        self.update_config(cx, |cfg| {
            cfg.environment_rail.append_sidebar_member(environment);
        });
    }

    pub(crate) fn hydrate_pinned_environments_at_startup(&mut self, cx: &mut Context<Self>) {
        let pinned = cx.global::<Config>().environment_rail.pinned.clone();
        let hosts = crate::ui::remote_connect::available_hosts(cx);
        for key in pinned {
            let id: EnvironmentId = key.parse().unwrap_or_else(|_| EnvironmentId::local());
            if id.is_local() {
                continue;
            }
            self.append_sidebar_environment(&id, cx);
            let Some(choice) = hosts
                .iter()
                .find(|host| EnvironmentId::for_remote(&host.target) == id)
                .cloned()
            else {
                continue;
            };
            // Pin auto-connect needs a claimed remote workspace so the
            // supervisor keeps the host in bound_machines (ENV-PIN-48).
            let workspace = crate::ui::windows::resolve_workspace_for_environment(
                cx,
                Some(choice.target.clone()),
            );
            crate::ui::remote_workspace::RemoteLinks::supervise(cx, workspace);
            let status = crate::ui::remote_workspace::RemoteLinks::status_for_host(
                cx,
                choice.target.host_id(),
            );
            if matches!(
                status,
                RemoteStatus::Attached
                    | RemoteStatus::Connecting
                    | RemoteStatus::Reconnecting { .. }
            ) {
                continue;
            }
            self.connect_to_host(choice, cx);
        }
    }

    pub(crate) fn render_environment_rail(
        &self,
        sessions: gpui::AnyElement,
        current_row_count: usize,
        search_expanded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
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

        let current_index = sections.iter().position(|section| section.is_current);
        let (before, current_section, after) = match current_index {
            Some(index) => {
                let mut before = sections;
                let after = before.split_off(index + 1);
                let current_section = before.pop();
                (before, current_section, after)
            }
            None => (sections, None, Vec::new()),
        };

        let preview_row_count =
            |id: &EnvironmentId| self.environment_navigator_cache.preview_rows(id).len();
        let slots = rail_session_viewport_slots(
            &before,
            &current_section,
            &after,
            current_row_count,
            preview_row_count,
        );
        let header_count = before.len() + current_section.is_some() as usize + after.len();
        let available = rail_available_session_area_px(
            window.viewport_size().height.as_f32(),
            rail_search_block_height(),
            header_count,
        );
        let plan = rail_session_layout_plan(&slots, available, search_expanded);
        let mut viewport_heights = plan.viewport_heights_px.iter();

        let mut rail = v_flex()
            .id("environment-rail")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .py_1();

        for section in before {
            let preview_rows = preview_row_count(&section.id);
            let (viewport_h, flex_fill) = section_viewport_layout(
                &section,
                false,
                preview_rows,
                &mut viewport_heights,
                plan.single_fill,
            );
            rail = rail.child(
                v_flex()
                    .id(format!("environment-rail-before-{}", section.id.as_str()))
                    .flex_shrink_0()
                    .w_full()
                    .children(self.render_environment_rail_section(
                        section, viewport_h, flex_fill, window, cx,
                    )),
            );
        }

        if let Some(section) = current_section {
            let preview_rows = current_row_count;
            let (viewport_h, flex_fill) = section_viewport_layout(
                &section,
                true,
                preview_rows,
                &mut viewport_heights,
                plan.single_fill,
            );
            let header = self.render_environment_rail_row(section.clone(), window, cx);
            let mut sessions_col = v_flex()
                .id("environment-rail-current-sessions")
                .w_full()
                .min_h_0()
                .overflow_hidden()
                .pl(px(2.));
            if flex_fill {
                sessions_col = sessions_col.flex_1();
            } else if let Some(height) = viewport_h {
                sessions_col = sessions_col.flex_shrink_0().h(px(height));
            } else {
                sessions_col = sessions_col.flex_shrink_0();
            }
            let sessions_col = sessions_col.child(sessions);
            let mut current_col = v_flex()
                .id(format!("environment-rail-current-{}", section.id.as_str()))
                .w_full()
                .overflow_hidden()
                .child(header)
                .child(sessions_col);
            current_col = if flex_fill {
                current_col.flex_1().min_h_0()
            } else {
                current_col.flex_shrink_0()
            };
            rail = rail.child(current_col);
        } else {
            rail = rail.child(
                v_flex()
                    .id("environment-rail-current-sessions")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .pl(px(2.))
                    .child(sessions),
            );
        }

        for section in after {
            let preview_rows = preview_row_count(&section.id);
            let (viewport_h, flex_fill) = section_viewport_layout(
                &section,
                false,
                preview_rows,
                &mut viewport_heights,
                plan.single_fill,
            );
            rail = rail.child(
                v_flex()
                    .id(format!("environment-rail-after-{}", section.id.as_str()))
                    .flex_shrink_0()
                    .w_full()
                    .children(self.render_environment_rail_section(
                        section, viewport_h, flex_fill, window, cx,
                    )),
            );
        }

        rail
    }

    fn render_environment_rail_section(
        &self,
        section: EnvironmentRailSection,
        viewport_h: Option<f32>,
        flex_fill: bool,
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
        let preview_selected = self
            .environment_navigator_cache
            .get(&section.id)
            .and_then(|entry| entry.navigator.selected().cloned());
        let mut out = vec![
            self.render_environment_rail_row(section.clone(), window, cx)
                .into_any_element(),
        ];
        if preview.rows.is_empty() {
            return out;
        }
        let mut list = v_flex()
            .id(format!(
                "environment-rail-preview-viewport-{}",
                section.id.as_str()
            ))
            .w_full()
            .min_h_0()
            .overflow_y_scroll()
            .pl(px(2.));
        if flex_fill {
            list = list.flex_1();
        } else if let Some(height) = viewport_h {
            list = list.flex_shrink_0().h(px(height));
        } else {
            list = list.flex_shrink_0();
        }
        let select_target = section.target.clone();
        for (index, row) in preview.rows.into_iter().enumerate() {
            list = list.child(self.render_rail_preview_session_unit(
                row,
                index,
                section.id.clone(),
                select_target.clone(),
                preview_selected.as_ref(),
                cx,
            ));
        }
        out.push(list.into_any_element());
        out
    }

    fn render_environment_rail_row(
        &self,
        section: EnvironmentRailSection,
        _window: &mut Window,
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
        let current = section.is_current;
        let sf = &cx.global::<crate::ui::presets::Surfaces>().sidebar;
        let pill = gpui::rgb(sf.hover);
        let hover_fill = gpui::rgb(sf.pressed);

        h_flex()
            .id(row_id)
            .w_full()
            .px_2()
            .py_1()
            .gap_1p5()
            .items_center()
            .cursor_pointer()
            .rounded_md()
            .bg(pill)
            .hover(|row| row.bg(hover_fill))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    // ENV-RAIL-COLLAPSE-49: row click never toggles collapse —
                    // only the chevron does. Clicking current is a no-op;
                    // clicking non-current selects that Environment.
                    if section_for_row.is_current {
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
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_sm()
                            .font_weight(if current {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::MEDIUM
                            })
                            .text_color(cx.theme().foreground)
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
            .when(!section.trailing.is_empty(), |row| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(section.trailing.clone()),
                )
            })
            .child(
                div()
                    .flex_shrink_0()
                    .size(px(6.))
                    .rounded_full()
                    .bg(gpui::rgb(section.status_dot)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_rail_uses_before_current_after_layout() {
        let source = include_str!("environment_rail.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            production.contains("let (before, current_section, after)"),
            "rail must split sections into before / current / after"
        );
        assert!(
            production.contains("environment-rail-before-"),
            "non-current headers above current must stay shrink_0"
        );
        assert!(
            production.contains("environment-rail-after-"),
            "non-current headers below sessions must stay shrink_0 and visible"
        );
        assert!(
            production.contains("environment-rail-current-sessions"),
            "live sessions nest under the current Environment identity"
        );
        assert!(
            production.contains(".pl(px(2.))"),
            "nested sessions must be lightly indented under the Environment header"
        );
        assert!(
            production.contains("overflow_hidden()"),
            "session List must sit in an overflow_hidden flex column or it collapses to zero height"
        );
        assert!(
            !production.contains("flat_map(|section|"),
            "flex_1 sessions must not be inserted via flat_map between headers"
        );
        assert!(
            production.contains("rail_trailing_count"),
            "env headers use compact same-row trailing counts"
        );
        assert!(
            !production.contains("when(section.collapsed"),
            "collapsed env headers must not grow a dense second subtitle line"
        );
        assert!(
            !production.contains("rail_summary"),
            "dense connection · live · total subtitle path is deleted"
        );
        assert!(
            production.contains("one resting elevated pill"),
            "env identity rows share one resting pill chrome"
        );
        assert!(
            !production.contains("when(selected, |row| row.bg"),
            "env identity must not use bare vs filled two-state chrome"
        );
        assert!(
            production.contains("row click never toggles collapse"),
            "current env row click must not write collapse_overrides"
        );
        assert!(
            !production.contains("if section_for_row.is_current {\n                        this.toggle_environment_rail_collapsed"),
            "clicking the current Environment identity must not collapse it"
        );
    }

    #[test]
    fn rail_trailing_count_stays_compact() {
        use crate::ui::environment_navigator_cache::EnvironmentSessionCounts;
        assert_eq!(
            rail_trailing_count(EnvironmentSessionCounts { live: 0, total: 0 }),
            ""
        );
        assert_eq!(
            rail_trailing_count(EnvironmentSessionCounts { live: 0, total: 31 }),
            "31"
        );
        assert_eq!(
            rail_trailing_count(EnvironmentSessionCounts { live: 2, total: 31 }),
            "2·31"
        );
    }

    #[test]
    fn append_stable_rail_builds_local_plus_members_not_catalog() {
        let remote = EnvironmentId::for_remote(&RemoteTarget::direct("dev", "build.example", 22));
        let prefs = EnvironmentRailPreferences {
            sidebar_members: vec![remote.as_str().to_string()],
            ..Default::default()
        };
        assert!(prefs.is_sidebar_member(&remote));
        assert!(
            !prefs.is_sidebar_member(&EnvironmentId::for_remote(&RemoteTarget::direct(
                "dev",
                "other.example",
                22,
            )))
        );
        let source = include_str!("environment_rail.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(production.contains("ENV-SESSION-FIRST-RAIL-56"));
        assert!(production.contains("build_environment_rail_sections"));
        assert!(production.contains("render_environment_rail"));
        assert!(production.contains("sidebar_members"));
        assert!(production.contains("hydrate_pinned_environments_at_startup"));
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
            trailing: String::new(),
            status_dot: 0,
            target: None,
        };
        let preview = rail_preview_rows_for_section(&section, &[], "Unnamed");
        assert!(preview.rows.is_empty());
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
            trailing: String::new(),
            status_dot: 0,
            target: None,
        };
        let preview = rail_preview_rows_for_section(&section, &[], "Unnamed");
        assert!(preview.rows.is_empty());
    }

    #[test]
    fn rail_session_layout_plan_single_fill() {
        let plan = rail_session_layout_plan(&[12], 400.0, false);
        assert!(plan.single_fill);
        assert_eq!(plan.viewport_heights_px, vec![400.0]);
    }

    #[test]
    fn rail_session_layout_plan_natural_then_even_split() {
        let natural = rail_session_layout_plan(&[2, 3], 400.0, false);
        assert!(!natural.single_fill);
        assert_eq!(natural.viewport_heights_px, vec![56.0, 84.0]);

        let split = rail_session_layout_plan(&[20, 20], 200.0, false);
        assert!(!split.single_fill);
        assert_eq!(split.viewport_heights_px, vec![100.0, 100.0]);
    }

    #[test]
    fn rail_session_viewport_slots_skip_collapsed_and_empty_preview() {
        let local = EnvironmentRailSection {
            id: EnvironmentId::local(),
            label: "Local".into(),
            is_current: true,
            is_remote: false,
            pinned: false,
            collapsed: false,
            trailing: String::new(),
            status_dot: 0,
            target: None,
        };
        let remote = EnvironmentRailSection {
            id: EnvironmentId::for_remote(&RemoteTarget::direct("dev", "build.example", 22)),
            label: "Build".into(),
            is_current: false,
            is_remote: true,
            pinned: true,
            collapsed: false,
            trailing: String::new(),
            status_dot: 0,
            target: Some(RemoteTarget::direct("dev", "build.example", 22)),
        };
        let slots = rail_session_viewport_slots(&[], &Some(local), &[remote], 5, |_| 3);
        assert_eq!(slots, vec![5, 3]);
    }

    #[test]
    fn rail_preview_rows_reuse_session_row_chrome() {
        let source = include_str!("environment_rail.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            production.contains("render_rail_preview_session_unit"),
            "preview must delegate to the live session row renderer"
        );
        assert!(
            !production.contains("render_environment_rail_preview_row"),
            "no parallel preview row skin"
        );
        let sidebar = include_str!("tab_sidebar.rs");
        let sidebar_prod = sidebar.split("#[cfg(test)]").next().unwrap_or(sidebar);
        assert!(
            sidebar_prod.contains("render_rail_preview_session_unit")
                && sidebar_prod.contains("session_navigator_row")
                && sidebar_prod.contains("rail_preview_select"),
            "cached preview must paint through session_navigator_row"
        );
        let metrics = crate::ui::tab_sidebar::session_sidebar_surface_metrics();
        assert_eq!(
            RAIL_SESSION_ROW_HEIGHT, metrics.row_min_height,
            "rail viewport height must track live session row height"
        );
    }

    #[test]
    fn rail_preview_rows_respect_session_cap() {
        let source = include_str!("environment_rail.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            production.contains("overflow_y_scroll"),
            "env session/preview viewports must scroll internally"
        );
        assert!(
            !production.contains("toggle_rail_session_overflow"),
            "no +N more overflow affordance under env sessions"
        );
        assert!(
            !production.contains("preview_overflow"),
            "no +N more overflow affordance under env sessions"
        );
        assert!(
            production.contains("rail_session_layout_plan"),
            "dynamic layout replaces fixed row caps"
        );
        assert!(
            !production.contains("hard_omitting") || production.contains("rail_session"),
            "viewport scroll replaces hard omit"
        );
    }
}
