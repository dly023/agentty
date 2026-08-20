use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, Context, KeyDownEvent, MouseButton, MouseDownEvent, div,
    prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};

use crate::core::session::{SessionPane, SessionTab};
use crate::ui::app::AgenttyApp;

const LOGO: [&str; 4] = [
    " ▄▄▄ ▄▄▄ ▄  ▄ ▄▄▄▄",
    "  █   █  █  █    █",
    "  █   █  ▀▄▄█   █",
    "  ▀▄  ▀▄ ▄▄▄▀  █  ",
];

const LOGO_PX: f32 = 20.0;

const HOME_SHORTCUTS: [(&str, &str); 7] = [
    ("NewTab", "home.shortcut.new_tab"),
    ("ReopenClosedTab", "home.shortcut.reopen_closed"),
    ("ToggleSwitcher", "home.shortcut.switch_workspace"),
    ("TogglePalette", "home.shortcut.palette"),
    ("SplitRight", "home.shortcut.split_right"),
    ("SplitDown", "home.shortcut.split_down"),
    ("OpenSettings", "home.shortcut.settings"),
];

const CLOSED_LABEL_MAX: usize = 20;

fn closed_tab_label(tab: &SessionTab) -> Option<String> {
    if let Some(name) = tab.name.as_ref() {
        let name = name.trim();
        if !name.is_empty() {
            return Some(clamp_label(name));
        }
    }
    first_leaf_cwd(&tab.pane)
        .and_then(|p| p.file_name())
        .map(|s| clamp_label(&s.to_string_lossy()))
}

fn first_leaf_cwd(pane: &SessionPane) -> Option<&std::path::PathBuf> {
    match pane {
        SessionPane::Leaf { cwd, .. } => cwd.as_ref(),
        SessionPane::Split { a, b, .. } => first_leaf_cwd(a).or_else(|| first_leaf_cwd(b)),
    }
}

fn clamp_label(s: &str) -> String {
    if s.chars().count() > CLOSED_LABEL_MAX {
        format!("{}…", s.chars().take(CLOSED_LABEL_MAX).collect::<String>())
    } else {
        s.to_string()
    }
}

pub(crate) const PICKER_PATH_MAX: usize = 34;

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn relative_time(now: u64, then: u64) -> String {
    if then == 0 || then >= now {
        return "just now".to_string();
    }
    let secs = now - then;
    match secs {
        s if s < 60 => "just now".to_string(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 7200 => "1 hour ago".to_string(),
        s if s < 86_400 => format!("{} hours ago", s / 3600),
        s if s < 172_800 => "yesterday".to_string(),
        s if s < 604_800 => format!("{} days ago", s / 86_400),
        _ => "over a week ago".to_string(),
    }
}

/// Local wall-clock for Navigator hover/details Updated metadata.
/// Matches Ashide's `%Y-%m-%d %H:%M` and never returns a raw unix-millis string.
pub(crate) fn format_unix_ms_local(millis: u64) -> Option<String> {
    use chrono::{Local, TimeZone};
    let Ok(millis) = i64::try_from(millis) else {
        return None;
    };
    Local
        .timestamp_millis_opt(millis)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
}

pub(crate) fn format_session_updated_at(millis: Option<u64>, empty: &str) -> String {
    millis
        .and_then(format_unix_ms_local)
        .unwrap_or_else(|| empty.to_string())
}

pub(crate) fn display_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    let shortened = match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && text.starts_with(&home) => {
            format!("~{}", &text[home.len()..])
        }
        _ => text.to_string(),
    };
    if shortened.chars().count() <= PICKER_PATH_MAX {
        return shortened;
    }
    let tail: String = shortened
        .chars()
        .skip(shortened.chars().count() - PICKER_PATH_MAX)
        .collect();
    format!("…{tail}")
}

fn key_hint_tokens(action: &str, cx: &App) -> Option<Vec<String>> {
    let spec = crate::ui::keymap::effective_key(action, cx)?;
    let first = spec.split_whitespace().next()?;
    Some(crate::ui::keymap::key_tokens(first))
}

/// Geometry of the welcome page hierarchy (UI-WELCOME-SURFACE-04).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WelcomeSurfaceMetrics {
    pub column_width: f32,
    pub section_gap: f32,
    pub row_gap: f32,
    pub section_padding: f32,
    pub section_radius: f32,
}

/// One pure metrics mapping for the welcome page.
pub(crate) fn welcome_surface_metrics() -> WelcomeSurfaceMetrics {
    WelcomeSurfaceMetrics {
        column_width: 340.,
        section_gap: 40.,
        row_gap: 8.,
        section_padding: 12.,
        section_radius: 10.,
    }
}

