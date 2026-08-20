use gpui::{
    Anchor, Axis, Context, DismissEvent, Entity, MouseButton, PromptLevel, Window, deferred, div,
    linear_color_stop, linear_gradient, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::hover_card::HoverCard;
use gpui_component::input::Input;
use gpui_component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, InteractiveElementExt as _, Sizable as _, h_flex, v_flex,
};

use crate::ui::app::{AgenttyApp, TITLE_BAR_HEIGHT};
use crate::ui::reorder::{self, Reorder, Surface, TabDragHitZone, TabDragIntent};

const MIN_SIDEBAR_WIDTH: f32 = 180.;

const GRAB_HANDLE_W: f32 = 48.;
const MAX_SIDEBAR_WIDTH_RATIO: f32 = 0.5;

const RESIZE_HANDLE_WIDTH: f32 = 8.;
/// Snappy open delay (SESSION-HOVER-DETAILS-13). Long enough to ignore
/// cursor-skim passes, short enough that the detail card feels immediate.
const SESSION_HOVER_CARD_OPEN_DELAY: std::time::Duration = std::time::Duration::from_millis(120);
/// Close delay keeps the pointer-migration safe region usable without lag.
const SESSION_HOVER_CARD_CLOSE_DELAY: std::time::Duration = std::time::Duration::from_millis(120);
const SESSION_HOVER_CARD_GAP: f32 = 8.;

/// Environment rail indents nested session rows under section headers.
pub(crate) const RAIL_SESSION_INDENT: f32 = 2.0;

/// Lead inset that parks the hover detail card just past the sidebar split
/// (SESSION-HOVER-DETAILS-13 trailing sidecar). Uses the rendered panel width
/// and nested row insets — not the persisted config width before clamping.
fn session_hover_sidecar_lead(panel_width: f32) -> f32 {
    let metrics = session_sidebar_surface_metrics();
    let left_inset = RAIL_SESSION_INDENT + metrics.unit_outer_pad + metrics.row_pad_x;
    let right_inset = metrics.unit_outer_pad + metrics.row_pad_x;
    let trigger_width = (panel_width - left_inset - right_inset).max(0.);
    trigger_width + SESSION_HOVER_CARD_GAP
}

/// Hover detail yields to an open row context menu (SESSION-HOVER-DETAILS-13).
fn session_hover_allowed(context_menu_open: bool) -> bool {
    !context_menu_open
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SessionSidebarSurfaceMetrics {
    pub(crate) outer_gutter: f32,
    pub(crate) search_height: f32,
    pub(crate) row_min_height: f32,
    pub(crate) unit_gap: f32,
    /// Horizontal pad on each session unit under the Environment rail
    /// (SESSION-SIDEBAR-ROW-DENSITY-29 nested indent — not a second panel gutter).
    pub(crate) unit_outer_pad: f32,
    pub(crate) row_pad_x: f32,
    pub(crate) row_pad_y: f32,
    pub(crate) icon_size: f32,
    pub(crate) icon_glyph: f32,
    pub(crate) icon_radius: f32,
    pub(crate) icon_text_gap: f32,
    pub(crate) title_subtitle_gap: f32,
    pub(crate) subtitle_size: f32,
    pub(crate) text_fade_width: f32,
}

pub(crate) fn session_sidebar_surface_metrics() -> SessionSidebarSurfaceMetrics {
    SessionSidebarSurfaceMetrics {
        // Search chrome keeps the shared panel gutter. Nested session units
        // under the Environment rail use `unit_outer_pad` instead so rail
        // indent + unit pad + row pad stay in the 6–12px icon inset band.
        outer_gutter: crate::ui::app::panel_content_gutter(),
        search_height: 30.0,
        row_min_height: 28.0,
        unit_gap: 1.0,
        unit_outer_pad: 4.0,
        row_pad_x: 2.0,
        row_pad_y: 1.0,
        icon_size: 16.0,
        icon_glyph: 9.0,
        icon_radius: 4.0,
        icon_text_gap: 6.0,
        title_subtitle_gap: 1.0,
        subtitle_size: 10.0,
        text_fade_width: 14.0,
    }
}

/// Trailing overflow fade matched to the idle / hover / selected row surface
/// (SESSION-SIDEBAR-ROW-DENSITY-29; Cursor-style, no ellipsis).
fn session_row_text_fade(selected: bool, cx: &gpui::App) -> gpui::Div {
    let metrics = session_sidebar_surface_metrics();
    let sf = &cx.global::<crate::ui::presets::Surfaces>().sidebar;
    let solid: gpui::Hsla = if selected {
        gpui::rgb(sf.selected).into()
    } else {
        gpui::rgb(sf.base).into()
    };
    let hover: gpui::Hsla = gpui::rgb(sf.hover).into();
    let mut from = solid;
    from.a = 0.;
    let mut hover_from = hover;
    hover_from.a = 0.;
    div()
        .debug_selector(|| "SESSION_ROW_TEXT_FADE".into())
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .w(px(metrics.text_fade_width))
        .bg(linear_gradient(
            90.,
            linear_color_stop(from, 0.),
            linear_color_stop(solid, 1.),
        ))
        .when(!selected, |fade| {
            fade.group_hover("agent-session-row", move |fade| {
                fade.bg(linear_gradient(
                    90.,
                    linear_color_stop(hover_from, 0.),
                    linear_color_stop(hover, 1.),
                ))
            })
        })
}

/// Idle / hover / selected / keyboard-cursor fills from Surfaces.sidebar.
/// Only selected (or keyboard-cursor) rows use elevated pill chrome; idle rows
/// rest flat on sidebar.base and gain hover fill on pointer (SESSION-SIDEBAR-ROW-DENSITY-29).
/// Never theme().muted — contrast floors live on this ladder (POLISH-008).
pub(crate) fn session_row_surface_fill(
    selected: bool,
    keyboard_cursor: bool,
    sidebar: &crate::ui::presets::Surface,
) -> u32 {
    if selected {
        sidebar.selected
    } else if keyboard_cursor {
        sidebar.cursor
    } else {
        sidebar.base
    }
}

pub(crate) fn session_row_hover_fill(sidebar: &crate::ui::presets::Surface) -> u32 {
    sidebar.hover
}

/// Distance from the sidebar edge to the row icon
/// (outer gutter + row pad; SESSION-SIDEBAR-ROW-DENSITY-29).
fn session_row_content_inset(metrics: &SessionSidebarSurfaceMetrics) -> f32 {
    metrics.unit_outer_pad + metrics.row_pad_x
}

/// Selected / keyboard / idle rows always reserve the same border width
/// so chrome never shifts icon or title horizontally (SESSION-SIDEBAR-ROW-DENSITY-29).
fn session_row_reserves_border(_selected: bool, _keyboard_cursor: bool) -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionUnitVisualKind {
    Single,
    SplitGroup,
}

fn session_unit_visual_kind(row_count: usize) -> SessionUnitVisualKind {
    if row_count > 1 {
        SessionUnitVisualKind::SplitGroup
    } else {
        SessionUnitVisualKind::Single
    }
}

fn session_search_surface(cx: &gpui::App) -> gpui::Div {
    let metrics = session_sidebar_surface_metrics();
    crate::ui::panel_chrome::side_panel_search_surface(cx)
        .debug_selector(|| "SESSION_SEARCH_SURFACE".into())
        .mx(px(metrics.outer_gutter))
        .mb(px(metrics.unit_gap))
}

fn session_unit_stack_surface(grouped: bool, group_selected: bool, cx: &gpui::App) -> gpui::Div {
    let sf = &cx.global::<crate::ui::presets::Surfaces>().sidebar;
    let group_fill: gpui::Hsla = gpui::rgb(sf.hover).into();
    v_flex()
        .relative()
        .w_full()
        .gap(if grouped { px(1.) } else { px(0.) })
        .when(grouped, |stack| {
            stack
                .debug_selector(|| "SESSION_SPLIT_GROUP".into())
                .p(px(3.))
                .pl(px(7.))
                .rounded_lg()
                .border_1()
                .border_color(if group_selected {
                    cx.theme().ring.opacity(0.55)
                } else {
                    cx.theme().sidebar_border
                })
                .bg(group_fill.opacity(0.35))
                .child(
                    div()
                        .absolute()
                        .left(px(2.))
                        .top(px(6.))
                        .bottom(px(6.))
                        .w(px(2.))
                        .rounded_full()
                        .bg(cx.theme().accent.opacity(0.7)),
                )
        })
}

/// Status affordance for one Session Navigator row. Historical rows resume on
/// the canonical row click, so they do not need a second hover action glyph.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SessionRowHoverAffordance {
    None,
    RestoringStatus,
}

pub(crate) fn session_row_hover_affordance(
    active: bool,
    badge: Option<agentty_core::agent_runtime::ExecutionBadge>,
) -> SessionRowHoverAffordance {
    if badge == Some(agentty_core::agent_runtime::ExecutionBadge::Restoring) {
        SessionRowHoverAffordance::RestoringStatus
    } else {
        let _ = active;
        SessionRowHoverAffordance::None
    }
}

/// Empty projection placeholder kinds (SESSION-EMPTY-STATE-SURFACE-28). Each
/// state maps to one distinct glyph so the four states are distinguishable
/// without reading the message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SessionEmptyStateKind {
    Discovering,
    ScanError,
    Empty,
    NoMatch,
}

pub(crate) fn session_empty_state_kind(
    discovering: bool,
    has_error: bool,
    query_empty: bool,
) -> SessionEmptyStateKind {
    if discovering {
        SessionEmptyStateKind::Discovering
    } else if has_error {
        SessionEmptyStateKind::ScanError
    } else if query_empty {
        SessionEmptyStateKind::Empty
    } else {
        SessionEmptyStateKind::NoMatch
    }
}

impl SessionEmptyStateKind {
    pub(crate) fn icon(self) -> IconName {
        match self {
            SessionEmptyStateKind::Discovering => IconName::LoaderCircle,
            SessionEmptyStateKind::ScanError => IconName::TriangleAlert,
            SessionEmptyStateKind::Empty => IconName::Inbox,
            SessionEmptyStateKind::NoMatch => IconName::Search,
        }
    }
}

