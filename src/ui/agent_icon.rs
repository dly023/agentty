//! Shared agent avatar badge (UI-AGENT-BRAND-ICON-07).
//!
//! Bundled currentColor SVG silhouettes render as a contrasting glyph on the
//! agent's accent plate — white on dark accents; light-accent agents (Omp)
//! pass a brand/dark glyph so the mark stays visible. Raster / brand-filled
//! Orca marks were reverted after they disappeared at sidebar density.

use gpui::{App, IntoElement, div, prelude::*, px, rgb};

/// One shared badge for Session Navigator rows, tab chips, and similar chrome.
pub(crate) fn agent_icon_badge(
    path: &'static str,
    box_size: f32,
    radius: f32,
    accent: u32,
    glyph: u32,
    glyph_size: f32,
    _cx: &App,
) -> impl IntoElement + use<> {
    div()
        .flex_shrink_0()
        .relative()
        .size(px(box_size))
        .rounded(px(radius))
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(accent))
        .child(
            gpui::svg()
                .path(path)
                .size(px(glyph_size))
                .text_color(rgb(glyph)),
        )
}

#[cfg(test)]
mod tests {
    #[test]
    fn agent_icon_badge_uses_accent_plate_and_contrasting_glyph() {
        let source = include_str!("agent_icon.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            prod.contains(".bg(rgb(accent))")
                && prod.contains("text_color(rgb(glyph))")
                && !prod.contains("agent_icon_preserves_colors")
                && !prod.contains("img("),
            "badge must be accent fill + contrasting currentColor SVG, not Orca raster/preserve paths"
        );
        assert!(
            !prod.contains("text_color(gpui::white())"),
            "light-accent agents need a non-white glyph path; do not hard-code white"
        );
    }

    #[test]
    fn codex_svg_is_optically_scaled_for_badge_fill() {
        let svg = include_str!("../../assets/icons/agents/codex.svg");
        assert!(
            svg.contains("viewBox=\"2 2 20 20\""),
            "Codex blossom must use a cropped viewBox so lace ink fills the shared badge"
        );
        assert!(
            svg.contains("fill=\"#FF0000\"") || svg.contains("fill='#FF0000'"),
            "Codex mark must keep the currentColor sentinel fill"
        );
        assert!(
            !svg.contains("viewBox=\"0 0 24 24\""),
            "full 0 0 24 24 viewBox leaves Codex optically underfilled vs dense siblings"
        );
    }
}
