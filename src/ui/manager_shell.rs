//! Presentation only. Machine and session ownership belongs to each caller.
use gpui::{App, Div, Entity, Window, div, prelude::*, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
pub(super) const CARD_W: f32 = 840.;
pub(super) const LEFT_W: f32 = 340.;
pub(super) const CARD_TOP: f32 = 120.;
pub(super) const CARD_MARGIN: f32 = 24.;
const CHROME_H: f32 = 84.;
const BODY_H: f32 = 420.;

#[derive(Clone, Copy, Debug)]
pub(super) struct Layout {
    pub width: f32,
    pub top: f32,
    pub height: f32,
    pub body_height: f32,
    pub left_width: f32,
}

impl Layout {
    pub fn new(width: f32, height: f32) -> Self {
        let width = CARD_W.min((width - 2. * CARD_MARGIN).max(0.));
        let margin = CARD_MARGIN.min(height.max(0.) / 2.);
        let top = CARD_TOP.min((height - CHROME_H - BODY_H - margin).max(margin));
        let height = (CHROME_H + BODY_H).min((height - top - margin).max(0.));
        let body_height = (height - CHROME_H).max(0.);
        Self {
            width,
            top,
            height,
            body_height,
            left_width: LEFT_W.min(width * 0.5),
        }
    }
}

pub(super) fn layout(window: &Window) -> Layout {
    let viewport = window.viewport_size();
    Layout::new(viewport.width.as_f32(), viewport.height.as_f32())
}

pub(super) fn card(width: f32, cx: &App) -> Div {
    v_flex()
        .w(px(width))
        .min_w_0()
        .min_h_0()
        .bg(cx.theme().popover)
        .border_1()
        .border_color(cx.theme().border)
        .rounded(px(10.))
        .shadow_xl()
        .overflow_hidden()
}

pub(super) fn overlay(layout: Layout, cx: &App) -> Div {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_start()
        .justify_center()
        .pt(px(layout.top))
        .bg(super::presets::scrim_fill(cx))
}

pub(super) fn search(query: &Entity<InputState>, cx: &App) -> Div {
    h_flex()
        .items_center()
        .gap(px(8.))
        .pl(px(14.))
        .pr(px(12.))
        .h(px(42.))
        .flex_shrink_0()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .w(px(26.))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .child(
                    Icon::new(IconName::Search)
                        .size(px(16.))
                        .text_color(cx.theme().muted_foreground),
                ),
        )
        .child(Input::new(query).appearance(false).small().pl_0())
}

/// Wrap a cursor in a nonempty visible list. Callers own empty-list semantics.
pub(super) fn step(at: usize, n: usize, forward: bool) -> usize {
    match forward {
        true => (at + 1) % n,
        false => (at + n - 1) % n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_shell_fits_small_viewports() {
        for width in [0., 32., 240., 375., 768., 1440.] {
            for height in [0., 32., 140., 300., 667., 900.] {
                let layout = Layout::new(width, height);
                assert!(layout.width >= 0. && layout.width <= width, "{layout:?}");
                assert!(
                    layout.top >= 0. && layout.top + layout.height <= height,
                    "{layout:?}"
                );
                assert!(layout.body_height >= 0. && layout.body_height <= layout.height);
                assert!(layout.left_width >= 0. && layout.left_width <= layout.width * 0.5);
            }
        }
    }

    #[test]
    fn manager_shell_preserves_switcher_proportions() {
        let layout = Layout::new(1440., 900.);
        assert_eq!(layout.width, 840.);
        assert_eq!(layout.top, 120.);
        assert_eq!(layout.height, 504.);
        assert_eq!(layout.body_height, 420.);
        assert_eq!(layout.left_width, 340.);
    }
}