impl AgenttyApp {
    pub(crate) fn render_home(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (muted, foreground, accent) = (theme.muted_foreground, theme.foreground, theme.primary);

        let mut logo = v_flex()
            .font_family(self.font_family.clone())
            .text_size(px(LOGO_PX))
            .line_height(px(LOGO_PX))
            .text_color(muted);
        let (last, head) = LOGO.split_last().expect("LOGO is non-empty");
        for line in head {
            logo = logo.child(*line);
        }
        logo = logo.child(h_flex().child(*last).child(
            div().text_color(accent).child("▌").with_animation(
                "home-cursor-blink",
                Animation::new(Duration::from_millis(1200)).repeat(),
                |cursor, delta| cursor.opacity(if delta < 0.5 { 1.0 } else { 0.0 }),
            ),
        ));

        let metrics = welcome_surface_metrics();
        let closed_hint = self.closed.last().and_then(closed_tab_label);
        let mut list = v_flex().w_full().gap(px(metrics.row_gap)).text_sm();
        for (action, label) in HOME_SHORTCUTS {
            let (label, emphasized) = match (&closed_hint, action) {
                (Some(name), "ReopenClosedTab") => (
                    crate::core::i18n::current_format(cx, "home.reopen_tab", &[("name", name)]),
                    true,
                ),
                _ => (crate::core::i18n::current(cx, label).to_string(), false),
            };
            list = list.child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .text_color(if emphasized { foreground } else { muted })
                    .child(label)
                    .children(key_hint_tokens(action, cx).map(|tokens| {
                        h_flex().gap_1().children(
                            tokens
                                .into_iter()
                                .map(|token| crate::ui::kbd::kbd_chip(token, cx)),
                        )
                    })),
            );
        }
        let shortcuts = v_flex()
            .w(px(metrics.column_width))
            .p(px(metrics.section_padding))
            .rounded(px(metrics.section_radius))
            .border_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .child(list);

        let status = self.render_remote_status_strip(cx);

