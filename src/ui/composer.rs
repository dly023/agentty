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

    pub fn has_override(&self, target: &PaneIdentity) -> bool {
        self.overrides.contains_key(target)
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
    fn composer_mode_cycles_and_auto_includes_plain_shells() {
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
    AppContext as _, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, WindowExt as _};

pub struct ComposerState {
    pub target: PaneIdentity,
    pub input: Entity<InputState>,
    pub completion: Option<crate::ui::completion_surface::ComposerCompletionState>,
    pub completion_generation: u64,
    // The callback belongs to this exact Input instance. Dropping ComposerState
    // cancels it before a replacement Composer can become observable.
    _input_subscription: gpui::Subscription,
}

impl crate::ui::app::AgenttyApp {
    pub(crate) fn focused_terminal_leaf(
        &self,
        window: &Window,
        cx: &gpui::App,
    ) -> Option<Entity<crate::terminal::view::TerminalView>> {
        self.tabs
            .get(self.active)?
            .pane
            .focused_leaf(window, cx)
            .and_then(|slot| slot.terminal().cloned())
    }

    fn composer_input_focused_target(
        &self,
        window: &Window,
        cx: &gpui::App,
    ) -> Option<PaneIdentity> {
        for state in self.composers.values() {
            if state
                .input
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
            {
                return Some(state.target.clone());
            }
        }
        None
    }

    fn composer_target_identity(&self, window: &Window, cx: &gpui::App) -> Option<PaneIdentity> {
        if let Some(target) = self.composer_input_focused_target(window, cx) {
            return Some(target);
        }
        let terminal = self.focused_terminal_leaf(window, cx)?;
        Some(PaneIdentity {
            environment: crate::core::session::WorkspaceStore::environment_id(cx, self.workspace),
            pane_id: terminal.read(cx).pane_id(),
        })
    }

    fn focused_composer_target(&self, window: &Window, cx: &gpui::App) -> Option<PaneIdentity> {
        self.composer_target_identity(window, cx)
    }

    fn has_cli_agent_for(&self, target: &PaneIdentity, cx: &gpui::App) -> bool {
        let Some(terminal) = self.target_terminal(target, cx).ok() else {
            return false;
        };
        let view = terminal.read(cx);
        view.agent().is_some()
            || view.live_binding().agent.is_some()
            || view.agent_session().is_some()
    }

    fn focused_has_cli_agent(&self, window: &Window, cx: &gpui::App) -> bool {
        self.composer_target_identity(window, cx)
            .as_ref()
            .is_some_and(|target| self.has_cli_agent_for(target, cx))
    }

    pub(crate) fn composer_footer_visible(&self, target: &PaneIdentity, cx: &gpui::App) -> bool {
        let mode = cx.global::<crate::core::config::Config>().composer_mode;
        if mode == ComposerMode::Off && !self.composer_visibility.has_override(target) {
            return false;
        }
        let has_agent = self.has_cli_agent_for(target, cx);
        self.composer_visibility.has_override(target) || composer_should_dock(mode, has_agent)
    }

    pub(crate) fn composer_dock_expanded(&self, target: &PaneIdentity, cx: &gpui::App) -> bool {
        let mode = cx.global::<crate::core::config::Config>().composer_mode;
        let has_agent = self.has_cli_agent_for(target, cx);
        self.composer_visibility.resolve(target, mode, has_agent)
            && self.composers.contains_key(target)
    }

    fn close_composer_for(
        &mut self,
        target: &PaneIdentity,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if let Some(state) = self.composers.remove(target) {
            let draft = state.input.read(cx).value().to_string();
            self.composer_drafts.set(state.target, draft);
        }
        if self.composers.is_empty() {
            self.focus_active(window, cx);
        }
    }

    pub(crate) fn composer_expanded_for(&self, target: &PaneIdentity) -> bool {
        self.composers.contains_key(target)
    }

    pub(crate) fn toggle_composer_from_footer(
        &mut self,
        target: &PaneIdentity,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let mode = cx.global::<crate::core::config::Config>().composer_mode;
        let has_agent = self.has_cli_agent_for(target, cx);
        if self.composer_visibility.toggle(target, mode, has_agent) {
            self.ensure_composer_for(target.clone(), window, cx);
            if let Some(state) = self.composers.get(target) {
                let focus = state.input.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
        } else {
            self.close_composer_for(target, window, cx);
        }
        cx.notify();
    }

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

    pub(crate) fn sync_composer_dock(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let Some(tab) = self.tabs.get(self.active) else {
            return false;
        };
        let slots = crate::ui::composer_dock::dock_slots_for_tab(tab, self, window, cx);
        self.sync_composer_docks(&slots, window, cx)
    }

    pub(crate) fn sync_composer_docks(
        &mut self,
        slots: &[PaneIdentity],
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let mode = cx.global::<crate::core::config::Config>().composer_mode;
        let slot_set: std::collections::HashSet<_> = slots.iter().cloned().collect();
        let before = !self.composers.is_empty();
        self.composers.retain(|target, state| {
            if slot_set.contains(target) {
                return true;
            }
            let draft = state.input.read(cx).value().to_string();
            self.composer_drafts.set(target.clone(), draft);
            false
        });
        for target in slots {
            let has_agent = self.has_cli_agent_for(target, cx);
            if self.composer_visibility.resolve(target, mode, has_agent) {
                self.ensure_composer_for(target.clone(), window, cx);
            } else {
                self.close_composer_for(target, window, cx);
            }
        }
        before != !self.composers.is_empty()
    }

    pub(crate) fn ensure_composer_open(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(target) = self.composer_target_identity(window, cx) else {
            return;
        };
        self.ensure_composer_for(target, window, cx);
    }

    pub(crate) fn close_composer(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(target) = self.composer_target_identity(window, cx) else {
            return;
        };
        self.close_composer_for(&target, window, cx);
    }

    fn ensure_composer_for(
        &mut self,
        target: PaneIdentity,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.composers.contains_key(&target) {
            return;
        }
        let initial = self.composer_drafts.get(&target).to_owned();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .placeholder(crate::core::i18n::current(cx, "composer.placeholder"))
                .default_value(initial)
        });
        let target_for_callback = target.clone();
        let input_subscription =
            cx.subscribe_in(&input, window, move |this, source, event, window, cx| {
                let source_is_current = this.composers.values().any(|composer| {
                    composer.input.entity_id() == source.entity_id()
                        && composer.target == target_for_callback
                });
                if !source_is_current {
                    return;
                }
                match event {
                    gpui_component::input::InputEvent::PressEnter { shift: false, .. } => {
                        let completion_has_selection = this
                            .composers
                            .get(&target_for_callback)
                            .and_then(|state| state.completion.as_ref())
                            .and_then(|state| state.session.selected())
                            .is_some();
                        if completion_has_selection {
                            this.composer_completion_accept_for(&target_for_callback, window, cx);
                        } else {
                            this.composer_completion_close_for(&target_for_callback, cx);
                            this.submit_composer_for(&target_for_callback, window, cx);
                        }
                    }
                    gpui_component::input::InputEvent::Change => {
                        this.composer_completion_refilter_for(&target_for_callback, cx);
                    }
                    _ => {}
                }
            });
        self.composers.insert(
            target.clone(),
            ComposerState {
                target,
                input,
                completion: None,
                completion_generation: 0,
                _input_subscription: input_subscription,
            },
        );
        cx.notify();
    }

    pub(crate) fn composer_input_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.composer_input_focused_target(window, cx).is_some()
    }

    pub(crate) fn complete_focused_surface(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let composer_focused = self.composer_input_focused(window, cx);
        let terminal = self.focused_terminal_leaf(window, cx);
        let terminal_active = terminal
            .as_ref()
            .is_some_and(|leaf| leaf.read(cx).input_active_for_completion());
        match crate::ui::completion_surface::completion_focus_owner(
            composer_focused,
            terminal_active,
        ) {
            Some(crate::ui::completion_surface::CompletionFocusOwner::Composer) => {
                if let Some(target) = self.composer_input_focused_target(window, cx) {
                    self.composer_complete_tab_for(&target, forward, window, cx);
                }
            }
            Some(crate::ui::completion_surface::CompletionFocusOwner::Terminal) => {
                if let Some(leaf) = terminal {
                    leaf.update(cx, |view, cx| view.tab_pressed(forward, cx));
                }
            }
            None => {}
        }
    }

    pub(crate) fn composer_complete_tab_for(
        &mut self,
        target: &PaneIdentity,
        forward: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if !cx.global::<crate::core::config::Config>().tab_completion {
            return;
        }
        let Some(composer) = self.composers.get_mut(target) else {
            return;
        };
        if composer.completion.is_some() {
            if let Some(state) = composer.completion.as_mut() {
                state.session.select(forward);
            }
            cx.notify();
            return;
        }
        let input = composer.input.clone();
        let completion_generation = composer.completion_generation;
        drop(composer);
        let (line, cursor_chars) = {
            let state = input.read(cx);
            let line = state.value().to_string();
            let cursor_chars =
                crate::ui::completion_surface::byte_offset_to_char_index(&line, state.cursor());
            (line, cursor_chars)
        };
        let Ok(terminal) = self.target_terminal(target, cx) else {
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
        let authority = if paths_local {
            crate::terminal::completion::CompletionAuthority::Local
        } else {
            crate::terminal::completion::CompletionAuthority::Remote
        };
        let mut gui = crate::terminal::completion::complete_with_authority(
            &line,
            cursor_chars,
            local_cwd.as_deref(),
            authority,
        )
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
            let generation = completion_generation.wrapping_add(1);
            let request = agentty_core::agent_runtime::CompletionRequest {
                operation: agentty_core::agent_runtime::OperationId(generation),
                generation: agentty_core::agent_runtime::CompletionGeneration(generation),
                authority,
                cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
                input: line.to_string(),
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
        let Some(composer) = self.composers.get_mut(target) else {
            return;
        };
        composer.completion_generation = composer.completion_generation.wrapping_add(1);
        let generation = composer.completion_generation;
        composer.completion = Some(crate::ui::completion_surface::ComposerCompletionState::new(
            generation,
            crate::terminal::completion::CompletionSession::new(word_start, word, gui.candidates),
        ));
        let _ = window;
        cx.notify();
    }

    fn composer_completion_refilter_for(
        &mut self,
        target: &PaneIdentity,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(composer) = self.composers.get_mut(target) else {
            return;
        };
        let line = composer.input.read(cx).value().to_string();
        let cursor_chars = crate::ui::completion_surface::byte_offset_to_char_index(
            &line,
            composer.input.read(cx).cursor(),
        );
        let Some(state) = composer.completion.as_mut() else {
            return;
        };
        let word: String = line
            .chars()
            .skip(state.session.word_start)
            .take(cursor_chars.saturating_sub(state.session.word_start))
            .collect();
        if !state.session.refilter(&word) {
            composer.completion = None;
        }
        cx.notify();
    }

    fn composer_completion_accept_for(
        &mut self,
        target: &PaneIdentity,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(composer) = self.composers.get(target) else {
            return;
        };
        let input = composer.input.clone();
        let draft = input.read(cx).value().to_string();
        let Some(state) = self
            .composers
            .get(target)
            .and_then(|composer| composer.completion.as_ref())
        else {
            return;
        };
        let Some(accepted) =
            crate::ui::completion_surface::accept_into_draft(&draft, &state.session)
        else {
            return;
        };
        let position = crate::ui::completion_surface::char_index_to_position(
            &accepted.text,
            accepted.cursor_chars,
        );
        input.update(cx, |state, cx| {
            state.set_value(&accepted.text, window, cx);
            state.set_cursor_position(position, window, cx);
        });
        if let Some(composer) = self.composers.get_mut(target) {
            composer.completion = None;
        }
        cx.notify();
    }

    fn composer_completion_close_for(
        &mut self,
        target: &PaneIdentity,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(composer) = self.composers.get_mut(target) else {
            return;
        };
        if crate::ui::completion_surface::clear_composer_completion(&mut composer.completion) {
            cx.notify();
        }
    }

    fn render_composer_completion_menu_for(
        &self,
        target: &PaneIdentity,
        cx: &gpui::App,
    ) -> Option<impl IntoElement + use<>> {
        let state = self.composers.get(target)?.completion.as_ref()?;
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
        let theme = cx.theme();
        let rows: Vec<_> = (0..items.len().min(MAX_ROWS))
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
                .debug_selector(|| "COMPOSER_COMPLETION_MENU".into())
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

    pub(crate) fn target_terminal(
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

    fn submit_composer_for(
        &mut self,
        target: &PaneIdentity,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(state) = self.composers.get(target) else {
            return;
        };
        let draft = state.input.read(cx).value().to_string();
        if draft.trim().is_empty() {
            return;
        }
        self.composer_drafts.set(target.clone(), draft.clone());
        let outcome = match self.target_terminal(target, cx) {
            Ok(terminal) => self.deliver_agent_prompt_to(terminal, &draft, window, cx),
            Err(outcome) => outcome,
        };
        match outcome {
            DeliveryOutcome::Delivered => {
                self.composer_drafts.clear(target);
                if let Some(state) = self.composers.get(target) {
                    state.input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                }
                self.save_session(cx);
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

    pub(crate) fn render_composer_for(
        &self,
        target: &PaneIdentity,
        cx: &mut gpui::Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let state = self.composers.get(target)?;
        let menu = self.render_composer_completion_menu_for(target, cx);
        Some(
            div()
                .debug_selector(|| "COMPOSER_RICH_INPUT_DOCK".into())
                .on_action(
                    cx.listener(|_, action: &gpui_component::input::Enter, _, cx| {
                        if !action.shift {
                            cx.stop_propagation();
                        }
                    }),
                )
                .flex_shrink_0()
                .gap_1()
                .px_2()
                .py_1()
                .bg(cx.theme().background)
                .border_t_1()
                .border_color(cx.theme().border)
                .children(menu)
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

#[cfg(all(test, unix))]
mod gpui_tests {
    use super::*;
    use crate::daemon::protocol::{ClientMsg, DaemonMsg};
    use crate::terminal::completion::{Candidate, CandidateKind, CompletionSession};
    use crate::terminal::view::quiet_test_pane;
    use crate::ui::app::{AgenttyApp, test_window::harness_with_pane};
    use crate::ui::pane::{Pane, PaneSlot};
    use gpui::{Axis, Entity, MouseButton, TestAppContext, VisualTestContext};
    use gpui_component::input::InputEvent;
    use std::io::ErrorKind;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    fn set_composer_mode(cx: &mut VisualTestContext, mode: ComposerMode) {
        cx.update(|_, cx| {
            let mut cfg = cx.global::<crate::core::config::Config>().clone();
            cfg.composer_mode = mode;
            cx.set_global(cfg);
        });
    }

    fn composer_input(app: &AgenttyApp) -> Entity<InputState> {
        app.composers
            .values()
            .next()
            .expect("expected a composer instance")
            .input
            .clone()
    }

    fn open_and_focus_composer(
        app: &Entity<AgenttyApp>,
        cx: &mut VisualTestContext,
    ) -> Entity<InputState> {
        let input = app.update_in(cx, |app, window, cx| {
            app.ensure_composer_open(window, cx);
            let input = composer_input(app);
            input.update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
            input
        });
        cx.run_until_parked();
        input
    }

    fn set_draft(
        app: &Entity<AgenttyApp>,
        input: &Entity<InputState>,
        value: &str,
        cx: &mut VisualTestContext,
    ) {
        let position =
            crate::ui::completion_surface::char_index_to_position(value, value.chars().count());
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.set_value(value, window, cx);
                input.set_cursor_position(position, window, cx);
            });
        });
        app.update_in(cx, |_, _, cx| cx.notify());
        cx.run_until_parked();
    }

    fn input_value(input: &Entity<InputState>, cx: &mut VisualTestContext) -> String {
        cx.update(|_, cx| input.read(cx).value().to_string())
    }

    fn click_debug_selector(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("production selector {selector} must be rendered"));
        let point = gpui::point(
            bounds.origin.x + bounds.size.width / 2.,
            bounds.origin.y + bounds.size.height / 2.,
        );
        cx.simulate_mouse_move(point, None, gpui::Modifiers::none());
        cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();
    }

    fn report_agent_session(
        app: &Entity<AgenttyApp>,
        stream: &mut UnixStream,
        cx: &mut VisualTestContext,
    ) {
        DaemonMsg::AgentStatus(Some(crate::core::cli_agent::AgentSessionState {
            status: crate::core::cli_agent::AgentStatus::Idle,
            session_id: Some("composer-activity-e2e".into()),
            launch_argv: Some(vec!["codex".into()]),
            rich: true,
            ..Default::default()
        }))
        .encode(stream)
        .expect("test pane accepts an AgentStatus frame");
        for _ in 0..200 {
            cx.run_until_parked();
            if app.update_in(cx, |app, window, cx| {
                app.focused_leaf(window, cx)
                    .is_some_and(|leaf| leaf.read(cx).agent_session().is_some())
            }) {
                app.update_in(cx, |_, _, cx| cx.notify());
                cx.run_until_parked();
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("AgentStatus did not reach the production terminal view");
    }

    fn next_input_until_timeout(stream: &mut UnixStream) -> Option<Vec<u8>> {
        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .expect("test socket accepts a timeout");
        loop {
            match ClientMsg::read(stream) {
                Ok(ClientMsg::Input(bytes)) => return Some(bytes),
                Ok(_) => continue,
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    return None;
                }
                Err(error) => panic!("composer test socket failed before Input: {error}"),
            }
        }
    }

    fn candidate(text: &str, start: usize, end: usize) -> Candidate {
        Candidate {
            text: text.into(),
            kind: CandidateKind::Command,
            start,
            end,
            description: None,
            icon: None,
        }
    }

    #[gpui::test]
    fn composer_shift_enter_inserts_newline_without_delivery(cx: &mut TestAppContext) {
        let (app, mut cx, mut pane_stream) = harness_with_pane(cx);
        let input = open_and_focus_composer(&app, &mut cx);
        set_draft(&app, &input, "first line", &mut cx);

        cx.simulate_keystrokes("shift-enter");

        assert_eq!(input_value(&input, &mut cx), "first line\n");
        assert_eq!(
            next_input_until_timeout(&mut pane_stream),
            None,
            "Shift+Enter edits the draft and must not reach the PTY"
        );
    }

    #[gpui::test]
    fn composer_enter_accepts_open_completion_without_delivery(cx: &mut TestAppContext) {
        let (app, mut cx, mut pane_stream) = harness_with_pane(cx);
        let input = open_and_focus_composer(&app, &mut cx);
        set_draft(&app, &input, "git ch", &mut cx);
        app.update_in(&mut cx, |app, window, cx| {
            let target = app
                .composer_target_identity(window, cx)
                .expect("composer owns a stable target");
            if let Some(state) = app.composers.get_mut(&target) {
                state.completion =
                    Some(crate::ui::completion_surface::ComposerCompletionState::new(
                        1,
                        CompletionSession::new(4, "ch".into(), vec![candidate("checkout", 4, 6)]),
                    ));
            }
            cx.notify();
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("COMPOSER_COMPLETION_MENU").is_some(),
            "the completion state is painted as an open menu before Enter"
        );

        cx.simulate_keystrokes("enter");

        assert_eq!(input_value(&input, &mut cx), "git checkout");
        app.update_in(&mut cx, |app, window, cx| {
            let target = app
                .composer_target_identity(window, cx)
                .expect("composer target");
            assert!(
                app.composers
                    .get(&target)
                    .and_then(|state| state.completion.as_ref())
                    .is_none(),
                "accept closes the completion menu"
            );
        });
        assert_eq!(
            next_input_until_timeout(&mut pane_stream),
            None,
            "accept mutates only the draft and must not reach the PTY"
        );
    }

    #[gpui::test]
    fn composer_enter_without_completion_submits_once(cx: &mut TestAppContext) {
        let (app, mut cx, mut pane_stream) = harness_with_pane(cx);
        let input = open_and_focus_composer(&app, &mut cx);
        set_draft(&app, &input, "explain this", &mut cx);

        cx.simulate_keystrokes("enter");

        assert_eq!(
            next_input_until_timeout(&mut pane_stream),
            Some(crate::core::agent_prompt::submit_bytes("explain this")),
            "plain Enter follows the canonical AgentPrompt delivery path"
        );
        assert_eq!(
            next_input_until_timeout(&mut pane_stream),
            None,
            "one Enter must produce exactly one delivery"
        );
        assert_eq!(input_value(&input, &mut cx), "");
        app.update_in(&mut cx, |app, window, cx| {
            let terminal = app
                .focused_leaf(window, cx)
                .expect("the Composer keeps its terminal target");
            assert_eq!(
                terminal.read(cx).live_first_user_title(),
                Some("explain this"),
                "Composer delivery must stamp the canonical live binding, not only write to the PTY"
            );
        });
    }

    fn composer_height_for(
        app: &Entity<AgenttyApp>,
        input: &Entity<InputState>,
        rows: usize,
        cx: &mut VisualTestContext,
    ) -> gpui::Pixels {
        let value = (1..=rows)
            .map(|row| format!("row {row}"))
            .collect::<Vec<_>>()
            .join("\n");
        set_draft(app, input, &value, cx);
        cx.debug_bounds("COMPOSER_RICH_INPUT_DOCK")
            .expect("the rendered composer publishes test bounds")
            .size
            .height
    }

    #[gpui::test]
    fn composer_auto_grow_clamps_between_two_and_eight_rows(cx: &mut TestAppContext) {
        let (app, mut cx, _pane_stream) = harness_with_pane(cx);
        let input = open_and_focus_composer(&app, &mut cx);

        let one = composer_height_for(&app, &input, 1, &mut cx);
        let two = composer_height_for(&app, &input, 2, &mut cx);
        let four = composer_height_for(&app, &input, 4, &mut cx);
        let eight = composer_height_for(&app, &input, 8, &mut cx);
        let nine = composer_height_for(&app, &input, 9, &mut cx);

        assert_eq!(one, two, "one content row still reserves the 2-row minimum");
        assert!(four > two, "the dock grows above its 2-row minimum");
        assert!(eight > four, "the dock continues growing through row 8");
        assert_eq!(nine, eight, "row 9 is scrolled inside the 8-row maximum");
    }

    #[gpui::test]
    fn composer_footer_toggle_round_trips_draft_and_focus(cx: &mut TestAppContext) {
        let (app, mut cx, mut pane_stream) = harness_with_pane(cx);
        report_agent_session(&app, &mut pane_stream, &mut cx);
        let input = open_and_focus_composer(&app, &mut cx);
        set_draft(&app, &input, "draft survives collapse", &mut cx);
        let target = app.update_in(&mut cx, |app, window, cx| {
            let composer = app
                .composers
                .get(
                    &app.composer_target_identity(window, cx)
                        .expect("open Composer owns a stable target"),
                )
                .expect("composer state exists");
            assert!(
                composer
                    .input
                    .read(cx)
                    .focus_handle(cx)
                    .contains_focused(window, cx),
                "the expanded Composer Input owns focus before the first click"
            );
            composer.target.clone()
        });
        assert!(
            cx.debug_bounds("COMPOSER_RICH_INPUT_DOCK").is_some(),
            "expanded state paints the production Composer dock"
        );

        click_debug_selector("COMPOSER_CONTEXT_FOOTER_TOGGLE", &mut cx);

        assert!(
            cx.debug_bounds("COMPOSER_RICH_INPUT_DOCK").is_none(),
            "collapse removes the production Composer dock"
        );
        assert!(
            cx.debug_bounds("COMPOSER_CONTEXT_FOOTER_TOGGLE").is_some(),
            "the context footer remains visible while Composer is collapsed"
        );

        app.update_in(&mut cx, |app, window, cx| {
            assert!(
                !app.composers.contains_key(&target),
                "first click collapses the Composer"
            );
            assert_eq!(
                app.composer_drafts.get(&target),
                "draft survives collapse",
                "collapse snapshots the exact draft before dropping ComposerState"
            );
            assert_eq!(
                cx.global::<crate::core::config::Config>().composer_mode,
                ComposerMode::Auto,
                "direct manipulation must not mutate the persisted mode"
            );
            let terminal = app.focused_leaf(window, cx).expect("focused test terminal");
            assert!(
                terminal
                    .read(cx)
                    .focus_handle(cx)
                    .contains_focused(window, cx),
                "collapse returns focus to the same terminal"
            );
        });

        click_debug_selector("COMPOSER_CONTEXT_FOOTER_TOGGLE", &mut cx);

        assert!(
            cx.debug_bounds("COMPOSER_RICH_INPUT_DOCK").is_some(),
            "expand restores the production Composer dock"
        );
        assert!(
            cx.debug_bounds("COMPOSER_CONTEXT_FOOTER_TOGGLE").is_some(),
            "the context footer remains visible after expand"
        );

        app.update_in(&mut cx, |app, window, cx| {
            let restored = app
                .composers
                .get(&target)
                .expect("second click restores the Composer");
            assert_eq!(
                restored.input.read(cx).value().to_string(),
                "draft survives collapse",
                "expand restores the exact pane-isolated draft"
            );
            assert!(
                restored
                    .input
                    .read(cx)
                    .focus_handle(cx)
                    .contains_focused(window, cx),
                "expand focuses the restored Composer Input"
            );
            let terminal = app
                .focused_leaf(window, cx)
                .expect("same test terminal remains active");
            assert!(
                !terminal
                    .read(cx)
                    .focus_handle(cx)
                    .contains_focused(window, cx),
                "Composer focus is exclusive after expand"
            );
            assert_eq!(
                cx.global::<crate::core::config::Config>().composer_mode,
                ComposerMode::Auto,
                "round trip still leaves the persisted mode unchanged"
            );
        });
    }

    #[gpui::test]
    fn composer_input_focus_does_not_retarget_to_first_leaf(cx: &mut TestAppContext) {
        let (app, mut cx, _left_stream) = harness_with_pane(cx);
        set_composer_mode(&mut cx, ComposerMode::Always);
        let right_pane_id = app.update_in(&mut cx, |app, window, cx| {
            let (view2, _stream2) = quiet_test_pane(2, window, cx);
            let right_id = view2.read(cx).pane_id();
            let existing = std::mem::replace(&mut app.tabs[0].pane, Pane::Empty);
            app.tabs[0].pane = Pane::split_node(
                Axis::Horizontal,
                0.5,
                existing,
                Pane::leaf(PaneSlot::Ready(view2.clone())),
            );
            view2.update(cx, |view, cx| view.focus_handle.focus(window, cx));
            app.sync_composer_dock(window, cx);
            right_id
        });
        cx.run_until_parked();

        let _input = open_and_focus_composer(&app, &mut cx);
        app.update_in(&mut cx, |app, window, cx| {
            assert!(
                app.composer_input_focused(window, cx),
                "composer input keeps focus without retargeting"
            );
            let target = app
                .composers
                .keys()
                .find(|target| target.pane_id == right_pane_id)
                .cloned()
                .expect("composer stays bound to the focused column");
            assert_eq!(
                target.pane_id, right_pane_id,
                "composer focus must not retarget to the DFS-first leaf"
            );
        });
    }

    #[gpui::test]
    fn vertical_split_composer_submits_to_focused_pane(cx: &mut TestAppContext) {
        let (app, mut cx, mut top_stream) = harness_with_pane(cx);
        set_composer_mode(&mut cx, ComposerMode::Always);
        let mut bottom_stream = app.update_in(&mut cx, |app, window, cx| {
            let (view2, stream2) = quiet_test_pane(2, window, cx);
            let existing = std::mem::replace(&mut app.tabs[0].pane, Pane::Empty);
            app.tabs[0].pane = Pane::split_node(
                Axis::Vertical,
                0.5,
                existing,
                Pane::leaf(PaneSlot::Ready(view2.clone())),
            );
            view2.update(cx, |view, cx| view.focus_handle.focus(window, cx));
            app.sync_composer_dock(window, cx);
            stream2
        });
        cx.run_until_parked();

        let input = open_and_focus_composer(&app, &mut cx);
        set_draft(&app, &input, "bottom pane prompt", &mut cx);
        cx.simulate_keystrokes("enter");

        assert_eq!(
            next_input_until_timeout(&mut top_stream),
            None,
            "vertical split delivery must not reach the unfocused pane"
        );
        assert_eq!(
            next_input_until_timeout(&mut bottom_stream),
            Some(crate::core::agent_prompt::submit_bytes(
                "bottom pane prompt"
            )),
            "vertical split submits to the focused terminal leaf"
        );
    }

    #[gpui::test]
    fn horizontal_split_composer_submits_to_own_column(cx: &mut TestAppContext) {
        let (app, mut cx, mut left_stream) = harness_with_pane(cx);
        set_composer_mode(&mut cx, ComposerMode::Always);
        let (mut right_stream, right_target) = app.update_in(&mut cx, |app, window, cx| {
            let (view2, stream2) = quiet_test_pane(2, window, cx);
            let target = PaneIdentity {
                environment: crate::core::session::WorkspaceStore::environment_id(
                    cx,
                    app.workspace,
                ),
                pane_id: view2.read(cx).pane_id(),
            };
            let existing = std::mem::replace(&mut app.tabs[0].pane, Pane::Empty);
            app.tabs[0].pane = Pane::split_node(
                Axis::Horizontal,
                0.5,
                existing,
                Pane::leaf(PaneSlot::Ready(view2.clone())),
            );
            view2.update(cx, |view, cx| view.focus_handle.focus(window, cx));
            app.sync_composer_dock(window, cx);
            (stream2, target)
        });
        cx.run_until_parked();

        let right_input = app.update_in(&mut cx, |app, window, cx| {
            let input = app
                .composers
                .get(&right_target)
                .expect("right column owns a composer after dock sync")
                .input
                .clone();
            input.update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
            input
        });
        cx.run_until_parked();
        set_draft(&app, &right_input, "right column prompt", &mut cx);
        cx.simulate_keystrokes("enter");

        assert_eq!(
            next_input_until_timeout(&mut left_stream),
            None,
            "right-column submit must not reach the left pane"
        );
        assert_eq!(
            next_input_until_timeout(&mut right_stream),
            Some(crate::core::agent_prompt::submit_bytes(
                "right column prompt"
            )),
            "horizontal split submits through the column-local composer"
        );
    }

    #[gpui::test]
    fn stale_composer_input_callback_cannot_submit_replacement(cx: &mut TestAppContext) {
        let (app, mut cx, mut pane_stream) = harness_with_pane(cx);
        let stale_input = open_and_focus_composer(&app, &mut cx);

        let replacement_input = app.update_in(&mut cx, |app, window, cx| {
            app.close_composer(window, cx);
            app.ensure_composer_open(window, cx);
            let input = composer_input(app);
            let position = crate::ui::completion_surface::char_index_to_position(
                "replacement draft",
                "replacement draft".chars().count(),
            );
            input.update(cx, |input, cx| {
                input.set_value("replacement draft", window, cx);
                input.set_cursor_position(position, window, cx);
            });
            input
        });

        cx.update(|_, cx| {
            stale_input.update(cx, |_, cx| {
                cx.emit(InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                });
            });
        });
        cx.run_until_parked();

        assert_eq!(
            input_value(&replacement_input, &mut cx),
            "replacement draft",
            "a torn-down Input cannot mutate the replacement Composer"
        );
        assert_eq!(
            next_input_until_timeout(&mut pane_stream),
            None,
            "a torn-down Input cannot submit the replacement draft"
        );

        app.update_in(&mut cx, |_, window, cx| {
            replacement_input.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.simulate_keystrokes("enter");
        assert_eq!(
            next_input_until_timeout(&mut pane_stream),
            Some(crate::core::agent_prompt::submit_bytes("replacement draft")),
            "the replacement Composer owns one live callback after the stale one is cancelled"
        );
    }
}
