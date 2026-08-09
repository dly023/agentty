//! Shared floating notice surface (UI-NOTICE-SURFACE-01).
//!
//! Every floating or inline status notice in the window derives its chrome
//! from one typed severity: border tone and leading glyph come from the same
//! pure mapping, and call sites only supply the message and actions.

use gpui::{App, Div, prelude::*, px};
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex};

/// Severity of a floating status notice.
#[derive(Clone, Copy, Debug)]
pub(crate) enum NoticeSeverity {
    Info,
    Warning,
    Danger,
}

/// Leading glyph for a notice severity.
pub(crate) fn notice_severity_icon(severity: NoticeSeverity) -> IconName {
    match severity {
        NoticeSeverity::Info => IconName::Info,
        NoticeSeverity::Warning => IconName::TriangleAlert,
        NoticeSeverity::Danger => IconName::CircleX,
    }
}

/// One shared notice container: popover fill, severity border tone, rounded
/// corners, shadow, and compact text. Callers append the leading glyph,
/// message, and action children.
pub(crate) fn notice_surface(severity: NoticeSeverity, cx: &App) -> Div {
    let theme = cx.theme();
    let border = match severity {
        NoticeSeverity::Info => theme.border,
        NoticeSeverity::Warning => theme.warning.opacity(0.4),
        NoticeSeverity::Danger => theme.danger.opacity(0.4),
    };
    h_flex()
        .occlude()
        .items_center()
        .gap_2()
        .px_3()
        .py_1p5()
        .rounded_lg()
        .bg(theme.popover)
        .border_1()
        .border_color(border)
        .shadow_md()
        .text_xs()
        .text_color(theme.muted_foreground)
}

/// Severity glyph rendered with the shared muted color.
pub(crate) fn notice_icon(severity: NoticeSeverity, cx: &App) -> Icon {
    Icon::new(notice_severity_icon(severity))
        .size(px(14.))
        .text_color(match severity {
            NoticeSeverity::Info => cx.theme().muted_foreground,
            NoticeSeverity::Warning => cx.theme().warning,
            NoticeSeverity::Danger => cx.theme().danger,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::IconNamed as _;

    #[test]
    fn notice_surface_maps_severity_to_icon_and_color() {
        assert!(
            notice_severity_icon(NoticeSeverity::Info)
                .path()
                .ends_with("info.svg")
        );
        assert!(
            notice_severity_icon(NoticeSeverity::Warning)
                .path()
                .ends_with("triangle-alert.svg")
        );
        assert!(
            notice_severity_icon(NoticeSeverity::Danger)
                .path()
                .ends_with("circle-x.svg")
        );
        let paths = [
            notice_severity_icon(NoticeSeverity::Info).path(),
            notice_severity_icon(NoticeSeverity::Warning).path(),
            notice_severity_icon(NoticeSeverity::Danger).path(),
        ];
        for i in 0..paths.len() {
            for j in (i + 1)..paths.len() {
                assert_ne!(paths[i], paths[j], "each severity needs a distinct glyph");
            }
        }
    }
}
