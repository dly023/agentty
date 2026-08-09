//! Shared side-panel chrome: framed search and glyph empty surfaces
//! (UI-SIDE-PANEL-SEARCH-09 / UI-SIDE-PANEL-EMPTY-10).

use gpui::{App, Div, SharedString, div, prelude::*, px};
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};

/// Explicit geometry for the framed side-panel search control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SidePanelSearchMetrics {
    pub height: f32,
    pub icon_gap: f32,
    pub pad_x: f32,
}

pub(crate) fn side_panel_search_metrics() -> SidePanelSearchMetrics {
    SidePanelSearchMetrics {
        height: 30.0,
        icon_gap: 6.0,
        pad_x: 8.0,
    }
}

/// Framed search surface shared by Session Navigator and right-panel filters.
pub(crate) fn side_panel_search_surface(cx: &App) -> Div {
    let metrics = side_panel_search_metrics();
    h_flex()
        .debug_selector(|| "SIDE_PANEL_SEARCH_SURFACE".into())
        .flex_shrink_0()
        .h(px(metrics.height))
        .items_center()
        .gap(px(metrics.icon_gap))
        .px(px(metrics.pad_x))
        .rounded_md()
        .border_1()
        .border_color(cx.theme().sidebar_border)
        .bg(cx.theme().background.opacity(0.55))
}

/// Glyph-above-message empty surface for side panels.
pub(crate) fn side_panel_empty_surface(
    icon: IconName,
    message: impl Into<SharedString>,
    hint: Option<SharedString>,
    cx: &App,
) -> Div {
    let muted = cx.theme().muted_foreground;
    v_flex()
        .debug_selector(|| "SIDE_PANEL_EMPTY_SURFACE".into())
        .w_full()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .px(px(crate::ui::app::panel_content_gutter()))
        .py(px(24.))
        .text_center()
        .child(
            Icon::new(icon)
                .size(px(26.))
                .text_color(muted.opacity(0.55)),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(muted)
                .child(message.into()),
        )
        .when_some(hint, |this, hint| {
            this.child(
                div()
                    .text_size(px(11.))
                    .text_color(muted.opacity(0.75))
                    .child(hint),
            )
        })
}

/// Rounded-rect plate radius for unbranded tab / row avatars
/// (matches agent_icon_badge geometry; UI-AGENT-BRAND-ICON-07).
pub(crate) fn unbranded_avatar_radius(box_size: f32) -> f32 {
    (box_size * 0.28).clamp(4.0, 8.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_panel_search_metrics_are_explicit() {
        let m = side_panel_search_metrics();
        assert_eq!(m.height, 30.0);
        assert_eq!(m.icon_gap, 6.0);
        assert_eq!(m.pad_x, 8.0);
    }

    #[test]
    fn unbranded_avatar_radius_matches_brand_badge_band() {
        assert_eq!(unbranded_avatar_radius(20.0), 5.6);
        assert_eq!(unbranded_avatar_radius(16.0), 4.48);
        assert_eq!(unbranded_avatar_radius(32.0), 8.0);
    }

    #[test]
    fn side_panel_chrome_helpers_are_wired() {
        let right = include_str!("right_panel.rs");
        let left = include_str!("tab_sidebar.rs");
        let strip = include_str!("tab_strip.rs");
        let prod_strip = strip.split("#[cfg(test)]").next().unwrap_or(strip);
        assert!(
            right.contains("side_panel_search_surface")
                && right.contains("side_panel_empty_surface"),
            "right panel must consume shared side-panel chrome"
        );
        assert!(
            left.contains("side_panel_search_surface"),
            "session navigator search must consume the shared framed surface"
        );
        assert!(
            prod_strip.contains("unbranded_avatar_radius"),
            "unbranded tab avatars must use the shared rounded-rect radius"
        );
    }
}
