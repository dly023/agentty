use std::collections::HashMap;

use agentty_core::core::environment::EnvironmentId;

/// Stable product identity for a terminal pane. GPUI entity ids and focus are
/// deliberately excluded: neither is stable enough to authorize delivery.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PaneIdentity {
    pub environment: EnvironmentId,
    pub pane_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputDelivery {
    AgentPrompt(String),
    CommandLine(String),
    Resume(agentty_core::agent_runtime::ResumeInvocation),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Delivered,
    TargetMissing,
    EnvironmentMismatch,
    Disconnected,
}

#[derive(Default)]
pub struct ComposerDraftStore {
    drafts: HashMap<PaneIdentity, String>,
}

impl ComposerDraftStore {
    pub fn get(&self, target: &PaneIdentity) -> &str {
        self.drafts.get(target).map(String::as_str).unwrap_or("")
    }

    pub fn set(&mut self, target: PaneIdentity, draft: String) {
        if draft.is_empty() {
            self.drafts.remove(&target);
        } else {
            self.drafts.insert(target, draft);
        }
    }

    pub fn clear(&mut self, target: &PaneIdentity) {
        self.drafts.remove(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentty_core::core::environment::EnvironmentDescriptor;

    fn pane(id: u64) -> PaneIdentity {
        PaneIdentity {
            environment: EnvironmentDescriptor::local().id,
            pane_id: id,
        }
    }

    #[test]
    fn drafts_are_isolated_per_pane() {
        let mut drafts = ComposerDraftStore::default();
        drafts.set(pane(1), "one".into());
        drafts.set(pane(2), "two".into());
        assert_eq!(drafts.get(&pane(1)), "one");
        assert_eq!(drafts.get(&pane(2)), "two");
    }

    #[test]
    fn successful_delivery_clears_only_target_draft() {
        let mut drafts = ComposerDraftStore::default();
        drafts.set(pane(1), "one".into());
        drafts.set(pane(2), "two".into());
        drafts.clear(&pane(1));
        assert_eq!(drafts.get(&pane(1)), "");
        assert_eq!(drafts.get(&pane(2)), "two");
    }
}

use gpui::{
    AppContext as _, Entity, IntoElement, ParentElement as _, Styled as _, Window, div, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, WindowExt as _};

pub struct ComposerState {
    pub target: PaneIdentity,
    pub input: Entity<InputState>,
}

impl crate::ui::app::AgenttyApp {
    pub(crate) fn toggle_composer(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if let Some(state) = self.composer.take() {
            let draft = state.input.read(cx).value().to_string();
            self.composer_drafts.set(state.target, draft);
            self.focus_active(window, cx);
            cx.notify();
            return;
        }
        let Some(terminal) = self.focused_leaf(window, cx) else {
            return;
        };
        let target = PaneIdentity {
            environment: crate::core::session::WorkspaceStore::environment_id(cx, self.workspace),
            pane_id: terminal.read(cx).pane_id(),
        };
        let initial = self.composer_drafts.get(&target).to_owned();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .placeholder(crate::core::i18n::current(cx, "composer.placeholder"))
                .default_value(initial)
        });
        let weak = cx.weak_entity();
        cx.subscribe_in(&input, window, move |_this, _input, event, window, cx| {
            if matches!(
                event,
                gpui_component::input::InputEvent::PressEnter { shift: false, .. }
            ) {
                let _ = weak.update(cx, |this, cx| this.submit_composer(window, cx));
            }
        })
        .detach();
        input.update(cx, |input, cx| input.focus(window, cx));
        self.composer = Some(ComposerState { target, input });
        cx.notify();
    }

    fn target_terminal(
        &self,
        target: &PaneIdentity,
        cx: &gpui::App,
    ) -> Result<Entity<crate::terminal::view::TerminalView>, DeliveryOutcome> {
        let environment = crate::core::session::WorkspaceStore::environment_id(cx, self.workspace);
        if environment != target.environment {
            return Err(DeliveryOutcome::EnvironmentMismatch);
        }
        self.tabs
            .iter()
            .flat_map(|tab| tab.pane.terminals())
            .find(|terminal| terminal.read(cx).pane_id() == target.pane_id)
            .ok_or(DeliveryOutcome::TargetMissing)
    }

    fn submit_composer(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(state) = self.composer.as_ref() else {
            return;
        };
        let target = state.target.clone();
        let draft = state.input.read(cx).value().to_string();
        if draft.trim().is_empty() {
            return;
        }
        self.composer_drafts.set(target.clone(), draft.clone());
        let outcome = match self.target_terminal(&target, cx) {
            Ok(terminal) => terminal
                .read(cx)
                .deliver_input(&InputDelivery::AgentPrompt(draft), cx),
            Err(outcome) => outcome,
        };
        match outcome {
            DeliveryOutcome::Delivered => {
                self.composer_drafts.clear(&target);
                self.composer = None;
                self.focus_active(window, cx);
            }
            DeliveryOutcome::TargetMissing => window.push_notification(
                crate::core::i18n::current(cx, "composer.target_missing"),
                cx,
            ),
            DeliveryOutcome::EnvironmentMismatch => window.push_notification(
                crate::core::i18n::current(cx, "composer.environment_mismatch"),
                cx,
            ),
            DeliveryOutcome::Disconnected => window
                .push_notification(crate::core::i18n::current(cx, "composer.disconnected"), cx),
        }
        cx.notify();
    }

    pub(crate) fn render_composer(
        &mut self,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Option<impl IntoElement> {
        let state = self.composer.as_ref()?;
        Some(
            div()
                .absolute()
                .left(px(16.))
                .right(px(16.))
                .bottom(px(16.))
                .p(px(8.))
                .rounded(px(10.))
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .shadow_lg()
                .child(Input::new(&state.input)),
        )
    }
}