#[derive(Clone)]
struct DragSessionUnit;
#[derive(Clone)]
struct DragSessionIcon;
#[derive(Clone)]
struct DragSessionBody;

impl gpui::Render for DragSessionUnit {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl gpui::Render for DragSessionIcon {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl gpui::Render for DragSessionBody {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl AgenttyApp {
    /// Resolve a Navigator unit to a 1-pane live TabId for same-Environment merge.
    pub(crate) fn mergeable_tab_id_for_unit(
        &self,
        unit_index: usize,
        projection: &crate::ui::session_navigator::SessionViewportProjection,
    ) -> Option<agentty_core::core::machine::TabId> {
        let rows = projection.rows_for_unit(unit_index, &self.session_navigator)?;
        if rows.len() != 1 {
            return None;
        }
        let tab_id = rows[0].carrier.as_ref()?.tab_id.as_ref()?;
        let tab = self
            .tabs
            .iter()
            .find(|tab| Some(tab.tree_id.get().to_string()) == Some(tab_id.clone()))?;
        if tab.pane.terminals().len() == 1 && tab.pane.leaves().len() == 1 {
            Some(tab.tree_id.get())
        } else {
            None
        }
    }

    pub(crate) fn tab_sidebar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        self.ensure_session_navigator_scan(cx);
        self.rebuild_session_navigator(cx);

        let max_width = (window.viewport_size().width.as_f32() * MAX_SIDEBAR_WIDTH_RATIO)
            .max(MIN_SIDEBAR_WIDTH);
        let width = self.sidebar_width.get().clamp(MIN_SIDEBAR_WIDTH, max_width);
        self.sidebar_hover_panel_width.set(width);
        let query = self.sidebar_search.read(cx).value().trim().to_lowercase();
        let search_active = !query.is_empty();
        let projection = self.session_viewport_projection(cx);
        let current_row_count = projection.len();
        if projection != self.session_viewport_projection {
            let (old_range, new_count) = projection.splice_delta(&self.session_viewport_projection);
            self.session_list_state.splice(old_range, new_count);
            self.session_viewport_projection = projection.clone();
        }
        self.normalize_session_keyboard_cursor(cx);

        let preview = query
            .is_empty()
            .then(|| {
                reorder::preview(
                    &self.reorder,
                    &Surface::Navigator,
                    projection.len(),
                    window.mouse_position(),
                )
            })
            .flatten();
        if let Some(preview) = &preview {
            let tab_intent = self.reorder.borrow().as_ref().and_then(|r| r.tab_intent);
            match tab_intent {
                Some(crate::ui::reorder::TabDragIntent::Merge) => {
                    let source_tab = self.reorder.borrow().as_ref().and_then(|r| r.source_tab);
                    let target_tab = preview.hovered.and_then(|hovered| {
                        self.mergeable_tab_id_for_unit(hovered, &projection)
                            .filter(|tab| Some(*tab) != source_tab)
                    });
                    reorder::set_tab_pending(
                        &self.reorder,
                        &Surface::Navigator,
                        preview.order.clone(),
                        preview.target,
                        target_tab,
                    );
                }
                Some(crate::ui::reorder::TabDragIntent::Reorder) => {
                    reorder::set_tab_pending(
                        &self.reorder,
                        &Surface::Navigator,
                        preview.order.clone(),
                        preview.target,
                        None,
                    );
                }
                None => {
                    reorder::set_pending(
                        &self.reorder,
                        &Surface::Navigator,
                        preview.order.clone(),
                        preview.target,
                        crate::ui::reorder::ReorderIntent::Reorder,
                    );
                }
            }
        }

        let list = if projection.is_empty() {
            let message = if self.session_refresh.is_inflight() {
                crate::core::i18n::current(cx, "session.discovering").to_string()
            } else if let Some(error) = &self.session_scan_error {
                error.clone()
            } else if query.is_empty() {
                crate::core::i18n::current(cx, "session.empty").to_string()
            } else {
                crate::core::i18n::current(cx, "session.no_match").to_string()
            };
            let empty_kind = session_empty_state_kind(
                self.session_refresh.is_inflight(),
                self.session_scan_error.is_some(),
                query.is_empty(),
            );
            crate::ui::panel_chrome::side_panel_empty_surface(empty_kind.icon(), message, None, cx)
                .id("agent-session-navigator-list")
                .flex_1()
                .min_h_0()
                .into_any_element()
        } else {
            let projection = projection.clone();
            let preview = preview.clone();
            let list_state = self.session_list_state.clone();
            div()
                .id("agent-session-navigator-list")
                .flex_1()
                .min_h_0()
                .w_full()
                .on_scroll_wheel(cx.listener(|_, _: &gpui::ScrollWheelEvent, _window, cx| {
                    cx.notify();
                }))
                .child(
                    gpui::list(
                        list_state.clone(),
                        cx.processor(move |this, unit_index, _window, cx| {
                            this.session_navigator_unit(
                                unit_index,
                                &projection,
                                preview.as_ref(),
                                &list_state,
                                query.is_empty(),
                                cx,
                            )
                        }),
                    )
                    .size_full(),
                )
                .into_any_element()
        };

        let controls = h_flex()
            .flex_shrink_0()
            .h(px(TITLE_BAR_HEIGHT))
            .items_center()
            .gap(px(2.))
            .pl(px(crate::ui::app::TITLE_BAR_LEAD))
            .pr(px(crate::ui::app::panel_split_chrome_inset()))
            .justify_end()
            .when_some(crate::ui::app::window_mark(), |row, mark| {
                row.child(div().flex_shrink_0().child(mark))
            })
            .child(div().flex_1().min_w(px(GRAB_HANDLE_W)))
            .child(
                crate::ui::tab_strip::chrome_tile(
                    Button::new("session-refresh").icon(Icon::empty().path("icons/refresh.svg")),
                    false,
                    cx,
                )
                .rounded_lg()
                .tooltip(crate::core::i18n::current(cx, "session.refresh"))
                .on_click(cx.listener(|this, _, _window, cx| {
                    this.refresh_session_navigator(cx);
                })),
            )
            .child(
                crate::ui::tab_strip::chrome_tile(
                    Button::new("sidebar-collapse")
                        .icon(Icon::empty().path("icons/panel-left.svg")),
                    false,
                    cx,
                )
                .rounded_lg()
                .tooltip(crate::core::i18n::current(cx, "sidebar.hide"))
                .on_click(cx.listener(|this, _, _window, cx| this.toggle_left_panel(cx))),
            );

