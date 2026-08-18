//! Canonical session-title evidence and normalization.
//!
//! A provider title and a first-user excerpt are different pieces of evidence.
//! Keep them separate until the last display boundary so discovery, live
//! carriers, and refreshes cannot accidentally replace one source with a
//! placeholder from another source.

use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionTitleCandidates {
    pub provider_title: Option<String>,
    pub first_user_title: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSessionTitleCandidates {
    provider_title: Option<String>,
    first_user_title: Option<String>,
}

impl<'de> Deserialize<'de> for SessionTitleCandidates {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawSessionTitleCandidates::deserialize(deserializer)?;
        Ok(Self::from_raw(
            raw.provider_title.as_deref(),
            raw.first_user_title.as_deref(),
        ))
    }
}

impl Serialize for SessionTitleCandidates {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Keep the field names and null shape stable, but never persist a
        // placeholder introduced through a direct public struct literal.
        let normalized = Self::from_raw(
            self.provider_title.as_deref(),
            self.first_user_title.as_deref(),
        );
        let mut state = serializer.serialize_struct("SessionTitleCandidates", 2)?;
        state.serialize_field("provider_title", &normalized.provider_title)?;
        state.serialize_field("first_user_title", &normalized.first_user_title)?;
        state.end()
    }
}

impl SessionTitleCandidates {
    fn normalize_in_place(&mut self) -> bool {
        let normalized = Self::from_raw(
            self.provider_title.as_deref(),
            self.first_user_title.as_deref(),
        );
        if *self == normalized {
            return false;
        }
        *self = normalized;
        true
    }

    pub fn from_raw(provider_title: Option<&str>, first_user: Option<&str>) -> Self {
        Self {
            provider_title: provider_title.and_then(normalize_title_candidate),
            first_user_title: first_user.and_then(first_user_title_candidate),
        }
    }

    /// Provider-authored title is a stable identity observation. A later
    /// refresh may add it when the slot is empty, but cannot silently replace
    /// the first canonical provider value with an arrival-order-dependent one.
    pub fn observe_provider(&mut self, value: Option<&str>) -> bool {
        let mut changed = self.normalize_in_place();
        let Some(value) = value.and_then(normalize_title_candidate) else {
            return changed;
        };
        if self.provider_title.is_some() {
            return changed;
        }
        self.provider_title = Some(value);
        changed = true;
        changed
    }

    /// First-user evidence is write-once. Later prompts are not title edits.
    pub fn observe_first_user(&mut self, prompt: &str) -> bool {
        let mut changed = self.normalize_in_place();
        if self.first_user_title.is_some() {
            return changed;
        }
        let Some(value) = first_user_title_candidate(prompt) else {
            return changed;
        };
        self.first_user_title = Some(value);
        changed = true;
        changed
    }

    /// Merge independent history/live observations without importing fallback
    /// text. Each candidate kind is monotonic and write-once: once a meaningful
    /// candidate has settled, a later same-kind observation is evidence-only
    /// and cannot silently rewrite it. Provider evidence is still resolved
    /// before first-user evidence at the display boundary. A conflicting
    /// provider replacement belongs to the explicit typed update path, not to
    /// refresh merge.
    pub fn merge(&mut self, incoming: &Self) -> bool {
        let mut changed = self.normalize_in_place();
        let incoming = Self::from_raw(
            incoming.provider_title.as_deref(),
            incoming.first_user_title.as_deref(),
        );
        changed |= merge_missing_slot(&mut self.provider_title, incoming.provider_title);
        changed |= merge_missing_slot(&mut self.first_user_title, incoming.first_user_title);
        changed
    }

    /// Bridge a legacy resolved `title` field whose original source was not
    /// persisted. If it is equal to the typed first-user evidence, preserve
    /// that evidence rather than inventing a provider title.
    pub fn merge_legacy_title(&mut self, legacy: Option<&str>) -> bool {
        let mut changed = self.normalize_in_place();
        let Some(legacy) = legacy.and_then(normalize_title_candidate) else {
            return changed;
        };
        if self.provider_title.is_some()
            || self.first_user_title.as_deref() == Some(legacy.as_str())
        {
            return changed;
        }
        self.provider_title = Some(legacy);
        changed = true;
        changed
    }

