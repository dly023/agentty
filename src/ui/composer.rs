use std::collections::HashMap;

use agentty_core::core::config::{ComposerMode, composer_should_dock};
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

#[derive(Default)]
pub struct ComposerVisibilityOverrides {
    overrides: HashMap<PaneIdentity, bool>,
}

impl ComposerVisibilityOverrides {
    pub fn resolve(&self, target: &PaneIdentity, mode: ComposerMode, has_agent: bool) -> bool {
        self.overrides
            .get(target)
            .copied()
            .unwrap_or_else(|| composer_should_dock(mode, has_agent))
    }

    pub fn toggle(&mut self, target: &PaneIdentity, mode: ComposerMode, has_agent: bool) -> bool {
        let expanded = !self.resolve(target, mode, has_agent);
        self.overrides.insert(target.clone(), expanded);
        expanded
    }

    pub fn clear(&mut self) {
        self.overrides.clear();
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

    #[test]
    fn composer_mode_cycles_and_auto_hides_plain_shells() {
        assert_eq!(ComposerMode::Auto.next(), ComposerMode::Always);
        assert_eq!(ComposerMode::Always.next(), ComposerMode::Off);
        assert_eq!(ComposerMode::Off.next(), ComposerMode::Auto);
        assert!(composer_should_dock(ComposerMode::Auto, true));
        assert!(composer_should_dock(ComposerMode::Auto, false));
        assert!(composer_should_dock(ComposerMode::Always, false));
        assert!(!composer_should_dock(ComposerMode::Off, true));
    }

    #[test]
    fn composer_auto_mode_is_available_for_plain_terminals() {
        assert!(composer_should_dock(ComposerMode::Auto, false));
    }

    #[test]
    fn activity_bar_toggles_composer_without_mutating_mode() {
        let mode = ComposerMode::Auto;
        let target = pane(7);
        let mut overrides = ComposerVisibilityOverrides::default();

        assert!(overrides.resolve(&target, mode, true));
        assert!(!overrides.toggle(&target, mode, true));
        assert_eq!(mode, ComposerMode::Auto);
        assert!(!overrides.resolve(&target, mode, true));
        assert!(overrides.toggle(&target, mode, true));
        assert_eq!(mode, ComposerMode::Auto);
    }
}

use gpui::{
    AppContext as _, Entity, Focusable as _, IntoElement, ParentElement as _, Styled as _, Window,
    div, prelude::FluentBuilder as _, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, WindowExt as _};

pub struct ComposerState {
    pub target: PaneIdentity,
    pub input: Entity<InputState>,
}

impl crate::ui::app::AgenttyApp {
    fn focused_has_cli_agent(&self, window: &Window, cx: &gpui::App) -> bool {
        let Some(terminal) = self.focused_leaf(window, cx) else {
            return false;
        };
        let view = terminal.read(cx);
        view.agent().is_some()
            || view.live_binding().agent.is_some()
            || view.agent_session().is_some()
    }

    fn focused_composer_target(&self, window: &Window, cx: &gpui::App) -> Option<PaneIdentity> {
        let terminal = self.focused_leaf(window, cx)?;
        Some(PaneIdentity {
            environment: crate::core::session::WorkspaceStore::environment_id(cx, self.workspace),
            pane_id: terminal.read(cx).pane_id(),
        })
    }

    fn close_composer(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if let Some(state) = self.composer.take() {
            let draft = state.input.read(cx).value().to_string();
            self.composer_drafts.set(state.target, draft);
            self.composer_completion_close(cx);
        }
        self.focus_active(window, cx);
    }

    pub(crate) fn composer_expanded_for(&self, target: &PaneIdentity) -> bool {
        self.composer
            .as_ref()
            .is_some_and(|state| state.target == *target)
    }