        let search_bar = session_search_surface(cx)
            .child(
                Icon::new(IconName::Search)
                    .size(px(12.))
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "up" => {
                                cx.stop_propagation();
                                this.move_session_keyboard_cursor(-1, cx);
                            }
                            "down" => {
                                cx.stop_propagation();
                                this.move_session_keyboard_cursor(1, cx);
                            }
                            "enter" => {
                                cx.stop_propagation();
                                this.activate_session_keyboard_cursor(window, cx);
                            }
                            "escape" => {
                                cx.stop_propagation();
                                this.escape_session_keyboard_cursor(window, cx);
                            }
                            _ => {}
                        }
                    }))
                    .child(Input::new(&self.sidebar_search).appearance(false).pl_0()),
            );

        let handle_active = self.sidebar_dragging.get();
        let handle = div()
            .group("sidebar-resize")
            .occlude()
            .absolute()
            .top_0()
            .right(px(-(RESIZE_HANDLE_WIDTH / 2.)))
            .w(px(RESIZE_HANDLE_WIDTH))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_col_resize()
            .child(
                div()
                    .w(px(1.))
                    .h_full()
                    .when(handle_active, |d| d.bg(cx.theme().drag_border))
                    .group_hover("sidebar-resize", |s| s.bg(cx.theme().drag_border)),
            )
            .on_mouse_down(MouseButton::Left, {
                let dragging = self.sidebar_dragging.clone();
                move |_ev, window, _cx| {
                    dragging.set(true);
                    window.refresh();
                }
            });

        let more_below = !projection.is_empty()
            && crate::ui::scrollbar::list_has_overflow_below(&self.session_list_state);
        let sessions = crate::ui::scrollbar::with_vertical_scrollbar_overflow(
            "agent-session-navigator-scrollbar",
            list,
            &self.session_list_state,
            more_below,
            cx.theme().sidebar,
        );

        div()
            .relative()
            .flex_shrink_0()
            .w(px(width))
            .h_full()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                v_flex()
                    .size_full()
                    .child(crate::ui::app::title_bar_drag(
                        controls.id("sidebar-titlebar-drag"),
                        "sidebar-titlebar-drag",
                        window,
                        cx,
                    ))
                    .child(search_bar)
                    // ENV-SESSION-FIRST-RAIL-56: sessions nest under the current
                    // Environment identity row inside the append-stable rail.
                    .child(self.render_environment_rail(
                        sessions.into_any_element(),
                        current_row_count,
                        search_active,
                        window,
                        cx,
                    )),
            )
            .child(handle)
    }

    fn session_navigator_unit(
        &self,
        unit_index: usize,
        projection: &crate::ui::session_navigator::SessionViewportProjection,
        preview: Option<&crate::ui::reorder::Preview>,
        list_state: &gpui::ListState,
        draggable: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(unit) = projection.unit(unit_index) else {
            return div().into_any_element();
        };
        let Some(rows) = projection.rows_for_unit(unit_index, &self.session_navigator) else {
            return div().into_any_element();
        };
        let surface_metrics = session_sidebar_surface_metrics();
        let visual_kind = session_unit_visual_kind(rows.len());
        let grouped = visual_kind == SessionUnitVisualKind::SplitGroup;
        let group_selected = rows.iter().any(|row| {
            self.session_navigator
                .selected()
                .is_some_and(|selected| selected == &row.row_id)
        });
        let unit_key = unit
            .row_ids
            .first()
            .map(|row_id| row_id.as_str())
            .unwrap_or("stale");
        let mergeable_tab = self.mergeable_tab_id_for_unit(unit_index, projection);
        let mut stack = session_unit_stack_surface(grouped, group_selected, cx);
        for (row_index, row) in rows.into_iter().enumerate() {
            let icon_merge = draggable
                .then_some(mergeable_tab)
                .flatten()
                .filter(|_| row_index == 0 && !grouped);
            let body_reorder = draggable && icon_merge.is_some();
            stack = stack.child(self.session_navigator_row(
                row,
                unit_index,
                projection.len(),
                list_state,
                icon_merge,
                body_reorder,
                cx,
                None,
                None,
                None,
            ));
        }
        let group = v_flex()
            .id(gpui::SharedString::from(format!(
                "session-reorder-unit-{unit_key}"
            )))
            .debug_selector(move || format!("SESSION_UNIT_{unit_index}"))
            .relative()
            .w_full()
            .px(px(surface_metrics.unit_outer_pad))
            .pb(px(surface_metrics.unit_gap))
            .child(stack);
        let dragged = preview.is_some_and(|preview| preview.from == unit_index);
        // Split groups and non-mergeable units keep whole-unit body reorder.
        let group = group
            .when(draggable && (grouped || mergeable_tab.is_none()), |group| {
                group.on_drag(DragSessionUnit, {
                    let state = self.reorder.clone();
                    let list_state = list_state.clone();
                    let unit_count = projection.len();
                    move |_drag, grab, _window, cx| {
                        cx.stop_propagation();
                        let rects = (0..unit_count)
                            .map(|index| list_state.bounds_for_item(index))
                            .collect();
                        *state.borrow_mut() = Some(Reorder::new_projected(
                            Surface::Navigator,
                            unit_index,
                            rects,
                            Axis::Vertical,
                            px(1.5),
                            grab,
                        ));
                        cx.new(|_| DragSessionUnit)
                    }
                })
            })
            .when(dragged, |group| group.opacity(0.75));
        match preview {
            Some(preview) if preview.from == unit_index => {
                deferred(group.top(preview.held)).into_any_element()
            }
            Some(preview) => group.top(preview.offsets[unit_index]).into_any_element(),
            None => group.into_any_element(),
        }
    }

    /// Cached Environment rail preview uses the same row renderer as the live
    /// Session Navigator (ENV-RAIL-TREE-52); only activation differs.
    pub(crate) fn render_rail_preview_session_unit(
        &self,
        row: agentty_core::agent_runtime::NavigatorRow,
        unit_index: usize,
        preview_env_id: agentty_core::core::environment::EnvironmentId,
        select_target: Option<agentty_core::core::session::RemoteTarget>,
        preview_selected: Option<&agentty_core::agent_runtime::NavigatorRowId>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let metrics = session_sidebar_surface_metrics();
        let stack = session_unit_stack_surface(false, false, cx).child(self.session_navigator_row(
            row,
            unit_index,
            1,
            &self.session_list_state,
            None,
            false,
            cx,
            Some(select_target),
            preview_selected,
            Some(preview_env_id),
        ));
        v_flex()
            .id(format!("environment-rail-preview-unit-{unit_index}"))
            .w_full()
            .px(px(metrics.unit_outer_pad))
            .pb(px(metrics.unit_gap))
            .child(stack)
    }

    fn session_navigator_row(
        &self,
        row: agentty_core::agent_runtime::NavigatorRow,
        unit_index: usize,
        unit_count: usize,
        list_state: &gpui::ListState,
        merge_tab: Option<agentty_core::core::machine::TabId>,
        body_reorder: bool,
        cx: &mut Context<Self>,
        rail_preview_select: Option<Option<agentty_core::core::session::RemoteTarget>>,
        preview_selected: Option<&agentty_core::agent_runtime::NavigatorRowId>,
        preview_env_id: Option<agentty_core::core::environment::EnvironmentId>,
    ) -> gpui::AnyElement {
        let preview = rail_preview_select.is_some();
        let title = row.display_title(&crate::core::i18n::current(cx, "session.default_name"));
        let row_id = row.row_id.clone();
        let debug_row_id = row.row_id.clone();
        let alias_input = if preview {
            None
        } else {
            self.session_alias_edit
                .as_ref()
                .filter(|edit| edit.row_id == row.row_id)
                .map(|edit| edit.input.clone())
        };
        let edit_row_id = row.row_id.clone();
        let selected = if preview {
            preview_selected.is_some_and(|selected| selected == &row.row_id)
        } else {
            self.session_navigator
                .selected()
                .is_some_and(|selected| selected == &row.row_id)
        };
        let keyboard_cursor = if preview {
            false
        } else {
            self.session_keyboard_cursor.current() == Some(&row.row_id)
        };
        let active = row.lifecycle == agentty_core::agent_runtime::RowLifecycle::Live;
        let execution = row.execution.as_ref();
        let badge = agentty_core::agent_runtime::execution_badge(
            row.lifecycle,
            execution.map(|execution| &execution.state),
            execution.is_some_and(|execution| execution.focused),
            execution.is_some_and(|execution| execution.unread),
        );
        let status_dot = badge.map(|badge| match badge {
            agentty_core::agent_runtime::ExecutionBadge::Restoring
            | agentty_core::agent_runtime::ExecutionBadge::Waiting => cx.theme().warning,
            agentty_core::agent_runtime::ExecutionBadge::Running => cx.theme().info,
            agentty_core::agent_runtime::ExecutionBadge::FocusedLive => cx.theme().success,
            agentty_core::agent_runtime::ExecutionBadge::BackgroundLive => {
                cx.theme().muted_foreground
            }
            agentty_core::agent_runtime::ExecutionBadge::CompletedUnread => cx.theme().accent,
        });
        let agent_icon = row.agent.icon_path();
        let agent_accent = row.agent.accent_rgb();
        let agent_glyph = row.agent.glyph_rgb();
        let metrics = session_sidebar_surface_metrics();
        let icon_badge = crate::ui::agent_icon::agent_icon_badge(
            agent_icon,
            metrics.icon_size,
            metrics.icon_radius,
            agent_accent,
            agent_glyph,
            metrics.icon_glyph,
            cx,
        );
        let reserve_border = session_row_reserves_border(selected, keyboard_cursor);
        let border_color = if selected {
            cx.theme().border
        } else if keyboard_cursor {
            cx.theme().ring
        } else {
            cx.theme().transparent
        };
        let sf = &cx.global::<crate::ui::presets::Surfaces>().sidebar;
        let fill = session_row_surface_fill(selected, keyboard_cursor, sf);
        let hover_fill = session_row_hover_fill(sf);
        let text = if selected {
            gpui::rgb(sf.text_selected)
        } else {
            gpui::rgb(sf.text_resting)
        };
        let execution_pill_key = crate::ui::agent_attention::execution_attention_pill_key(badge);
        let execution_pill_color = match badge {
            Some(agentty_core::agent_runtime::ExecutionBadge::Waiting) => cx.theme().warning,
            Some(agentty_core::agent_runtime::ExecutionBadge::CompletedUnread) => cx.theme().accent,
            _ => cx.theme().transparent,
        };
        let rail_preview_select = rail_preview_select;
        let preview_select_target = rail_preview_select.clone();
        let preview_select_for_click = preview_select_target.clone();
        let item = h_flex()
            .id(gpui::SharedString::from(format!(
                "agent-session-row-{}",
                row.row_id.as_str()
            )))
            .debug_selector(move || format!("SESSION_ROW_{}", debug_row_id.as_str()))
            .group("agent-session-row")
            .w_full()
            .min_h(px(metrics.row_min_height))
            .items_center()
            .relative()
            .gap(px(metrics.icon_text_gap))
            .px(px(metrics.row_pad_x))
            .py(px(metrics.row_pad_y))
            .rounded_md()
            .when(reserve_border, |item| {
                item.border_1().border_color(border_color)
            })
            .cursor_pointer()
            .text_color(text)
            .bg(gpui::rgb(fill))
            .when(execution_pill_key.is_some(), |item| {
                item.border_l_2().border_color(execution_pill_color)
            })
            .when(!selected, |item| {
                item.hover(|item| item.bg(gpui::rgb(hover_fill)))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                if cx.has_active_drag() {
                    return;
                }
                cx.stop_propagation();
                if let Some(select_target) = preview_select_for_click.clone() {
                    if let Some(target) = select_target {
                        this.select_environment(Some(target), window, cx);
                    } else {
                        this.select_environment(None, window, cx);
                    }
                } else {
                    this.activate_navigator_row(row_id.clone(), window, cx);
                }
            }))
            .child({
                let mut icon = div()
                    .id(gpui::SharedString::from(format!(
                        "session-unit-icon-{unit_index}"
                    )))
                    .debug_selector(move || format!("SESSION_UNIT_{unit_index}_ICON"))
                    .flex_shrink_0()
                    .relative()
                    .child(icon_badge)
                    .when_some(status_dot, |icon_box, dot| {
                        icon_box.child(
                            div()
                                .absolute()
                                .right(px(-1.))
                                .bottom(px(-1.))
                                .size(px(7.))
                                .rounded_full()
                                .border_2()
                                .border_color(cx.theme().sidebar)
                                .bg(dot),
                        )
                    });
                if let Some(source_tab) = merge_tab {
                    let state = self.reorder.clone();
                    let list_state = list_state.clone();
                    icon = icon.on_drag(DragSessionIcon, move |_drag, grab, _window, cx| {
                        cx.stop_propagation();
                        let rects = (0..unit_count)
                            .map(|index| list_state.bounds_for_item(index))
                            .collect();
                        *state.borrow_mut() = Some(Reorder::new_tab_projected(
                            Surface::Navigator,
                            unit_index,
                            rects,
                            Axis::Vertical,
                            px(1.5),
                            grab,
                            source_tab,
                            TabDragIntent::Merge,
                            TabDragHitZone::Icon,
                        ));
                        cx.new(|_| DragSessionIcon)
                    });
                }
                icon
            })
            .child({
                let mut body = h_flex()
                    .id(gpui::SharedString::from(format!(
                        "session-unit-body-{unit_index}"
                    )))
                    .debug_selector(move || format!("SESSION_UNIT_{unit_index}_BODY"))
                    .flex_1()
                    .min_w_0()
                    .relative()
                    .items_center()
                    .gap(px(4.))
                    .overflow_hidden();
                if body_reorder {
                    if let Some(source_tab) = merge_tab {
                        let state = self.reorder.clone();
                        let list_state = list_state.clone();
                        body = body.on_drag(DragSessionBody, move |_drag, grab, _window, cx| {
                            cx.stop_propagation();
                            let rects = (0..unit_count)
                                .map(|index| list_state.bounds_for_item(index))
                                .collect();
                            *state.borrow_mut() = Some(Reorder::new_tab_projected(
                                Surface::Navigator,
                                unit_index,
                                rects,
                                Axis::Vertical,
                                px(1.5),
                                grab,
                                source_tab,
                                TabDragIntent::Reorder,
                                TabDragHitZone::Body,
                            ));
                            cx.new(|_| DragSessionBody)
                        });
                    }
                }
                body.child(match alias_input {
                    Some(input) => div()
                        .w_full()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(Input::new(&input).appearance(false).xsmall())
                        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                            if event.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.cancel_session_alias_edit(cx);
                            }
                        }))
                        .into_any_element(),
                    None => div()
                        .id(gpui::SharedString::from(format!(
                            "session-alias-label-{}",
                            edit_row_id.as_str()
                        )))
                        .w_full()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_sm()
                        .on_double_click(cx.listener(move |this, _, window, cx| {
                            this.begin_session_alias_edit(edit_row_id.clone(), window, cx)
                        }))
                        .child(title)
                        .into_any_element(),
                })
                .when_some(execution_pill_key, |row, key| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(execution_pill_color)
                            .bg(execution_pill_color.opacity(0.12))
                            .child(crate::core::i18n::current(cx, key)),
                    )
                })
                .child(session_row_text_fade(selected, cx))
            })
            .when(row.pinned, |item| {
                item.child(
                    div()
                        .id(gpui::SharedString::from(format!(
                            "session-pin-marker-{}",
                            row.row_id.as_str()
                        )))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(14.))
                        .child(
                            Icon::empty()
                                .path("icons/pin.svg")
                                .size(px(11.))
                                .text_color(cx.theme().muted_foreground),
                        ),
                )
            })
            .map(|item| match session_row_hover_affordance(active, badge) {
                SessionRowHoverAffordance::RestoringStatus => item.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::core::i18n::current(cx, "session.restoring")),
                ),
                SessionRowHoverAffordance::None => item,
            });
        if !session_hover_allowed(self.session_row_menu_open.get()) {
            return item.into_any_element();
        }
        let hover_row_id = row.row_id.clone();
        let hover_app = cx.entity().downgrade();
        let sidecar_lead = session_hover_sidecar_lead(self.sidebar_hover_panel_width.get());
        let hover_epoch = self.session_hover_epoch.get();
        let hover_preview_env = preview_env_id.clone();
        let hover_preview_select = rail_preview_select.clone();
        if preview {
            return Self::attach_session_row_hover(
                item,
                hover_row_id,
                hover_app,
                sidecar_lead,
                hover_epoch,
                hover_preview_env,
                hover_preview_select,
            );
        }
        Self::attach_session_row_hover(
            item.context_menu({
                let app = cx.entity().downgrade();
                let menu_row = row.clone();
                move |menu, _window, cx| {
                    let menu_entity = cx.entity();
                    if let Some(app) = app.upgrade() {
                        app.update(cx, |this, cx| {
                            this.begin_session_row_context_menu(&menu_entity, cx);
                        });
                    }
                    Self::session_row_context_menu(menu, menu_row.clone(), &app, cx)
                }
            }),
            hover_row_id,
            hover_app,
            sidecar_lead,
            hover_epoch,
            hover_preview_env,
            hover_preview_select,
        )
    }

    fn attach_session_row_hover<T: gpui::IntoElement + 'static>(
        trigger: T,
        hover_row_id: agentty_core::agent_runtime::NavigatorRowId,
        hover_app: gpui::WeakEntity<Self>,
        sidecar_lead: f32,
        hover_epoch: u64,
        hover_preview_env: Option<agentty_core::core::environment::EnvironmentId>,
        hover_preview_select: Option<Option<agentty_core::core::session::RemoteTarget>>,
    ) -> gpui::AnyElement {
        HoverCard::new(gpui::SharedString::from(format!(
            "session-hover-card-{}-{hover_epoch}",
            hover_row_id.as_str()
        )))
        .anchor(Anchor::TopLeft)
        .ml(px(sidecar_lead))
        .open_delay(SESSION_HOVER_CARD_OPEN_DELAY)
        .close_delay(SESSION_HOVER_CARD_CLOSE_DELAY)
        .trigger(trigger)
        .content({
            let hover_preview_select = hover_preview_select.clone();
            move |_, window, cx| {
                let row = hover_app.upgrade().and_then(|app| {
                    let app = app.read(cx);
                    if let Some(env_id) = hover_preview_env.as_ref() {
                        app.environment_navigator_cache
                            .detail_row(env_id, &hover_row_id)
                            .cloned()
                    } else {
                        app.session_navigator.detail_row(&hover_row_id).cloned()
                    }
                });
                match row {
                    Some(row) => Self::session_hover_detail_card(
                        row,
                        &hover_app,
                        hover_preview_select.clone(),
                        window,
                        cx,
                    ),
                    None => div()
                        .id("stale-session-hover-card")
                        .w(px(220.))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::core::i18n::current(cx, "session.hover_unavailable"))
                        .into_any_element(),
                }
            }
        })
        .into_any_element()
    }

    fn begin_session_row_context_menu(&mut self, menu: &Entity<PopupMenu>, cx: &mut Context<Self>) {
        self.session_row_menu_open.set(true);
        self.session_hover_epoch
            .set(self.session_hover_epoch.get().wrapping_add(1));
        self.session_row_menu_dismiss = Some(cx.subscribe(menu, {
            move |this, _menu, _: &DismissEvent, cx| {
                this.session_row_menu_open.set(false);
                this.session_row_menu_dismiss = None;
                cx.notify();
            }
        }));
        // Do not cx.notify() here: ContextMenu builds inside window.defer and
        // already calls window.refresh() after the menu view is attached.
        // An extra AgenttyApp notify on the same frame double-renders the
        // navigator and makes right-click feel lagged.
    }

    fn session_hover_detail_card(
        row: agentty_core::agent_runtime::NavigatorRow,
        app: &gpui::WeakEntity<Self>,
        preview_select: Option<Option<agentty_core::core::session::RemoteTarget>>,
        _window: &mut Window,
        cx: &mut gpui::App,
    ) -> gpui::AnyElement {
        let empty = "—";
        let title = row.display_title(&crate::core::i18n::current(cx, "session.default_name"));
        let badge = agentty_core::agent_runtime::execution_badge(
            row.lifecycle,
            row.execution.as_ref().map(|execution| &execution.state),
            row.execution
                .as_ref()
                .is_some_and(|execution| execution.focused),
            row.execution
                .as_ref()
                .is_some_and(|execution| execution.unread),
        );
        let status_key = match badge {
            Some(agentty_core::agent_runtime::ExecutionBadge::Restoring) => "session.restoring",
            Some(agentty_core::agent_runtime::ExecutionBadge::Waiting) => "activity.waiting",
            Some(agentty_core::agent_runtime::ExecutionBadge::Running) => "activity.working",
            Some(agentty_core::agent_runtime::ExecutionBadge::CompletedUnread) => "activity.done",
            Some(
                agentty_core::agent_runtime::ExecutionBadge::FocusedLive
                | agentty_core::agent_runtime::ExecutionBadge::BackgroundLive,
            ) => "session.details_live",
            None => "session.details_history",
        };
        let status = crate::core::i18n::current(cx, status_key).to_string();
        let waiting_message = session_hover_waiting_message(&row);
        let resume_command = session_hover_resume_line(&row);
        let transcript_path = session_hover_transcript_path(&row);
        let metadata_row = |label: &'static str, value: String, cx: &gpui::App| {
            v_flex()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::core::i18n::current(cx, label)),
                )
                .child(div().text_xs().child(value))
        };
        let row_id = row.row_id.clone();
        let activate_app = app.clone();
        let activate_preview = preview_select.clone();
        let details = row.clone();
        let details_app = app.clone();
        let preview_row = preview_select.is_some();
        let pinned = row.pinned;
        let mut card = v_flex()
            .id(gpui::SharedString::from(format!(
                "session-hover-detail-{}",
                row.row_id.as_str()
            )))
            .w(px(320.))
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .when(pinned, |header| {
                                header.child(
                                    div()
                                        .flex_shrink_0()
                                        .px_1p5()
                                        .py_0p5()
                                        .rounded_sm()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .bg(cx.theme().muted.opacity(0.18))
                                        .child(crate::core::i18n::current(cx, "menu.pin_session")),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(status),
                    ),
            );
        if let Some(message) = waiting_message {
            let warning = cx.theme().warning;
            card = card.child(
                div()
                    .px_2()
                    .py(px(6.))
                    .rounded_md()
                    .bg(warning.opacity(0.12))
                    .border_1()
                    .border_color(warning.opacity(0.35))
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(div().text_size(px(10.)).text_color(warning).child(
                                crate::core::i18n::current(cx, "session.hover_waiting_message"),
                            ))
                            .child(div().text_xs().child(message)),
                    ),
            );
        }
        card = card
            .child(metadata_row(
                "session.details_provider",
                row.agent.display_name().to_string(),
                cx,
            ))
            .child(metadata_row(
                "session.details_working_directory",
                row.cwd.clone().unwrap_or_else(|| empty.to_string()),
                cx,
            ))
            .child(metadata_row(
                "session.details_session_id",
                row.session_id.clone().unwrap_or_else(|| empty.to_string()),
                cx,
            ));
        if let Some(path) = transcript_path {
            card = card.child(metadata_row("session.details_transcript", path, cx));
        }
        if let Some(command) = resume_command {
            card = card.child(metadata_row("session.details_resume_command", command, cx));
        }
        card.child(metadata_row(
            "session.details_updated",
            crate::ui::home::format_session_updated_at(row.updated_at_unix_ms, empty),
            cx,
        ))
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new(gpui::SharedString::from(format!(
                        "session-hover-activate-{}",
                        row.row_id.as_str()
                    )))
                    .label(crate::core::i18n::current(
                        cx,
                        if preview_row {
                            "session.hover_switch_environment"
                        } else if row.lifecycle == agentty_core::agent_runtime::RowLifecycle::Live {
                            "session.details_live"
                        } else {
                            "session.resume"
                        },
                    ))
                    .small()
                    .on_click({
                        let activate_preview = activate_preview.clone();
                        move |_, window, cx| {
                            let select_target = activate_preview.clone();
                            let _ = activate_app.update(cx, |this, cx| {
                                if let Some(select_target) = select_target {
                                    if let Some(target) = select_target {
                                        this.select_environment(Some(target), window, cx);
                                    } else {
                                        this.select_environment(None, window, cx);
                                    }
                                } else {
                                    this.activate_navigator_row(row_id.clone(), window, cx);
                                }
                            });
                        }
                    }),
                )
                .child(
                    Button::new(gpui::SharedString::from(format!(
                        "session-hover-details-{}",
                        row.row_id.as_str()
                    )))
                    .label(crate::core::i18n::current(cx, "session.details_action"))
                    .ghost()
                    .small()
                    .on_click(move |_, window, cx| {
                        Self::show_session_details(details.clone(), &details_app, window, cx);
                    }),
                ),
        )
        .into_any_element()
    }

    fn show_session_details(
        details: agentty_core::agent_runtime::NavigatorRow,
        app: &gpui::WeakEntity<Self>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        let _ = app.update(cx, |_this, cx| {
            let empty = "—";
            let updated =
                crate::ui::home::format_session_updated_at(details.updated_at_unix_ms, empty);
            let command = details
                .resume_invocation()
                .as_ref()
                .map(agentty_core::agent_runtime::shell_command)
                .unwrap_or_else(|| empty.to_string());
            let body = crate::core::i18n::current_format(
                cx,
                "session.details_body",
                &[
                    ("agent", details.agent.display_name()),
                    ("session_id", details.session_id.as_deref().unwrap_or(empty)),
                    ("cwd", details.cwd.as_deref().unwrap_or(empty)),
                    ("updated", &updated),
                    ("source", details.source_path.as_deref().unwrap_or(empty)),
                    ("command", &command),
                ],
            );
            let _ = window.prompt(
                PromptLevel::Info,
                crate::core::i18n::current(cx, "session.details_title"),
                Some(&body),
                &[crate::core::i18n::current(cx, "common.ok")],
                cx,
            );
        });
    }

    fn session_row_context_menu(
        menu: PopupMenu,
        row: agentty_core::agent_runtime::NavigatorRow,
        app: &gpui::WeakEntity<Self>,
        cx: &gpui::App,
    ) -> PopupMenu {
        let id = row.session_id.clone();
        let cwd = row.cwd.clone();
        let source = row.source_path.clone();
        let command = row
            .resume_invocation()
            .as_ref()
            .map(agentty_core::agent_runtime::shell_command);
        let details = row.clone();
        let details_app = app.clone();
        let row_id = row.row_id.clone();
        let delete_app = app.clone();
        let pin_row_id = row.row_id.clone();
        let pin_app = app.clone();
        let pin_label = if row.pinned {
            crate::core::i18n::current(cx, "menu.unpin_session")
        } else {
            crate::core::i18n::current(cx, "menu.pin_session")
        };
        let is_live = row.lifecycle == agentty_core::agent_runtime::RowLifecycle::Live
            || row.lifecycle == agentty_core::agent_runtime::RowLifecycle::Restoring;
        let danger = cx.theme().danger;
        let danger_hover = session_delete_hover_fill(danger);
        let close_label = crate::core::i18n::current(cx, "menu.close_session");
        let delete_label = crate::core::i18n::current(cx, "menu.delete_session");
        let mut menu = menu
            .min_w(px(210.))
            .item(
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.copy_session_id"))
                    .disabled(id.is_none())
                    .on_click(move |_, _window, cx| {
                        if let Some(id) = id.as_ref() {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(id.clone()));
                        }
                    }),
            )
            .item(
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.copy_resume_command"))
                    .disabled(command.is_none())
                    .on_click(move |_, _window, cx| {
                        if let Some(command) = command.as_ref() {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(command.clone()));
                        }
                    }),
            )
            .item(
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.copy_cwd"))
                    .disabled(cwd.is_none())
                    .on_click(move |_, _window, cx| {
                        if let Some(cwd) = cwd.as_ref() {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(cwd.clone()));
                        }
                    }),
            )
            .item(
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.copy_path"))
                    .disabled(source.is_none())
                    .on_click(move |_, _window, cx| {
                        if let Some(source) = source.as_ref() {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(source.clone()));
                        }
                    }),
            )
            .separator()
            .item(
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.session_details"))
                    .on_click(move |_, window, cx| {
                        Self::show_session_details(details.clone(), &details_app, window, cx);
                    }),
            )
            .separator()
            .item({
                let alias_row_id = row.row_id.clone();
                let alias_app = app.clone();
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.set_session_alias"))
                    .on_click(move |_, window, cx| {
                        let _ = alias_app.update(cx, |this, cx| {
                            this.begin_session_alias_edit(alias_row_id.clone(), window, cx);
                        });
                    })
            })
            .item(
                PopupMenuItem::new(pin_label).on_click(move |_, window, cx| {
                    let _ = pin_app.update(cx, |this, cx| {
                        this.toggle_session_pin(pin_row_id.clone(), window, cx);
                    });
                }),
            )
            .separator()
            .item(
                PopupMenuItem::element(move |_window, _cx| {
                    let label = if is_live { close_label } else { delete_label };
                    div()
                        .w_full()
                        .h_full()
                        .rounded_sm()
                        .text_color(danger)
                        .hover(move |item| item.bg(danger_hover))
                        .child(gpui::SharedString::from(label))
                })
                .on_click(move |_, window, cx| {
                    let _ = delete_app.update(cx, |this, cx| {
                        if is_live {
                            this.close_live_session_row(row_id.clone(), window, cx);
                        } else {
                            this.delete_session_row(row_id.clone(), window, cx);
                        }
                    });
                }),
            );
        if is_live {
            let close_delete_app = app.clone();
            let close_delete_id = row.row_id.clone();
            let close_delete_label =
                crate::core::i18n::current(cx, "menu.close_and_delete_session");
            menu = menu.item(
                PopupMenuItem::element(move |_window, _cx| {
                    div()
                        .w_full()
                        .h_full()
                        .rounded_sm()
                        .text_color(danger)
                        .hover(move |item| item.bg(danger_hover))
                        .child(gpui::SharedString::from(close_delete_label))
                })
                .on_click(move |_, window, cx| {
                    let _ = close_delete_app.update(cx, |this, cx| {
                        this.close_and_delete_live_session_row(close_delete_id.clone(), window, cx);
                    });
                }),
            );
        }
        menu
    }

    #[cfg(test)]
    fn navigator_icon_path(agent: crate::core::cli_agent::CLIAgent) -> &'static str {
        agent.icon_path()
    }

    fn visual_tab_order(&self, _cx: &gpui::App) -> Vec<usize> {
        (0..self.tabs.len()).collect()
    }

    pub(crate) fn activate_visual(
        &mut self,
        n: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(&i) = self.visual_tab_order(cx).get(n) {
            self.activate(i, window, cx);
        }
    }
}

