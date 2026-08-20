use gpui::{
    App, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex};

use crate::core::i18n::ResolveLocale;
use crate::terminal::view::TerminalView;
use crate::ui::app::AgenttyApp;
use crate::ui::composer::PaneIdentity;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerFooterModel {
    pub environment_label: String,
    pub environment_detail: String,
    pub environment_color: u32,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub agent_name: Option<String>,
    pub structured_message: Option<String>,
    pub composer_expanded: bool,
}

pub fn composer_footer_model(
    terminal: &TerminalView,
    target: &PaneIdentity,
    composer_expanded: bool,
    app: &AgenttyApp,
    cx: &App,
) -> ComposerFooterModel {
    let locale = cx.global::<crate::core::config::Config>().locale.resolve();
    let remote = crate::core::session::WorkspaceStore::remote_ref(cx, app.workspace);
    let status = remote
        .as_ref()
        .and_then(|_| crate::ui::remote_workspace::RemoteLinks::status_of(cx, app.workspace));
    let remote_label = remote
        .as_ref()
        .map(|remote| crate::ui::remote_connect::label_for(&remote.target, cx));
    let (environment_label, environment_detail, environment_color, _) =
        AgenttyApp::environment_indicator_state(
            remote.as_ref(),
            remote_label.as_deref(),
            status.as_ref(),
            locale,
        );
    let cwd = terminal
        .cwd()
        .map(|path| truncate_middle(&path.display().to_string(), 48));
    let git_branch = terminal.git_status(cx).map(|status| status.branch);
    let agent_name = terminal
        .agent()
        .map(|agent| agent.display_name().to_string())
        .or_else(|| {
            terminal
                .live_binding()
                .agent
                .as_ref()
                .map(|agent| agent.display_name().to_string())
        });
    let structured_message = terminal.agent_session().and_then(|state| {
        (state.rich && state.message.is_some())
            .then(|| state.message.clone())
            .flatten()
    });
    ComposerFooterModel {
        environment_label,
        environment_detail,
        environment_color,
        cwd,
        git_branch,
        agent_name,
        structured_message,
        composer_expanded,
    }
}

fn truncate_middle(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let keep = max_chars.saturating_sub(1) / 2;
    let head: String = text.chars().take(keep).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(max_chars - keep - 1)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

fn chip(label: &str, value: &str, theme: &gpui_component::Theme) -> impl IntoElement + use<> {
    h_flex()
        .gap_1()
        .max_w(px(240.))
        .child(
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label.to_owned()),
        )
        .child(
            div()
                .truncate()
                .text_xs()
                .text_color(theme.foreground)
                .child(value.to_owned()),
        )
}

impl AgenttyApp {
    pub(crate) fn render_composer_context_footer(
        &self,
        terminal: &Entity<TerminalView>,
        target: &PaneIdentity,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let expanded = self.composer_expanded_for(target);
        let model = composer_footer_model(terminal.read(cx), target, expanded, self, cx);
        let toggle_label = crate::core::i18n::current(
            cx,
            if model.composer_expanded {
                "composer.toggle.collapse"
            } else {
                "composer.toggle.expand"
            },
        );
        let target = target.clone();
        let theme = cx.theme().clone();
        h_flex()
            .id("composer-context-footer")
            .debug_selector(|| "COMPOSER_CONTEXT_FOOTER".into())
            .flex_shrink_0()
            .min_h(px(30.))
            .px_3()
            .gap_2()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .items_center()
            .child(
                div()
                    .size(px(7.))
                    .rounded_full()
                    .bg(gpui::rgb(model.environment_color)),
            )
            .child(chip(
                &crate::core::i18n::current(cx, "composer.footer.environment"),
                &format!("{} · {}", model.environment_label, model.environment_detail),
                &theme,
            ))
            .when_some(model.cwd.clone(), |row, cwd| {
                row.child(chip(
                    &crate::core::i18n::current(cx, "composer.footer.cwd"),
                    &cwd,
                    &theme,
                ))
            })
            .when_some(model.git_branch.clone(), |row, branch| {
                row.child(chip(
                    &crate::core::i18n::current(cx, "composer.footer.branch"),
                    &branch,
                    &theme,
                ))
            })
            .when_some(model.agent_name.clone(), |row, agent| {
                row.child(chip(
                    &crate::core::i18n::current(cx, "composer.footer.agent"),
                    &agent,
                    &theme,
                ))
            })
            .child(div().flex_1().min_w_0())
            .when_some(model.structured_message.clone(), |row, message| {
                row.child(
                    div()
                        .truncate()
                        .max_w(px(280.))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(message),
                )
            })
            .child(
                div()
                    .id("composer-context-footer-toggle")
                    .debug_selector(|| "COMPOSER_CONTEXT_FOOTER_TOGGLE".into())
                    .flex_shrink_0()
                    .cursor_pointer()
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(toggle_label.clone())
                            .build(window, cx)
                    })
                    .on_click({
                        let target = target.clone();
                        cx.listener(move |this, _, window, cx| {
                            this.toggle_composer_from_footer(&target, window, cx);
                        })
                    })
                    .child(
                        Icon::new(if model.composer_expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronUp
                        })
                        .size(px(12.))
                        .text_color(theme.muted_foreground),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_middle_shortens_long_paths() {
        assert_eq!(truncate_middle("/short", 48), "/short");
        let long = "/very/long/path/that/exceeds/the/limit/significantly";
        let truncated = truncate_middle(long, 20);
        assert!(truncated.contains('…'));
        assert!(truncated.chars().count() <= 20);
    }

    #[test]
    fn composer_footer_model_includes_environment_and_cwd() {
        let src = include_str!("composer_context_footer.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("module source");
        assert!(src.contains("environment_indicator_state"));
        assert!(src.contains("terminal.cwd()"));
        assert!(src.contains("environment_label"));
        assert!(src.contains("git_branch"));
    }

    #[test]
    fn footer_omits_inferred_trust_badge() {
        let src = include_str!("composer_context_footer.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("module source");
        assert!(!src.contains("activity.inferred"));
        assert!(!src.contains("activity.rich"));
        assert!(!src.contains("presentation("));
    }
}