    pub(crate) fn toggle_composer_from_activity_bar(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(target) = self.focused_composer_target(window, cx) else {
            return;
        };
        let mode = cx.global::<crate::core::config::Config>().composer_mode;
        let has_agent = self.focused_has_cli_agent(window, cx);
        if self.composer_visibility.toggle(&target, mode, has_agent) {
            self.ensure_composer_open(window, cx);
            if let Some(state) = self.composer.as_ref() {
                let focus = state.input.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
        } else {
            self.close_composer(window, cx);
        }
        cx.notify();
    }

    /// Cycle persisted composer_mode (auto → always → off → auto) and sync dock.
    pub(crate) fn toggle_composer(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let next = cx
            .global::<crate::core::config::Config>()
            .composer_mode
            .next();
        self.update_config(cx, |cfg| cfg.composer_mode = next);
        self.composer_visibility.clear();
        let label = crate::core::i18n::current_format(
            cx,
            "composer.mode_switched",
            &[("mode", crate::core::i18n::current(cx, mode_i18n_key(next)))],
        );
        window.push_notification(label, cx);
        let _ = self.sync_composer_dock(window, cx);
        cx.notify();
    }

    /// Re-evaluate dock visibility from preference + focused agent.
    /// Returns true when composer presence changed (caller may notify).
    pub(crate) fn sync_composer_dock(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let mode = cx.global::<crate::core::config::Config>().composer_mode;
        let target = self.focused_composer_target(window, cx);
        let has_agent = self.focused_has_cli_agent(window, cx);
        let should = target
            .as_ref()
            .is_some_and(|target| self.composer_visibility.resolve(target, mode, has_agent));
        let was_open = self.composer.is_some();
        if should {
            self.ensure_composer_open(window, cx);
        } else {
            self.close_composer(window, cx);
        }
        was_open != self.composer.is_some()
    }

    fn ensure_composer_open(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(terminal) = self.focused_leaf(window, cx) else {
            return;
        };
        let target = PaneIdentity {
            environment: crate::core::session::WorkspaceStore::environment_id(cx, self.workspace),
            pane_id: terminal.read(cx).pane_id(),
        };
        if let Some(state) = self.composer.as_ref() {
            if state.target == target {
                return;
            }
            let draft = state.input.read(cx).value().to_string();
            self.composer_drafts.set(state.target.clone(), draft);
            self.composer = None;
            self.composer_completion = None;
        }
        let initial = self.composer_drafts.get(&target).to_owned();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                // Keep this explicit. `auto_grow` is a bounded layout policy,
                // while multiline is the editing contract for Shift+Enter.
                .multi_line(true)
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .placeholder(crate::core::i18n::current(cx, "composer.placeholder"))
                .default_value(initial)
        });
        cx.subscribe_in(
            &input,
            window,
            move |this, _input, event, window, cx| match event {
                gpui_component::input::InputEvent::PressEnter { shift: false, .. } => {
                    if this.composer_completion.is_some() {
                        this.composer_completion_accept(window, cx);
                    } else {
                        this.submit_composer(window, cx);
                    }
                }
                gpui_component::input::InputEvent::Change => {
                    this.composer_completion_refilter(cx);
                }
                _ => {}
            },
        )
        .detach();
        self.composer = Some(ComposerState { target, input });
        cx.notify();
    }