/// Truncate a path to the most informative suffix segments, keeping the
/// root indicator when present.  `/home/user/very/long/path/to/project`
/// becomes `…/path/to/project`.
fn smart_truncate_path(raw: &str, max_segments: usize) -> String {
    let path = raw.trim_end_matches('/');
    if path.is_empty() {
        return raw.to_owned();
    }
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() <= max_segments {
        return raw.to_owned();
    }
    let root = if raw.starts_with('/') { "/" } else { "" };
    let tail = &segments[segments.len() - max_segments..];
    format!("{root}…/{}", tail.join("/"))
}

fn smart_truncate_line(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let prefix: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{prefix}…")
}

pub(crate) fn session_hover_waiting_message(
    row: &agentty_core::agent_runtime::NavigatorRow,
) -> Option<String> {
    row.execution
        .as_ref()
        .and_then(|execution| {
            agentty_core::agent_runtime::execution_message(Some(&execution.state))
        })
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_owned)
}

pub(crate) fn session_hover_resume_line(
    row: &agentty_core::agent_runtime::NavigatorRow,
) -> Option<String> {
    row.resume_invocation()
        .as_ref()
        .map(agentty_core::agent_runtime::shell_command)
        .map(|command| smart_truncate_line(command.trim(), 96))
        .filter(|command| !command.is_empty())
}

