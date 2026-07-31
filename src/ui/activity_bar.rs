use gpui::{
    Context, IntoElement, ParentElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};

use crate::core::cli_agent::{AgentSessionState, AgentStatus};
use crate::ui::app::Tty7App;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityPresentation {
    pub title_key: &'static str,
    pub detail: Option<String>,
    pub structured: bool,
    pub color: u32,
}

pub fn presentation(state: Option<&AgentSessionState>) -> ActivityPresentation {
    let Some(state) = state else {
        return ActivityPresentation {
            title_key: "activity.idle",
            detail: None,
            structured: false,
            color: 0x64748B,
        };
    };
    let (title_key, color) = match state.status {
        AgentStatus::Idle => ("activity.idle", 0x64748B),
        AgentStatus::Working => ("activity.working", 0x3B82F6),
        AgentStatus::Waiting => ("activity.waiting", 0xF59E0B),
        AgentStatus::Done => ("activity.done", 0x22C55E),
    };
    ActivityPresentation {
        title_key,
        detail: state.message.clone(),
        structured: state.rich,
        color,
    }
}

impl Tty7App {
    pub(crate) fn render_activity_bar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let terminal = self.focused_leaf(window, cx)?;
        let state = terminal.read(cx).agent_session();
        if state.is_none() && terminal.read(cx).agent().is_none() {
            return None;
        }
        let view = presentation(state.as_ref());
        let trust_key = if view.structured {
            "activity.rich"
        } else {
            "activity.inferred"
        };
        let title = crate::core::i18n::current(cx, view.title_key);
        let trust = crate::core::i18n::current(cx, trust_key);
        Some(
            h_flex()
                .flex_shrink_0()
                .min_h(px(34.))
                .px_3()
                .gap_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().sidebar)
                .child(div().size(px(7.)).rounded_full().bg(gpui::rgb(view.color)))
                .child(Icon::new(IconName::Bot).size(px(14.)))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_xs().child(title))
                        .when_some(view.detail, |column, detail| {
                            column.child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(detail),
                            )
                        }),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(if view.structured {
                            cx.theme().success
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(trust),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_activity_is_visibly_not_structured() {
        let state = AgentSessionState {
            status: AgentStatus::Working,
            rich: false,
            ..Default::default()
        };
        let view = presentation(Some(&state));
        assert!(!view.structured);
        assert_eq!(view.title_key, "activity.working");
    }

    #[test]
    fn rich_activity_is_presented_as_structured() {
        let state = AgentSessionState {
            status: AgentStatus::Waiting,
            rich: true,
            message: Some("Approve shell".into()),
            ..Default::default()
        };
        let view = presentation(Some(&state));
        assert!(view.structured);
        assert_eq!(view.detail.as_deref(), Some("Approve shell"));
    }
}