    pub(crate) fn composer_input_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.composer.as_ref().is_some_and(|state| {
            state
                .input
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        })
    }

    /// Route Tab to the focused editable surface (INPUT-COMPLETION-EDITOR-SURFACE-06).
    pub(crate) fn complete_focused_surface(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let composer_focused = self.composer_input_focused(window, cx);
        let terminal = self.focused_leaf(window, cx);
        let terminal_active = terminal
            .as_ref()
            .is_some_and(|leaf| leaf.read(cx).input_active_for_completion());
        match crate::ui::completion_surface::completion_focus_owner(
            composer_focused,
            terminal_active,
        ) {
            Some(crate::ui::completion_surface::CompletionFocusOwner::Composer) => {
                self.composer_complete_tab(forward, window, cx);
            }
            Some(crate::ui::completion_surface::CompletionFocusOwner::Terminal) => {
                if let Some(leaf) = terminal {
                    leaf.update(cx, |view, cx| view.tab_pressed(forward, cx));
                }
            }
            None => {}
        }
    }

    pub(crate) fn composer_complete_tab(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if !cx.global::<crate::core::config::Config>().tab_completion {
            return;
        }
        if self.composer_completion.is_some() {
            if let Some(state) = self.composer_completion.as_mut() {
                state.session.select(forward);
            }
            cx.notify();
            return;
        }
        let Some(composer) = self.composer.as_ref() else {
            return;
        };
        let target = composer.target.clone();
        let input = composer.input.clone();
        let (line, cursor_chars) = {
            let state = input.read(cx);
            let line = state.value().to_string();
            let cursor_chars =
                crate::ui::completion_surface::byte_offset_to_char_index(&line, state.cursor());
            (line, cursor_chars)
        };
        let Ok(terminal) = self.target_terminal(&target, cx) else {
            return;
        };
        let (host, cwd, history, paths_local) = {
            let view = terminal.read(cx);
            (
                view.host(cx),
                view.cwd(),
                view.completion_history(),
                view.paths_are_local(),
            )
        };
        let local_cwd = paths_local
            .then(|| cwd.clone().or_else(|| std::env::current_dir().ok()))
            .flatten();
        let mut gui =
            crate::terminal::completion::complete(&line, cursor_chars, local_cwd.as_deref())
                .unwrap_or(crate::terminal::completion::Completion {
                    candidates: Vec::new(),
                    pending: Vec::new(),
                });
        if let Some(host) = host {
            let cursor_bytes = line
                .char_indices()
                .nth(cursor_chars)
                .map(|(offset, _)| offset)
                .unwrap_or(line.len());
            let authority = if paths_local {
                agentty_core::agent_runtime::AuthorityKind::Local
            } else {
                agentty_core::agent_runtime::AuthorityKind::Remote
            };
            let generation = self.composer_completion_generation.wrapping_add(1);
            let request = agentty_core::agent_runtime::CompletionRequest {
                operation: agentty_core::agent_runtime::OperationId(generation),
                generation: agentty_core::agent_runtime::CompletionGeneration(generation),
                authority,
                cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
                input: line.clone(),
                cursor: cursor_bytes,
                limit: 400,
                history,
            };
            if let agentty_core::agent_runtime::CompletionOutcome::Complete(candidates) =
                agentty_core::agent_runtime::complete(&*host, &request)
            {
                let mapped = candidates.into_iter().map(|candidate| {
                    let start = line[..candidate.replacement.start.min(line.len())]
                        .chars()
                        .count();
                    let end = line[..candidate.replacement.end.min(line.len())]
                        .chars()
                        .count();
                    let kind = match candidate.source {
                        agentty_core::agent_runtime::CompletionSourceKind::Filesystem => {
                            if candidate.value.ends_with('/') {
                                crate::terminal::completion::CandidateKind::Dir
                            } else {
                                crate::terminal::completion::CandidateKind::File
                            }
                        }
                        agentty_core::agent_runtime::CompletionSourceKind::Grammar => {
                            crate::terminal::completion::CandidateKind::Command
                        }
                        _ => crate::terminal::completion::CandidateKind::Value,
                    };
                    crate::terminal::completion::Candidate {
                        text: candidate.value,
                        kind,
                        start,
                        end,
                        description: Some(candidate.display).filter(|d| !d.is_empty()),
                        icon: None,
                    }
                });
                gui.candidates =
                    crate::ui::completion_surface::merge_candidates_by_text(gui.candidates, mapped);
            }
        }
        if gui.candidates.is_empty() {
            return;
        }
        let (word_start, _) = match gui.candidates.first() {
            Some(c) => (c.start, c.end),
            None => return,
        };
        let word: String = line
            .chars()
            .skip(word_start)
            .take(cursor_chars.saturating_sub(word_start))
            .collect();
        self.composer_completion_generation = self.composer_completion_generation.wrapping_add(1);
        let generation = self.composer_completion_generation;
        self.composer_completion =
            Some(crate::ui::completion_surface::ComposerCompletionState::new(
                generation,
                crate::terminal::completion::CompletionSession::new(
                    word_start,
                    word,
                    gui.candidates,
                ),
            ));
        let _ = window;
        cx.notify();
    }

    fn composer_completion_refilter(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(composer) = self.composer.as_ref() else {
            return;
        };
        let line = composer.input.read(cx).value().to_string();
        let cursor_chars = crate::ui::completion_surface::byte_offset_to_char_index(
            &line,
            composer.input.read(cx).cursor(),
        );
        let Some(state) = self.composer_completion.as_mut() else {
            return;
        };
        let word: String = line
            .chars()
            .skip(state.session.word_start)
            .take(cursor_chars.saturating_sub(state.session.word_start))
            .collect();
        if !state.session.refilter(&word) {
            self.composer_completion = None;
        }
        cx.notify();
    }

    fn composer_completion_accept(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(composer) = self.composer.as_ref() else {
            return;
        };
        let input = composer.input.clone();
        let draft = input.read(cx).value().to_string();
        let Some(state) = self.composer_completion.as_ref() else {
            return;
        };
        let Some(accepted) =
            crate::ui::completion_surface::accept_into_draft(&draft, &state.session)
        else {
            return;
        };
        debug_assert!(!accepted.submit);
        let position = crate::ui::completion_surface::char_index_to_position(
            &accepted.text,
            accepted.cursor_chars,
        );
        input.update(cx, |state, cx| {
            state.set_value(&accepted.text, window, cx);
            state.set_cursor_position(position, window, cx);
        });
        self.composer_completion = None;
        cx.notify();
    }

    fn composer_completion_close(&mut self, cx: &mut gpui::Context<Self>) {
        if crate::ui::completion_surface::clear_composer_completion(&mut self.composer_completion) {
            cx.notify();
        }
    }

    fn render_composer_completion_menu(&self, cx: &gpui::App) -> Option<impl IntoElement + use<>> {
        let state = self.composer_completion.as_ref()?;
        let items: Vec<&crate::terminal::completion::Candidate> = state
            .session
            .filtered
            .iter()
            .map(|&i| &state.session.all[i])
            .collect();
        if items.is_empty() {
            return None;
        }
        const MAX_ROWS: usize = 8;
        let first = 0usize;
        let visible = items.len().min(MAX_ROWS);
        let theme = cx.theme();
        let rows: Vec<_> = (first..first + visible)
            .map(|i| {
                let cand = items[i];
                let selected = state.session.index == Some(i);
                let label = if cand.is_dir() && !cand.text.ends_with('/') {
                    format!("{}/", cand.text)
                } else {
                    cand.text.clone()
                };
                div()
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .whitespace_nowrap()
                    .when(selected, |d| {
                        d.bg(theme.list_active).text_color(theme.foreground)
                    })
                    .child(div().flex_shrink_0().child(label))
                    .when_some(cand.description.clone(), |d, desc| {
                        d.child(div().ml_2().text_color(theme.muted_foreground).child(desc))
                    })
            })
            .collect();
        Some(
            div()
                .mb_1()
                .max_h(px(200.))
                .overflow_hidden()
                .rounded(px(8.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_md()
                .py_1()
                .children(rows),
        )
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
                .deliver_input(&InputDelivery::AgentPrompt(draft.clone()), cx),
            Err(outcome) => outcome,
        };
        match outcome {
            DeliveryOutcome::Delivered => {
                self.composer_drafts.clear(&target);
                if let Some(state) = self.composer.as_ref() {
                    state.input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                }
                if let Ok(terminal) = self.target_terminal(&target, cx) {
                    let stamped = terminal.update(cx, |view, cx| {
                        let changed = view.note_first_user_prompt(&draft);
                        if changed {
                            cx.notify();
                        }
                        changed
                    });
                    if stamped {
                        self.rebuild_session_navigator(cx);
                    }
                }
                let mode = cx.global::<crate::core::config::Config>().composer_mode;
                if mode == ComposerMode::Off {
                    self.composer = None;
                }
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
    ) -> Option<impl IntoElement + use<>> {
        let state = self.composer.as_ref()?;
        let menu = self.render_composer_completion_menu(cx);
        Some(
            div()
                .flex_shrink_0()
                .gap_1()
                .px_2()
                .py_1()
                .bg(cx.theme().background)
                .border_t_1()
                .border_color(cx.theme().border)
                .children(menu)
                // The dock's top divider is the only Composer frame. The input
                // control must stay borderless so it reads as one continuous
                // rich-input area rather than a box inside a box.
                .child(Input::new(&state.input).appearance(false)),
        )
    }
}

fn mode_i18n_key(mode: ComposerMode) -> &'static str {
    match mode {
        ComposerMode::Auto => "composer.mode.auto",
        ComposerMode::Always => "composer.mode.always",
        ComposerMode::Off => "composer.mode.off",
    }
}
