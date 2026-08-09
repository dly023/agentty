//! Shared kbd chip surface (UI-KBD-CHIP-03).
//!
//! Every keyboard shortcut hint in the window renders through this one
//! helper so the home page, command palette, and settings keybinding editor
//! cannot drift into divergent keycap chrome.

use gpui::{App, Div, Hsla, SharedString, div, prelude::*, px};
use gpui_component::ActiveTheme as _;

/// Geometry of one kbd chip token.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct KbdChipMetrics {
    pub height: f32,
    pub min_width: f32,
    pub padding_x: f32,
    pub radius: f32,
}

/// One pure metrics mapping for every kbd chip in the window.
pub(crate) fn kbd_chip_metrics() -> KbdChipMetrics {
    KbdChipMetrics {
        height: 20.,
        min_width: 20.,
        padding_x: 4.,
        radius: 6.,
    }
}

/// The canonical kbd chip palette: (fill, border, ink).
pub(crate) fn kbd_chip_colors(cx: &App) -> (Hsla, Hsla, Hsla) {
    let theme = cx.theme();
    (
        theme.secondary.opacity(0.6),
        theme.border,
        theme.muted_foreground,
    )
}

/// One boxed keycap token: muted fill, border, radius, centered text.
pub(crate) fn kbd_chip(token: impl Into<SharedString>, cx: &App) -> Div {
    let (fill, border, ink) = kbd_chip_colors(cx);
    kbd_chip_with(token, fill, border, ink)
}

/// Color-explicit variant for call sites that already resolved the theme
/// (e.g. inside closures that cannot borrow `cx`).
pub(crate) fn kbd_chip_with(
    token: impl Into<SharedString>,
    fill: Hsla,
    border: Hsla,
    ink: Hsla,
) -> Div {
    let m = kbd_chip_metrics();
    div()
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(m.min_width))
        .h(px(m.height))
        .px(px(m.padding_x))
        .rounded(px(m.radius))
        .bg(fill)
        .border_1()
        .border_color(border)
        .text_xs()
        .text_color(ink)
        .child(token.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kbd_chip_metrics_are_explicit() {
        let m = kbd_chip_metrics();
        assert_eq!(m.height, 20.);
        assert_eq!(m.min_width, 20.);
        assert_eq!(m.padding_x, 4.);
        assert_eq!(m.radius, 6.);
    }
}