pub(crate) fn session_hover_transcript_path(
    row: &agentty_core::agent_runtime::NavigatorRow,
) -> Option<String> {
    row.source_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| smart_truncate_path(path, 3))
}

fn session_delete_hover_fill(danger: gpui::Hsla) -> gpui::Hsla {
    danger.opacity(0.14)
}

fn navigator_subtitle(row: &agentty_core::agent_runtime::NavigatorRow) -> Option<String> {
    row.cwd
        .as_deref()
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .map(|cwd| smart_truncate_path(cwd, 3))
}

pub(crate) fn navigator_search_text(row: &agentty_core::agent_runtime::NavigatorRow) -> String {
    [
        row.alias.as_deref(),
        row.title.as_deref(),
        row.cwd.as_deref(),
        Some(row.agent.slug()),
        Some(row.agent.display_name()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
    .to_lowercase()
}

/// Live/restoring primary destructive action closes the carrier; historical
/// rows permanently delete. Live menus also expose Close and Delete.
pub(crate) fn live_session_destructive_action(is_live: bool) -> &'static str {
    if is_live { "close" } else { "delete" }
}

pub(crate) fn live_session_menu_actions(is_live: bool) -> &'static [&'static str] {
    if is_live {
        &["close", "close_and_delete"]
    } else {
        &["delete"]
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgenttyApp, RAIL_SESSION_INDENT, SESSION_HOVER_CARD_CLOSE_DELAY, SESSION_HOVER_CARD_GAP,
        SESSION_HOVER_CARD_OPEN_DELAY, SessionEmptyStateKind, SessionRowHoverAffordance,
        SessionUnitVisualKind, navigator_search_text, navigator_subtitle,
        session_delete_hover_fill, session_empty_state_kind, session_hover_allowed,
        session_hover_resume_line, session_hover_sidecar_lead, session_hover_transcript_path,
        session_hover_waiting_message, session_row_content_inset, session_row_hover_affordance,
        session_row_hover_fill, session_row_reserves_border, session_row_surface_fill,
        session_search_surface, session_sidebar_surface_metrics, session_unit_stack_surface,
        session_unit_visual_kind, smart_truncate_path,
    };
    use crate::core::cli_agent::{AgentSessionState, AgentStatus, CLIAgent};
    use agentty_core::agent_runtime::{
        AgentSessionKey, AgentSessionRecord, LiveExecutionState, NavigatorRow, SessionNavigator,
    };
    use gpui::{InteractiveElement as _, ParentElement as _, Styled as _};
    use gpui_component::IconNamed as _;

    fn row() -> NavigatorRow {
        let mut navigator = SessionNavigator::default();
        navigator.refresh(
            &[AgentSessionRecord {
                key: AgentSessionKey {
                    provider: "codex".into(),
                    session_id: "session-1".into(),
                },
                agent: CLIAgent::Codex,
                title: Some("Fix subtitle hierarchy".into()),
                title_candidates: Default::default(),
                cwd: Some("/repo/agentty".into()),
                updated_at_unix_ms: None,
                launch_argv: Vec::new(),
                source_path: None,
                created_at_unix_ms: None,
            }],
            &[],
        );
        navigator.rows()[0].clone()
    }

    #[test]
    fn navigator_subtitle_uses_session_context_without_repeating_provider() {
        let row = row();
        assert_eq!(navigator_subtitle(&row).as_deref(), Some("/repo/agentty"));
        assert_ne!(navigator_subtitle(&row).as_deref(), Some(row.agent.slug()));
        assert_ne!(
            navigator_subtitle(&row).as_deref(),
            Some(row.agent.display_name())
        );
    }

    #[test]
    fn provider_identity_remains_searchable_when_hidden_from_subtitle() {
        let row = row();
        let search = navigator_search_text(&row);
        assert!(search.contains("codex"));
        assert!(search.contains(&row.agent.display_name().to_lowercase()));
    }

    #[test]
    fn smart_path_truncation_keeps_informative_suffix() {
        assert_eq!(
            smart_truncate_path("/home/user/work/company/agentty", 3),
            "/…/work/company/agentty"
        );
        assert_eq!(smart_truncate_path("/repo/agentty", 3), "/repo/agentty");
    }

    #[test]
    fn session_row_visual_contract_is_distinct() {
        let danger = gpui::Hsla {
            h: 0.1,
            s: 0.8,
            l: 0.5,
            a: 1.0,
        };
        let hover = session_delete_hover_fill(danger);
        assert_eq!(hover.h, danger.h);
        assert_eq!(hover.s, danger.s);
        assert_eq!(hover.l, danger.l);
        assert!((hover.a - 0.14).abs() < f32::EPSILON);

        // Idle / hover / selected / cursor fills must stay distinct on the
        // Surfaces.sidebar ladder (never collapse onto theme muted).
        let sidebar = crate::ui::presets::Surface {
            base: 0x1e_1e_2e,
            hover: 0x2a_2a_3c,
            selected: 0x3a_3a_52,
            pressed: 0x33_33_48,
            cursor: 0x30_30_44,
            text_resting: 0xc0_c0_d0,
            text_selected: 0xf0_f0_ff,
        };
        let idle = session_row_surface_fill(false, false, &sidebar);
        let selected = session_row_surface_fill(true, false, &sidebar);
        let cursor = session_row_surface_fill(false, true, &sidebar);
        let row_hover = session_row_hover_fill(&sidebar);
        assert_ne!(idle, selected);
        assert_ne!(idle, row_hover);
        assert_ne!(idle, cursor);
        assert_ne!(selected, row_hover);
        assert_ne!(selected, cursor);
        assert_ne!(row_hover, cursor);
        assert_eq!(
            idle, sidebar.base,
            "idle session rows rest flat on sidebar.base"
        );
        assert_eq!(selected, sidebar.selected);
        assert_eq!(cursor, sidebar.cursor);
        assert_eq!(row_hover, sidebar.hover);
    }

    #[test]
    fn session_row_paint_uses_surfaces_sidebar_not_theme_muted() {
        let source = include_str!("tab_sidebar.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            prod.contains("session_row_surface_fill")
                && prod.contains("session_row_hover_fill")
                && prod.contains("Surfaces>().sidebar"),
            "navigator rows must paint from Surfaces.sidebar"
        );
        assert!(
            !prod.contains("item.bg(cx.theme().muted)")
                && !prod.contains("item.bg(cx.theme().sidebar_accent)"),
            "navigator rows must not use theme muted/sidebar_accent as fill authority"
        );
    }

    #[test]
    fn session_sidebar_rows_are_single_line_title_only() {
        let source = include_str!("tab_sidebar.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            production.contains("session-unit-body-") && production.contains("h_flex()"),
            "session row body must be a single horizontal line"
        );
        assert!(
            !production.contains("waiting_message.or(subtitle)"),
            "sidebar rows must not render a second subtitle line"
        );
    }

    #[test]
    fn rail_preview_rows_mount_session_hover_cards() {
        let source = include_str!("tab_sidebar.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            production.contains("environment_navigator_cache")
                && production.contains(".detail_row(env_id")
                && production.contains("attach_session_row_hover"),
            "preview rows must resolve hover detail from the cached Environment navigator"
        );
        assert!(
            !production.contains("if preview || !session_hover_allowed"),
            "preview rows must not disable hover cards"
        );
    }

    #[test]
    fn session_hover_detail_includes_rich_metadata() {
        let mut row = row();
        row.source_path = Some("/home/user/.codex/sessions/long/nested/session.jsonl".into());
        assert_eq!(
            session_hover_transcript_path(&row).as_deref(),
            Some("/…/long/nested/session.jsonl")
        );
        assert!(session_hover_waiting_message(&row).is_none());
        row.execution = Some(LiveExecutionState {
            state: AgentSessionState {
                status: AgentStatus::Waiting,
                message: Some("Approve file edit?".into()),
                ..Default::default()
            },
            focused: false,
            unread: false,
        });
        assert_eq!(
            session_hover_waiting_message(&row).as_deref(),
            Some("Approve file edit?")
        );
        assert!(
            session_hover_resume_line(&row).is_some_and(|command| command.contains("resume")),
            "historical rows expose truncated resume command"
        );

        let source = include_str!("tab_sidebar.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(production.contains("session_hover_waiting_message"));
        assert!(production.contains("session.details_transcript"));
        assert!(production.contains("session.details_resume_command"));
    }

    #[test]
    fn session_sidebar_surface_metrics_are_explicit() {
        let metrics = session_sidebar_surface_metrics();
        let inset = session_row_content_inset(&metrics);
        assert!(
            (6.0..=12.0).contains(&inset),
            "nested rail content inset must stay in the 6–12px band, got {inset}"
        );
        assert_eq!(
            metrics.outer_gutter,
            crate::ui::app::panel_content_gutter(),
            "search chrome still uses the shared panel content gutter"
        );
        assert_eq!(metrics.unit_outer_pad, 4.0);
        assert_eq!(metrics.search_height, 30.0);
        assert_eq!(metrics.row_min_height, 28.0);
        assert_eq!(metrics.unit_gap, 1.0);
        assert_eq!(metrics.row_pad_x, 2.0);
        assert_eq!(metrics.row_pad_y, 1.0);
        assert_eq!(metrics.icon_size, 16.0);
        assert_eq!(metrics.icon_glyph, 9.0);
        assert_eq!(metrics.icon_radius, 4.0);
        assert_eq!(metrics.icon_text_gap, 6.0);
        assert_eq!(metrics.title_subtitle_gap, 1.0);
        assert_eq!(metrics.subtitle_size, 10.0);
        assert_eq!(metrics.text_fade_width, 14.0);
        assert_eq!(inset, 6.0);
    }

    #[test]
    fn session_row_selection_keeps_stable_horizontal_inset() {
        assert!(session_row_reserves_border(false, false));
        assert!(session_row_reserves_border(true, false));
        assert!(session_row_reserves_border(false, true));
        assert!(session_row_reserves_border(true, true));
    }

    #[test]
    fn session_resume_affordance_overlays_without_flex_slot() {
        let source = include_str!("tab_sidebar.rs");
        assert!(
            source.contains(".absolute()")
                && source.contains("session-resume-affordance-")
                && source.contains(".right(px(pad))"),
            "resume affordance must overlay the trailing edge instead of reserving flex width"
        );
        assert!(
            !source.contains(".flex_shrink_0()\n                            .size(px(20.))"),
            "hidden resume affordance must not keep a permanent flex column"
        );
    }

    #[test]
    fn session_row_text_overflow_uses_trailing_fade_not_ellipsis() {
        let source = include_str!("tab_sidebar.rs");
        assert!(
            source.contains("fn session_row_text_fade")
                && source.contains("SESSION_ROW_TEXT_FADE")
                && source.contains("session_row_text_fade(selected, cx)"),
            "session rows must render a trailing fade helper for overflow text"
        );
        assert!(
            source.contains("metrics.icon_radius") && source.contains("agent_icon_badge("),
            "provider icons must use the shared rounded badge helper with metrics.icon_radius"
        );
        let alias_block = source
            .split("session-alias-label-")
            .nth(1)
            .expect("alias label block");
        let alias_style = alias_block
            .split(".into_any_element()")
            .next()
            .unwrap_or("");
        assert!(
            !alias_style.contains(".truncate()") && !alias_style.contains(".text_ellipsis()"),
            "title overflow must hard-clip + fade, not ellipsis truncate"
        );
    }

    #[test]
    fn split_session_units_use_one_visual_container() {
        assert_eq!(session_unit_visual_kind(0), SessionUnitVisualKind::Single);
        assert_eq!(session_unit_visual_kind(1), SessionUnitVisualKind::Single);
        assert_eq!(
            session_unit_visual_kind(2),
            SessionUnitVisualKind::SplitGroup
        );
        assert_eq!(
            session_unit_visual_kind(4),
            SessionUnitVisualKind::SplitGroup
        );
    }

    struct SessionSurfaceProbe;

    impl gpui::Render for SessionSurfaceProbe {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            let metrics = session_sidebar_surface_metrics();
            gpui::div()
                .w(gpui::px(220.))
                .h(gpui::px(220.))
                .child(session_search_surface(cx).child(gpui::div().size_full()))
                .child(
                    gpui::div().px(gpui::px(metrics.outer_gutter)).child(
                        session_unit_stack_surface(true, false, cx)
                            .child(gpui::div().min_h(gpui::px(metrics.row_min_height)))
                            .child(gpui::div().min_h(gpui::px(metrics.row_min_height))),
                    ),
                )
        }
    }

    #[gpui::test]
    fn rendered_sidebar_uses_search_surface_and_split_group_geometry(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{VisualTestContext, px, size};
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let surfaces = crate::ui::presets::by_id(cx, crate::ui::presets::DEFAULT_ID).surfaces();
            cx.set_global(surfaces);
        });
        let window = cx.open_window(size(px(220.), px(220.)), |_, _| SessionSurfaceProbe);
        cx.run_until_parked();
        let mut visual = VisualTestContext::from_window(window.into(), cx);

        let metrics = session_sidebar_surface_metrics();
        let search = visual
            .debug_bounds("SESSION_SEARCH_SURFACE")
            .expect("the real search surface helper must render");
        let split = visual
            .debug_bounds("SESSION_SPLIT_GROUP")
            .expect("the real split group surface helper must render");
        assert_eq!(search.size.height.as_f32(), metrics.search_height);
        assert_eq!(search.origin.x.as_f32(), metrics.outer_gutter);
        assert!(
            split.size.height.as_f32() >= metrics.row_min_height * 2.0,
            "the shared container must enclose both sibling rows"
        );
    }

    #[test]
    fn navigator_rows_use_their_agent_icon() {
        for agent in CLIAgent::ALL {
            let path = AgenttyApp::navigator_icon_path(agent);
            assert!(!path.is_empty(), "{}", agent.display_name());
            assert_ne!(
                path,
                "icons/terminal.svg",
                "{} must not collapse to a terminal glyph",
                agent.display_name()
            );
        }
    }

    #[test]
    fn session_hover_sidecar_lead_clears_the_sidebar_split() {
        let panel = 240.;
        let metrics = session_sidebar_surface_metrics();
        let left_inset = RAIL_SESSION_INDENT + metrics.unit_outer_pad + metrics.row_pad_x;
        let right_inset = metrics.unit_outer_pad + metrics.row_pad_x;
        let lead = session_hover_sidecar_lead(panel);
        assert_eq!(
            lead,
            panel - left_inset - right_inset + SESSION_HOVER_CARD_GAP
        );
        assert!(
            lead > panel - left_inset - right_inset,
            "sidecar lead must clear the row width before the gap"
        );
    }

    #[test]
    fn session_hover_sidecar_uses_rendered_panel_width_not_config() {
        let config_width = 400.;
        let panel_width = 240.;
        let config_lead = session_hover_sidecar_lead(config_width);
        let panel_lead = session_hover_sidecar_lead(panel_width);
        assert!(
            config_lead > panel_lead + 100.,
            "unclamped config width must not drive sidecar placement when panel is narrower"
        );
    }

    #[test]
    fn session_hover_yields_to_open_context_menu() {
        assert!(session_hover_allowed(false));
        assert!(!session_hover_allowed(true));
    }

    #[test]
    fn session_context_menu_exposes_set_alias() {
        let source = include_str!("tab_sidebar.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            prod.contains("menu.set_session_alias") && prod.contains("begin_session_alias_edit"),
            "context menu must expose Set Alias through begin_session_alias_edit"
        );
    }

    #[test]
    fn pinned_rows_render_explicit_pin_marker() {
        let source = include_str!("tab_sidebar.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            prod.contains("row.pinned")
                && prod.contains("session-pin-marker-")
                && prod.contains("icons/pin.svg"),
            "pinned rows must render an explicit pin glyph"
        );
        assert!(
            std::path::Path::new("assets/icons/pin.svg").exists(),
            "pin glyph asset must be bundled"
        );
    }

    #[test]
    fn session_hover_delays_stay_in_the_snappy_band() {
        assert!(
            SESSION_HOVER_CARD_OPEN_DELAY <= std::time::Duration::from_millis(200),
            "open delay must stay ≤200ms, got {:?}",
            SESSION_HOVER_CARD_OPEN_DELAY
        );
        assert!(
            SESSION_HOVER_CARD_CLOSE_DELAY <= std::time::Duration::from_millis(150),
            "close delay must stay ≤150ms, got {:?}",
            SESSION_HOVER_CARD_CLOSE_DELAY
        );
        let source = include_str!("tab_sidebar.rs");
        let begin = source
            .split("fn begin_session_row_context_menu")
            .nth(1)
            .and_then(|rest| rest.split("fn session_hover_detail_card").next())
            .unwrap_or("");
        // Strip the dismiss subscription body — notify there is required.
        let open_path = begin.split("cx.subscribe").next().unwrap_or(begin);
        assert!(
            !open_path.contains("cx.notify()"),
            "menu-open path must not cx.notify(); window.refresh from ContextMenu owns that frame"
        );
    }

    #[test]
    fn session_updated_display_uses_local_datetime_formatter() {
        let source = include_str!("tab_sidebar.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            prod.contains("format_session_updated_at"),
            "hover/details Updated must use format_session_updated_at"
        );
        assert!(
            !prod.contains("millis.to_string()"),
            "Updated must never render raw unix millis via millis.to_string()"
        );
        let millis = 1_784_303_401_029u64;
        let formatted = crate::ui::home::format_session_updated_at(Some(millis), "—");
        assert_ne!(formatted, millis.to_string());
        assert_ne!(formatted, "—");
    }

    struct HoverCardProbe;

    impl gpui::Render for HoverCardProbe {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            let lead = session_hover_sidecar_lead(240.);
            gpui::div().size_full().child(
                gpui_component::hover_card::HoverCard::new("session-hover-probe")
                    .anchor(gpui::Anchor::TopLeft)
                    .ml(gpui::px(lead))
                    .open_delay(SESSION_HOVER_CARD_OPEN_DELAY)
                    .close_delay(SESSION_HOVER_CARD_CLOSE_DELAY)
                    .trigger(
                        gpui::div()
                            .debug_selector(|| "HOVER_TRIGGER".into())
                            .w(gpui::px(
                                240. - 2. * session_sidebar_surface_metrics().outer_gutter,
                            ))
                            .h(gpui::px(40.)),
                    )
                    .child(
                        gpui::div()
                            .debug_selector(|| "HOVER_CONTENT".into())
                            .w(gpui::px(180.))
                            .h(gpui::px(100.)),
                    ),
            )
        }
    }

    #[gpui::test]
    fn pointer_safe_region_keeps_detail_open(cx: &mut gpui::TestAppContext) {
        use gpui::{Modifiers, VisualTestContext, point, px, size};
        cx.update(gpui_component::init);
        let window = cx.open_window(size(px(800.), px(400.)), |_, _| HoverCardProbe);
        cx.run_until_parked();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let trigger = cx
            .debug_bounds("HOVER_TRIGGER")
            .expect("hover trigger must render");
        cx.simulate_mouse_move(
            point(trigger.origin.x + px(10.), trigger.origin.y + px(10.)),
            None,
            Modifiers::none(),
        );
        cx.executor().advance_clock(SESSION_HOVER_CARD_OPEN_DELAY);
        cx.run_until_parked();
        let content = cx
            .debug_bounds("HOVER_CONTENT")
            .expect("hover content must open after delay");
        assert!(
            content.origin.x.as_f32()
                >= trigger.origin.x.as_f32() + trigger.size.width.as_f32() - 1.,
            "hover card must open as a trailing sidecar past the row, not over it (content_x={}, trigger_right={})",
            content.origin.x.as_f32(),
            trigger.origin.x.as_f32() + trigger.size.width.as_f32()
        );
        cx.simulate_mouse_move(
            point(content.origin.x + px(10.), content.origin.y + px(10.)),
            None,
            Modifiers::none(),
        );
        cx.executor()
            .advance_clock(SESSION_HOVER_CARD_CLOSE_DELAY * 2);
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("HOVER_CONTENT").is_some(),
            "content hover must cancel the trigger's pending close"
        );
    }
    #[test]
    fn historical_row_click_is_the_only_resume_affordance() {
        use agentty_core::agent_runtime::ExecutionBadge as Badge;
        assert_eq!(
            session_row_hover_affordance(false, None),
            SessionRowHoverAffordance::None
        );
        assert_eq!(
            session_row_hover_affordance(false, Some(Badge::CompletedUnread)),
            SessionRowHoverAffordance::None
        );
        assert_eq!(
            session_row_hover_affordance(true, None),
            SessionRowHoverAffordance::None
        );
        assert_eq!(
            session_row_hover_affordance(false, Some(Badge::Restoring)),
            SessionRowHoverAffordance::RestoringStatus
        );
        assert_eq!(
            session_row_hover_affordance(true, Some(Badge::Restoring)),
            SessionRowHoverAffordance::RestoringStatus
        );
        let source = include_str!("tab_sidebar.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(!production.contains("IconName::Play"));
        assert!(production.contains("this.activate_navigator_row(row_id.clone(), window, cx)"));
    }

    #[test]
    fn historical_row_activation_is_click_only() {
        let source = include_str!("tab_sidebar.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);

        let mut cursor = 0usize;
        while let Some(offset) = production[cursor..].find("on_mouse_down(") {
            let start = cursor + offset;
            let open_offset = production[start..]
                .find('(')
                .unwrap_or_else(|| panic!("expected opening paren for on_mouse_down at {start}"));
            let mut depth = 0i32;
            let mut end = None;
            for (idx, ch) in production[start + open_offset..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(start + open_offset + idx + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let end = end.unwrap_or_else(|| {
                panic!("could not parse on_mouse_down call ending for production slice at {start}")
            });
            let chunk = &production[start..end];
            assert!(
                !chunk.contains("activate_navigator_row("),
                "navigator activation must remain click-driven for session rows; chunk: {chunk}",
            );
            cursor = end;
        }

        assert!(
            production.contains("this.activate_navigator_row(row_id.clone(), window, cx)"),
            "activate_navigator_row should remain present and be reached from click paths"
        );
        assert!(
            production.contains(".has_active_drag()"),
            "navigator click activation should ignore click paths when a drag is active"
        );
        assert!(
            production.contains(".on_click(cx.listener(move |this, _, window, cx| {"),
            "session row activation should remain explicit in on_click handler"
        );
    }

    #[gpui::test]
    fn historical_row_pointer_drag_cancels_resume_and_click_release_selects(
        cx: &mut gpui::TestAppContext,
    ) {
        let (app, mut visual) = crate::ui::app::test_window::harness(cx);
        visual.update(|_, cx| {
            let mut config = cx.global::<crate::core::config::Config>().clone();
            config.tab_bar_position = crate::core::config::TabBarPosition::Left;
            cx.set_global(config);
        });
        let row_id = app.update_in(&mut visual, |app, _window, cx| {
            let target = crate::core::session::RemoteTarget::Alias {
                alias: "drag-only-remote".into(),
            };
            let remote_machine_workspace = crate::core::session::WorkspaceId::new();
            let remote = crate::core::session::WindowView::on_remote(
                crate::core::session::RemoteRef::new(target, remote_machine_workspace),
            );
            let window_workspace = remote.id;
            crate::core::session::WorkspaceStore::install_for_test(
                cx,
                crate::core::session::WindowViews {
                    views: vec![remote],
                    active: Some(window_workspace),
                },
            );
            app.workspace = window_workspace;
            app.tabs = vec![crate::ui::app::Tab::new(crate::ui::pane::Pane::Empty)];
            app.active = 0;
            app.sidebar_collapsed = false;
            app.session_scan_started = true;
            app.session_scan_error = None;
            app.session_history = vec![AgentSessionRecord {
                key: AgentSessionKey {
                    provider: "codex".into(),
                    session_id: "inactive-history-row".into(),
                },
                agent: CLIAgent::Codex,
                title: Some("Inactive history row".into()),
                title_candidates: Default::default(),
                cwd: None,
                updated_at_unix_ms: None,
                launch_argv: Vec::new(),
                source_path: None,
                created_at_unix_ms: None,
            }];
            app.rebuild_session_navigator(cx);
            let row_id = app.session_navigator.rows()[0].row_id.clone();
            cx.notify();
            row_id
        });
        visual.run_until_parked();

        let selector: &'static str =
            Box::leak(format!("SESSION_ROW_{}", row_id.as_str()).into_boxed_str());
        let row = visual
            .debug_bounds(selector)
            .expect("historical production navigator row must render");
        let press = gpui::point(
            row.origin.x + row.size.width / 2.,
            row.origin.y + row.size.height / 2.,
        );
        let dragged = gpui::point(press.x, press.y + gpui::px(16.));

        visual.simulate_mouse_move(press, None, gpui::Modifiers::none());
        visual.simulate_mouse_down(press, gpui::MouseButton::Left, gpui::Modifiers::none());
        assert!(
            app.update_in(&mut visual, |app, _, _| app
                .session_navigator
                .selected()
                .is_none()),
            "pointer-down alone must not select or resume a historical row"
        );
        visual.simulate_mouse_move(
            dragged,
            Some(gpui::MouseButton::Left),
            gpui::Modifiers::none(),
        );
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| cx.has_active_drag()),
            "movement beyond GPUI's drag threshold must enter navigator drag"
        );
        assert!(
            app.update_in(&mut visual, |app, _, _| app
                .session_navigator
                .selected()
                .is_none()),
            "starting a row drag must not select or resume it"
        );
        visual.simulate_mouse_up(dragged, gpui::MouseButton::Left, gpui::Modifiers::none());
        visual.run_until_parked();
        assert!(
            app.update_in(&mut visual, |app, _, _| app
                .session_navigator
                .selected()
                .is_none()),
            "release after navigator drag must not replay resume"
        );

        let row = visual
            .debug_bounds(selector)
            .expect("historical production navigator row must remain rendered");
        let click = gpui::point(
            row.origin.x + row.size.width / 2.,
            row.origin.y + row.size.height / 2.,
        );
        visual.simulate_mouse_move(click, None, gpui::Modifiers::none());
        visual.simulate_mouse_down(click, gpui::MouseButton::Left, gpui::Modifiers::none());
        assert!(
            app.update_in(&mut visual, |app, _, _| app
                .session_navigator
                .selected()
                .is_none()),
            "historical row activation must wait for click release"
        );
        visual.simulate_mouse_up(click, gpui::MouseButton::Left, gpui::Modifiers::none());
        visual.run_until_parked();
        let (selected, tab_count) = app.update_in(&mut visual, |app, _, _| {
            (app.session_navigator.selected().cloned(), app.tabs.len())
        });
        assert_eq!(
            selected.as_ref(),
            Some(&row_id),
            "stationary click-release must route through canonical row activation"
        );
        assert_eq!(
            tab_count, 1,
            "the disconnected remote fixture must not create a local fallback pane"
        );
    }

    #[test]
    fn session_navigator_refresh_uses_standard_refresh_asset() {
        let source = include_str!("tab_sidebar.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(production.contains("Button::new(\"session-refresh\")"));
        assert!(production.contains("icons/refresh.svg"));
        assert!(!production.contains("IconName::WindowRestore"));
    }

    #[test]
    fn session_dock_does_not_steal_tab_context_menu() {
        let source = include_str!("tab_sidebar.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(!production.contains("environment_menu_hosts"));
    }

    #[test]
    fn environment_rail_is_append_stable_with_member_sections() {
        let sidebar = include_str!("tab_sidebar.rs");
        let production = sidebar.split("#[cfg(test)]").next().unwrap_or(sidebar);
        assert!(
            production.contains("render_environment_rail"),
            "left sidebar must mount append-stable environment sections"
        );
        assert!(
            production.contains("ENV-SESSION-FIRST-RAIL-56"),
            "left sidebar must keep the append-stable rail contract"
        );
        assert!(
            production.contains("render_environment_rail(\n                        sessions"),
            "session list must nest into the rail under the current Environment"
        );
        assert!(
            !production.contains(".child(self.render_environment_rail(window, cx))\n                    .child(sessions)"),
            "sessions must not render as a sibling above/below the environment rail"
        );
        assert!(
            production.contains("SESSION_UNIT_") && production.contains("TabDragIntent::Merge"),
            "Left layout must expose navigator icon-Merge hit zones"
        );
        assert!(
            production.contains("mergeable_tab_id_for_unit"),
            "navigator merge must resolve same-Environment 1+1 live tabs"
        );
        let rail = include_str!("environment_rail.rs");
        let rail_prod = rail.split("#[cfg(test)]").next().unwrap_or(rail);
        assert!(
            rail_prod.contains("environment-rail-current-sessions"),
            "current Environment must own a nested sessions slot"
        );
        assert!(
            rail_prod.contains("let (before, current_section, after)"),
            "rail must use before/current/after so later env headers stay visible"
        );
    }

    #[test]
    fn empty_session_states_render_icon_and_message_hierarchy() {
        let discovering = session_empty_state_kind(true, false, true);
        let scan_error = session_empty_state_kind(false, true, true);
        let empty = session_empty_state_kind(false, false, true);
        let no_match = session_empty_state_kind(false, false, false);
        assert_eq!(discovering, SessionEmptyStateKind::Discovering);
        assert_eq!(scan_error, SessionEmptyStateKind::ScanError);
        assert_eq!(empty, SessionEmptyStateKind::Empty);
        assert_eq!(no_match, SessionEmptyStateKind::NoMatch);
        let icons = [
            discovering.icon().path(),
            scan_error.icon().path(),
            empty.icon().path(),
            no_match.icon().path(),
        ];
        for i in 0..icons.len() {
            for j in (i + 1)..icons.len() {
                assert_ne!(
                    icons[i], icons[j],
                    "each empty state needs a distinct glyph"
                );
            }
        }
    }

    #[test]
    fn live_session_menu_uses_close_not_provider_delete() {
        assert_eq!(
            super::live_session_destructive_action(true),
            "close",
            "live rows must close the carrier, not provider-delete"
        );
        assert_eq!(super::live_session_destructive_action(false), "delete");
    }

    #[test]
    fn live_session_menu_exposes_close_and_delete() {
        assert_eq!(
            super::live_session_menu_actions(true),
            &["close", "close_and_delete"]
        );
        assert_eq!(super::live_session_menu_actions(false), &["delete"]);
    }
}