    pub fn resolved(&self) -> Option<&str> {
        resolve_title_candidates(
            self.provider_title.as_deref(),
            self.first_user_title.as_deref(),
        )
    }
}

/// Merge one normalized candidate slot without replacing a settled value.
///
/// This is intentionally not a lexical min/max merge. The first meaningful
/// observation is the stable evidence for the slot; a later conflicting value
/// has no authority to edit it unless an explicit typed update is performed.
fn merge_missing_slot(current: &mut Option<String>, incoming: Option<String>) -> bool {
    let Some(incoming) = incoming else {
        return false;
    };

    match current.as_deref() {
        None => {
            *current = Some(incoming);
            true
        }
        Some(_) => false,
    }
}

/// Borrowing resolver for parser metadata that must not allocate a temporary
/// candidate state merely to choose the display source.
pub fn resolve_title_candidates<'a>(
    provider_title: Option<&'a str>,
    first_user_title: Option<&'a str>,
) -> Option<&'a str> {
    provider_title
        .map(str::trim)
        .filter(|value| !value.is_empty() && !is_absent_session_title(value))
        .or_else(|| {
            first_user_title
                .map(str::trim)
                .filter(|value| !value.is_empty() && !is_absent_session_title(value))
        })
}

/// Normalize provider-authored title candidates. Product/localized catch-all
/// labels are absence, not names.
pub fn normalize_title_candidate(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || is_absent_session_title(trimmed) {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Titles that must not beat a first User message or stick as the sole name.
/// Matching folds all Unicode whitespace, so `Agent 会话` and `Agent会话` are
/// the same product placeholder.
pub fn is_absent_session_title(title: &str) -> bool {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return true;
    }
    let compact: String = trimmed.chars().filter(|ch| !ch.is_whitespace()).collect();
    let lower = compact.to_ascii_lowercase();
    matches!(
        compact.as_str(),
        "Agent会话" | "新会话" | "未命名会话" | "未命名"
    ) || matches!(
        lower.as_str(),
        "agentsession" | "agentty" | "untitled" | "newsession" | "unnamed" | "unnamedsession"
    ) || lower.starts_with("agentty—")
        || lower.starts_with("agentty-")
}

/// First-line excerpt suitable for a live/history display title candidate.
pub fn first_user_title_candidate(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut chars = line.chars();
    let head: String = chars.by_ref().take(80).collect();
    let value = if chars.next().is_some() {
        format!("{}…", head.trim_end())
    } else {
        head
    };
    normalize_title_candidate(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_title_candidates_merge_provider_and_first_user() {
        let mut live = SessionTitleCandidates::from_raw(None, Some("Fix the flaky gate"));
        let history = SessionTitleCandidates::from_raw(Some("Provider title"), None);
        assert!(live.merge(&history));
        assert_eq!(live.resolved(), Some("Provider title"));

        let mut history_first = SessionTitleCandidates::from_raw(Some("Provider title"), None);
        let live_first = SessionTitleCandidates::from_raw(None, Some("Fix the flaky gate"));
        assert!(history_first.merge(&live_first));
        assert_eq!(history_first.resolved(), Some("Provider title"));
    }

    #[test]
    fn absent_title_matches_spacing_variants() {
        assert!(is_absent_session_title("Agent 会话"));
        assert!(is_absent_session_title("Agent会话"));
        assert!(is_absent_session_title("Agent\u{3000}会话"));
        assert!(is_absent_session_title("Agent session"));
        assert_eq!(first_user_title_candidate("Agent会话"), None);
    }

    #[test]
    fn first_user_observation_is_write_once() {
        let mut state = SessionTitleCandidates::default();
        assert!(state.observe_first_user("First request"));
        assert!(!state.observe_first_user("Second request"));
        assert_eq!(state.first_user_title.as_deref(), Some("First request"));
    }

    #[test]
    fn provider_observation_is_first_valid_write_once() {
        let mut state = SessionTitleCandidates::from_raw(Some("Canonical"), None);
        assert!(!state.observe_provider(Some("Later refresh")));
        assert_eq!(state.provider_title.as_deref(), Some("Canonical"));

        // Direct live observations remain first-valid so a later refresh cannot
        // rewrite the provider's initial title in place.
        let mut first = SessionTitleCandidates::default();
        assert!(first.observe_provider(Some("A")));
        assert!(!first.observe_provider(Some("B")));
        assert_eq!(first.provider_title.as_deref(), Some("A"));

        let mut reverse = SessionTitleCandidates::default();
        assert!(reverse.observe_provider(Some("B")));
        assert!(!reverse.observe_provider(Some("A")));
        assert_eq!(reverse.provider_title.as_deref(), Some("B"));
    }

    #[test]
    fn provider_merge_does_not_rewrite_settled_title_without_typed_update() {
        let mut settled = SessionTitleCandidates::from_raw(Some("Original provider title"), None);
        let later_refresh = SessionTitleCandidates::from_raw(Some("A later refresh"), None);

        assert!(!settled.merge(&later_refresh));
        assert_eq!(
            settled.provider_title.as_deref(),
            Some("Original provider title")
        );
    }

    #[test]
    fn merge_keeps_first_observation_for_both_candidate_kinds() {
        let provider_a = SessionTitleCandidates::from_raw(Some("Provider A"), None);
        let provider_b = SessionTitleCandidates::from_raw(Some("Provider B"), None);
        let first_a = SessionTitleCandidates::from_raw(None, Some("Request A"));
        let first_b = SessionTitleCandidates::from_raw(None, Some("Request B"));

        let mut state = provider_a.clone();
        assert!(!state.merge(&provider_b));
        assert!(state.merge(&first_b));
        assert!(!state.merge(&first_a));

        assert_eq!(state.provider_title.as_deref(), Some("Provider A"));
        assert_eq!(state.first_user_title.as_deref(), Some("Request B"));
        assert_eq!(state.resolved(), Some("Provider A"));

        // A second independent reducer may settle on its own first observation,
        // but merging it later cannot rewrite this already-settled state.
        let mut reverse = provider_b;
        reverse.merge(&provider_a);
        reverse.merge(&first_a);
        reverse.merge(&first_b);
        assert_eq!(reverse.provider_title.as_deref(), Some("Provider B"));
        assert_eq!(reverse.first_user_title.as_deref(), Some("Request A"));
    }

    #[test]
    fn deserialization_discards_placeholder_candidates_before_merge() {
        let value = serde_json::json!({
            "provider_title": "Agent 会话",
            "first_user_title": "  "
        });
        let mut state: SessionTitleCandidates = serde_json::from_value(value).unwrap();
        assert!(state.provider_title.is_none());
        assert!(state.first_user_title.is_none());
        assert!(state.merge_legacy_title(Some("Draw a fox")));
        assert_eq!(state.resolved(), Some("Draw a fox"));
    }

    #[test]
    fn direct_placeholder_fields_cannot_block_first_valid_merge() {
        let mut state = SessionTitleCandidates {
            provider_title: Some("Agent 会话".into()),
            first_user_title: Some("  \n".into()),
        };
        assert!(state.merge(&SessionTitleCandidates::from_raw(
            Some("Named rollout"),
            Some("Draw a fox"),
        )));
        assert_eq!(state.provider_title.as_deref(), Some("Named rollout"));
        assert_eq!(state.first_user_title.as_deref(), Some("Draw a fox"));
    }

    #[test]
    fn serialization_normalizes_direct_placeholder_fields() {
        let state = SessionTitleCandidates {
            provider_title: Some("Agent会话".into()),
            first_user_title: Some("  Draw a fox\nsecond line  ".into()),
        };
        let value = serde_json::to_value(state).unwrap();
        assert!(
            value
                .get("provider_title")
                .is_some_and(|value| value.is_null())
        );
        assert_eq!(
            value
                .get("first_user_title")
                .and_then(serde_json::Value::as_str),
            Some("Draw a fox")
        );
    }
}