        v_flex()
            .id("home-page")
            .track_focus(&self.home_focus)
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(metrics.section_gap))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| this.new_tab(window, cx)),
            )
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "enter" && !ev.keystroke.modifiers.modified() {
                    this.new_tab(window, cx);
                }
            }))
            .child(
                v_flex().items_center().gap_3().child(logo).child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(crate::core::i18n::current(cx, "home.tagline")),
                ),
            )
            .children(status)
            .child(shortcuts)
            .with_animation(
                "home-fade-in",
                Animation::new(Duration::from_millis(150)),
                |page, delta| page.opacity(delta),
            )
    }

    /// Quiet content-column placeholder while machine tree hydrate is priming
    /// (UI-STARTUP-TERMINAL-55). Never the vacuous welcome logo page.
    pub(crate) fn render_restoring_surface(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .id("restoring-surface")
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(crate::core::i18n::current(cx, "home.restoring")),
            )
    }

    fn render_remote_status_strip(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let machine = self.remote_machine_label(cx);
        let status = self.remote_status(cx)?;
        let message = status.strip_message(&machine, cx)?;
        let action = status.action_label(cx);
        let severity = crate::ui::notice::NoticeSeverity::Info;
        Some(
            crate::ui::notice::notice_surface(severity, cx)
                .child(crate::ui::notice::notice_icon(severity, cx))
                .child(message)
                .when_some(action, |this, label| {
                    this.child(
                        Button::new("home-remote-status-action")
                            .label(label)
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _window, cx| this.remote_retry(cx)))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()),
                    )
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn leaf(cwd: Option<&str>) -> SessionPane {
        SessionPane::Leaf {
            cwd: cwd.map(PathBuf::from),
            pane_id: None,
            ssh_spec: None,
            live_binding: Default::default(),
        }
    }

    #[test]
    fn closed_tab_label_prefers_the_user_set_name() {
        let tab = SessionTab {
            name: Some("build".into()),
            tree_id: None,
            sidebar_group: None,
            pane: leaf(Some("/work/getty")),
        };
        assert_eq!(closed_tab_label(&tab).as_deref(), Some("build"));
    }

    #[test]
    fn closed_tab_label_falls_back_to_the_first_leaf_cwd_dir_name() {
        let tab = SessionTab {
            name: None,
            tree_id: None,
            sidebar_group: None,
            pane: leaf(Some("/work/getty")),
        };
        assert_eq!(closed_tab_label(&tab).as_deref(), Some("getty"));

        let tab = SessionTab {
            name: Some("   ".into()),
            tree_id: None,
            sidebar_group: None,
            pane: leaf(Some("/work/getty")),
        };
        assert_eq!(closed_tab_label(&tab).as_deref(), Some("getty"));
    }

    #[test]
    fn closed_tab_label_searches_splits_for_the_first_cwd() {
        let tab = SessionTab {
            name: None,
            tree_id: None,
            sidebar_group: None,
            pane: SessionPane::Split {
                axis: crate::core::session::SessionAxis::Horizontal,
                ratio: 0.5,
                a: Box::new(leaf(None)),
                b: Box::new(leaf(Some("/tmp/demo"))),
            },
        };
        assert_eq!(closed_tab_label(&tab).as_deref(), Some("demo"));
    }

    #[test]
    fn closed_tab_label_is_none_when_nothing_is_known() {
        let unnamed = SessionTab {
            name: None,
            tree_id: None,
            sidebar_group: None,
            pane: leaf(None),
        };
        assert_eq!(closed_tab_label(&unnamed), None);
        let root = SessionTab {
            name: None,
            tree_id: None,
            sidebar_group: None,
            pane: leaf(Some("/")),
        };
        assert_eq!(closed_tab_label(&root), None);
    }

    #[test]
    fn closed_tab_label_clamps_runaway_names() {
        let tab = SessionTab {
            name: Some("a".repeat(40)),
            tree_id: None,
            sidebar_group: None,
            pane: leaf(None),
        };
        let label = closed_tab_label(&tab).unwrap();
        assert_eq!(label.chars().count(), CLOSED_LABEL_MAX + 1);
        assert!(label.ends_with('…'));
    }

    #[test]
    fn relative_time_reads_coarsely_across_the_ranges() {
        let now = 10_000_000u64;
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(relative_time(now, now - 30), "just now");
        assert_eq!(relative_time(now, now - 120), "2 min ago");
        assert_eq!(relative_time(now, now - 3600), "1 hour ago");
        assert_eq!(relative_time(now, now - 4 * 3600), "4 hours ago");
        assert_eq!(relative_time(now, now - 90_000), "yesterday");
        assert_eq!(relative_time(now, now - 3 * 86_400), "3 days ago");
        assert_eq!(relative_time(now, now - 30 * 86_400), "over a week ago");
    }

    #[test]
    fn format_unix_ms_local_is_readable_not_raw_millis() {
        // Screenshot regression: hover/details showed 1784303401029 literally.
        let millis = 1_784_303_401_029u64;
        let formatted = format_unix_ms_local(millis).expect("valid unix millis");
        assert_ne!(formatted, millis.to_string());
        assert!(
            regex_is_local_datetime(&formatted),
            "expected YYYY-MM-DD HH:MM, got {formatted}"
        );
        assert_eq!(format_session_updated_at(None, "—"), "—");
        assert_eq!(format_session_updated_at(Some(millis), "—"), formatted);
    }

    fn regex_is_local_datetime(value: &str) -> bool {
        let bytes = value.as_bytes();
        bytes.len() == 16
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes[10] == b' '
            && bytes[13] == b':'
            && bytes[..4].iter().all(u8::is_ascii_digit)
            && bytes[5..7].iter().all(u8::is_ascii_digit)
            && bytes[8..10].iter().all(u8::is_ascii_digit)
            && bytes[11..13].iter().all(u8::is_ascii_digit)
            && bytes[14..16].iter().all(u8::is_ascii_digit)
    }

    #[test]
    fn relative_time_never_renders_a_negative_age() {
        let now = 1_000_000u64;
        assert_eq!(relative_time(now, 0), "just now");
        assert_eq!(relative_time(now, now + 5_000), "just now");
    }

    #[test]
    fn display_path_collapses_home_and_elides_from_the_front() {
        let saved = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", "/Users/tester") };

        assert_eq!(
            display_path(std::path::Path::new("/Users/tester/repo/agentty")),
            "~/repo/agentty"
        );
        assert_eq!(display_path(std::path::Path::new("/opt/work")), "/opt/work");

        let long = display_path(std::path::Path::new(
            "/Users/tester/very/deeply/nested/projects/area/thing",
        ));
        assert!(long.starts_with('…'), "{long} should be front-elided");
        assert!(long.ends_with("thing"), "{long} must keep the tail");
        assert_eq!(long.chars().count(), PICKER_PATH_MAX + 1);

        match saved {
            Some(home) => unsafe { std::env::set_var("HOME", home) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    fn logo_rows_never_exceed_the_first_row_width() {
        let width = LOGO[0].chars().count();
        for row in &LOGO {
            assert!(row.chars().count() <= width, "row {row:?} exceeds {width}");
        }
    }
    #[test]
    fn welcome_surface_metrics_are_explicit() {
        let m = welcome_surface_metrics();
        assert_eq!(m.column_width, 340.);
        assert_eq!(m.section_gap, 40.);
        assert_eq!(m.row_gap, 8.);
        assert_eq!(m.section_padding, 12.);
        assert_eq!(m.section_radius, 10.);
    }
}
