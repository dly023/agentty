//! Shared completion surface policy (INPUT-COMPLETION-EDITOR-SURFACE-06).
//!
//! Tab/menu/accept attach to the focused editable surface — docked composer
//! Input when it holds focus, otherwise the terminal local-edit line. Accept
//! mutates only that buffer and never auto-submits or writes the PTY.

use std::collections::BTreeSet;

use crate::terminal::completion::{Candidate, CompletionSession, Replacement};

/// Which editable surface currently owns Tab completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionFocusOwner {
    Composer,
    Terminal,
}

/// Exclusive focus gate: Tab must not dual-write composer and terminal.
pub(crate) fn completion_focus_owner(
    composer_focused: bool,
    terminal_input_active: bool,
) -> Option<CompletionFocusOwner> {
    match (composer_focused, terminal_input_active) {
        (true, _) => Some(CompletionFocusOwner::Composer),
        (false, true) => Some(CompletionFocusOwner::Terminal),
        (false, false) => None,
    }
}

/// Apply a candidate replacement into a text buffer (char offsets).
/// Returns the new text and the new cursor as a char index.
pub(crate) fn apply_replacement(
    orig: &str,
    start: usize,
    end: usize,
    text: &str,
) -> (String, usize) {
    Replacement {
        orig: orig.to_owned(),
        start,
        end,
        text: text.to_owned(),
    }
    .apply()
}

/// Merge enrichment candidates into an existing list, deduping by `text`.
pub(crate) fn merge_candidates_by_text(
    primary: Vec<Candidate>,
    extra: impl IntoIterator<Item = Candidate>,
) -> Vec<Candidate> {
    let mut seen: BTreeSet<String> = primary.iter().map(|c| c.text.clone()).collect();
    let mut out = primary;
    for candidate in extra {
        if seen.insert(candidate.text.clone()) {
            out.push(candidate);
        }
    }
    out
}

/// Outcome of accepting a completion into a draft buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftAccept {
    pub text: String,
    pub cursor_chars: usize,
    /// Always false — accept must never submit the composer.
    pub submit: bool,
}

/// Accept the selected candidate into a draft. Never submits.
pub(crate) fn accept_into_draft(draft: &str, session: &CompletionSession) -> Option<DraftAccept> {
    let candidate = session.selected()?;
    let (text, cursor_chars) =
        apply_replacement(draft, candidate.start, candidate.end, &candidate.text);
    Some(DraftAccept {
        text,
        cursor_chars,
        submit: false,
    })
}

/// Convert a UTF-8 byte offset into a char index for completion engines that
/// use character offsets (terminal cmd / signature completer).
pub(crate) fn byte_offset_to_char_index(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    text[..byte].chars().count()
}

/// Convert a char index into an editor Position for InputState::set_cursor_position.
pub(crate) fn char_index_to_position(
    text: &str,
    char_idx: usize,
) -> gpui_component::input::Position {
    let mut line = 0u32;
    let mut character = 0u32;
    for (i, ch) in text.chars().enumerate() {
        if i >= char_idx {
            break;
        }
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16() as u32;
        }
    }
    gpui_component::input::Position { line, character }
}

/// Composer-owned completion session with a generation token for stale async.
pub(crate) struct ComposerCompletionState {
    pub session: CompletionSession,
    pub generation: u64,
}

impl ComposerCompletionState {
    pub fn new(generation: u64, session: CompletionSession) -> Self {
        Self {
            session,
            generation,
        }
    }

    pub fn accepts_generation(&self, generation: u64) -> bool {
        self.generation == generation
    }
}

/// Close helper used when focus leaves the composer or the dock hides.
#[allow(dead_code)]
pub(crate) fn clear_composer_completion(state: &mut Option<ComposerCompletionState>) -> bool {
    state.take().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::completion::CandidateKind;

    fn cand(text: &str, start: usize, end: usize) -> Candidate {
        Candidate {
            text: text.into(),
            kind: CandidateKind::Command,
            start,
            end,
            description: None,
            icon: None,
        }
    }

    #[test]
    fn apply_replacement_mutates_only_target_range() {
        let (text, cursor) = apply_replacement("git ch", 4, 6, "checkout");
        assert_eq!(text, "git checkout");
        assert_eq!(cursor, 12);
        assert!(!text.contains("chch"));
    }

    #[test]
    fn composer_completion_accepts_into_draft_not_submit() {
        let session = CompletionSession::new(
            4,
            "ch".into(),
            vec![cand("checkout", 4, 6), cand("cherry-pick", 4, 6)],
        );
        let accepted = accept_into_draft("git ch", &session).expect("selected");
        assert_eq!(accepted.text, "git checkout");
        assert!(!accepted.submit);
    }

    #[test]
    fn completion_focus_owner_is_composer_or_terminal_not_both() {
        assert_eq!(
            completion_focus_owner(true, true),
            Some(CompletionFocusOwner::Composer)
        );
        assert_eq!(
            completion_focus_owner(true, false),
            Some(CompletionFocusOwner::Composer)
        );
        assert_eq!(
            completion_focus_owner(false, true),
            Some(CompletionFocusOwner::Terminal)
        );
        assert_eq!(completion_focus_owner(false, false), None);
        // Composer wins when both claim activity — never dual ownership.
        assert_ne!(
            completion_focus_owner(true, true),
            Some(CompletionFocusOwner::Terminal)
        );
    }

    #[test]
    fn merge_candidates_dedupes_by_text() {
        let merged = merge_candidates_by_text(
            vec![cand("checkout", 0, 0)],
            [cand("checkout", 0, 0), cand("status", 0, 0)],
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "checkout");
        assert_eq!(merged[1].text, "status");
    }

    #[test]
    fn stale_composer_generation_is_rejected() {
        let state =
            ComposerCompletionState::new(3, CompletionSession::new(0, String::new(), Vec::new()));
        assert!(state.accepts_generation(3));
        assert!(!state.accepts_generation(2));
    }
}
