use gpui::{Context, FontWeight, MouseButton, MouseDownEvent, Window, div, prelude::*, px};
use gpui_component::button::Button;
use gpui_component::input::Input;
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};

use crate::ui::app::{TITLE_BAR_HEIGHT, Tty7App};

const MIN_SIDEBAR_WIDTH: f32 = 180.;

const GRAB_HANDLE_W: f32 = 48.;
const MAX_SIDEBAR_WIDTH_RATIO: f32 = 0.5;

const RESIZE_HANDLE_WIDTH: f32 = 8.;

impl Tty7App {
    pub(crate) fn tab_sidebar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        if !self.session_scan_started {
            self.refresh_session_navigator(cx);
        }
        self.rebuild_session_navigator(cx);

        let max_width = (window.viewport_size().width.as_f32() * MAX_SIDEBAR_WIDTH_RATIO)
            .max(MIN_SIDEBAR_WIDTH);
        let width = self.sidebar_width.get().clamp(MIN_SIDEBAR_WIDTH, max_width);
        let query = self.sidebar_search.read(cx).value().trim().to_lowercase();
        let rows = self.session_navigator.rows().to_vec();
        let remote = crate::core::session::WorkspaceStore::remote_ref(cx, self.workspace);
        let (environment_label, authority) = match remote {
            None => (
                crate::core::i18n::current(cx, "environment.local.label").to_string(),
                crate::core::i18n::current(cx, "environment.local.detail").to_string(),
            ),
            Some(remote) => (
                remote.target.to_string(),
                crate::core::i18n::current(cx, "environment.remote.detail").to_string(),
            ),
        };

        let mut list = v_flex()
            .id("agent-session-navigator-list")
            .track_scroll(&self.sidebar_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_1()
            .py_1p5()
            .gap_0p5();

        let mut visible = 0usize;
        for row in rows {
            let title = row.alias.clone().or(row.title.clone()).unwrap_or_else(|| {
                row.session_id.clone().unwrap_or_else(|| {
                    crate::core::i18n::current(cx, "session.default_name").to_string()
                })
            });
            let cwd = row.cwd.clone().unwrap_or_default();
            let provider = row.agent.slug().to_string();
            if !query.is_empty()
                && !title.to_lowercase().contains(&query)
                && !cwd.to_lowercase().contains(&query)
                && !provider.to_lowercase().contains(&query)
            {
                continue;
            }
            visible += 1;
            let row_id = row.row_id.clone();
            let selected = self
                .session_navigator
                .selected()
                .is_some_and(|selected| selected == &row.row_id);
            let active = row.lifecycle == tty7_core::agent_runtime::RowLifecycle::Live;
            let restoring = row.lifecycle == tty7_core::agent_runtime::RowLifecycle::Restoring;
            let state_label = if active {
                crate::core::i18n::current(cx, "session.live")
            } else if restoring {
                crate::core::i18n::current(cx, "session.restoring")
            } else {
                crate::core::i18n::current(cx, "session.resume")
            };
            let state_color = if active {
                cx.theme().success
            } else {
                cx.theme().muted_foreground
            };
            let icon = match row.agent {
                crate::core::cli_agent::CLIAgent::Codex => IconName::Bot,
                crate::core::cli_agent::CLIAgent::Claude => IconName::Bot,
                _ => IconName::SquareTerminal,
            };
            let item = h_flex()
                .id(gpui::SharedString::from(format!(
                    "agent-session-row-{}",
                    row.row_id.as_str()
                )))
                .group("agent-session-row")
                .w_full()
                .min_h(px(48.))
                .items_center()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded_lg()
                .cursor_pointer()
                .when(selected || active, |item| {
                    item.bg(cx.theme().sidebar_accent)
                        .text_color(cx.theme().sidebar_accent_foreground)
                })
                .when(!(selected || active), |item| {
                    item.text_color(cx.theme().sidebar_foreground)
                        .hover(|item| item.bg(cx.theme().sidebar_accent.opacity(0.65)))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.activate_navigator_row(row_id.clone(), window, cx);
                    }),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(26.))
                        .rounded_md()
                        .bg(cx.theme().secondary)
                        .child(Icon::new(icon).size(px(14.))),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.))
                        .child(div().w_full().truncate().text_sm().child(title))
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_1p5()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(provider)
                                .when(!cwd.is_empty(), |line| {
                                    line.child("·")
                                        .child(div().flex_1().min_w_0().truncate().child(cwd))
                                }),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(state_color)
                        .child(state_label),
                );
            list = list.child(item);
        }

        if visible == 0 {
            let message = if self.session_refresh.is_inflight() {
                crate::core::i18n::current(cx, "session.discovering").to_string()
            } else if let Some(error) = &self.session_scan_error {
                error.clone()
            } else if query.is_empty() {
                crate::core::i18n::current(cx, "session.empty").to_string()
            } else {
                crate::core::i18n::current(cx, "session.no_match").to_string()
            };
            list = list.child(
                v_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_6()
                    .text_center()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(IconName::Bot).size(px(20.)))
                    .child(message),
            );
        }

        let controls = h_flex()
            .flex_shrink_0()
            .h(px(TITLE_BAR_HEIGHT))
            .items_center()
            .justify_end()
            .gap(px(2.))
            .pr(px(crate::ui::app::tile_trailing_inset()))
            .when_some(crate::ui::app::window_mark(), |row, mark| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .pl(px(crate::ui::app::CONTENT_INSET))
                        .child(mark),
                )
                .child(div().flex_1().min_w(px(GRAB_HANDLE_W)))
            })
            .child(
                crate::ui::tab_strip::chrome_tile(
                    Button::new("session-refresh").icon(IconName::WindowRestore),
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

        let heading = v_flex()
            .flex_shrink_0()
            .px(px(crate::ui::app::CONTENT_INSET))
            .pt_2()
            .pb_1()
            .gap_0p5()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(crate::core::i18n::current(cx, "session.title")),
            )
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} · {}", environment_label, authority)),
            );

        let top_bar = h_flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(6.))
            .h(px(40.))
            .px(px(crate::ui::app::CONTENT_INSET))
            .child(
                Icon::new(IconName::Search)
                    .size(px(14.))
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
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
                    .child(heading)
                    .child(top_bar)
                    .child(crate::ui::scrollbar::with_vertical_scrollbar(
                        "agent-session-navigator-scrollbar",
                        list,
                        &self.sidebar_scroll,
                    )),
            )
            .child(handle)
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
