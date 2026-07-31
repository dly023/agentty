use gpui::{Context, MouseButton, MouseDownEvent, Window, div, prelude::*, px};
use gpui_component::button::Button;
use gpui_component::input::Input;
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};

use crate::ui::app::{AgenttyApp, TITLE_BAR_HEIGHT};

const MIN_SIDEBAR_WIDTH: f32 = 180.;

const GRAB_HANDLE_W: f32 = 48.;
const MAX_SIDEBAR_WIDTH_RATIO: f32 = 0.5;

const RESIZE_HANDLE_WIDTH: f32 = 8.;

impl AgenttyApp {
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
        let mut list = v_flex()
            .id("agent-session-navigator-list")
            .track_scroll(&self.sidebar_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_1()
            .gap(px(2.));

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
            let active = row.lifecycle == agentty_core::agent_runtime::RowLifecycle::Live;
            let restoring = row.lifecycle == agentty_core::agent_runtime::RowLifecycle::Restoring;
            let status_dot = if active {
                Some(cx.theme().success)
            } else if restoring {
                Some(cx.theme().warning)
            } else {
                None
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
                .items_center()
                .gap_2p5()
                .pl_3()
                .pr_2p5()
                .py_2p5()
                .rounded_md()
                .cursor_pointer()
                .text_color(cx.theme().sidebar_foreground)
                .when(selected, |item| item.bg(cx.theme().sidebar_accent))
                .when(!selected, |item| {
                    item.hover(|item| item.bg(cx.theme().sidebar_accent.opacity(0.5)))
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
                        .relative()
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(cx.theme().muted_foreground)
                        .child(Icon::new(icon).size(px(15.)))
                        .when_some(status_dot, |icon_box, dot| {
                            icon_box.child(
                                div()
                                    .absolute()
                                    .right(px(1.))
                                    .bottom(px(1.))
                                    .size(px(7.))
                                    .rounded_full()
                                    .bg(dot),
                            )
                        }),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().w_full().truncate().text_sm().child(title))
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_1()
                                .text_size(px(10.5))
                                .text_color(cx.theme().muted_foreground)
                                .child(provider)
                                .when(!cwd.is_empty(), |line| {
                                    line.child("·")
                                        .child(div().flex_1().min_w_0().truncate().child(cwd))
                                }),
                        ),
                )
                .map(|item| {
                    if restoring {
                        item.child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::core::i18n::current(cx, "session.restoring")),
                        )
                    } else if !active {
                        item.child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(cx.theme().info)
                                .opacity(0.)
                                .group_hover("agent-session-row", |label| label.opacity(1.))
                                .child(crate::core::i18n::current(cx, "session.resume")),
                        )
                    } else {
                        item
                    }
                });
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
            .gap(px(2.))
            .pr(px(crate::ui::app::tile_trailing_inset()))
            .justify_end()
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

        let search_bar = h_flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(6.))
            .py_1()
            .px_2()
            .child(
                Icon::new(IconName::Search)
                    .size(px(12.))
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .h(px(24.))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
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
                    .child(search_bar)
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
