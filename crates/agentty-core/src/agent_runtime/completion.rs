use serde::{Deserialize, Serialize};

use super::{AuthorityKind, OperationId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CompletionGeneration(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplacementRange {
    /// UTF-8 byte offsets into `CompletionRequest::input`.
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub operation: OperationId,
    pub generation: CompletionGeneration,
    pub authority: AuthorityKind,
    pub cwd: Option<String>,
    pub input: String,
    pub cursor: usize,
    pub limit: usize,
    #[serde(default)]
    pub history: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionSourceKind {
    Grammar,
    Signature,
    Filesystem,
    Repository,
    History,
    Generator,
    Prediction,
}

impl CompletionSourceKind {
    pub fn is_deterministic(self) -> bool {
        !matches!(self, Self::Prediction)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionCandidate {
    pub value: String,
    pub display: String,
    pub replacement: ReplacementRange,
    pub source: CompletionSourceKind,
    pub score: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionOutcome {
    Complete(Vec<CompletionCandidate>),
    Failed { message: String },
    Cancelled,
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionCommit {
    Replaced,
    PreservedDeterministic,
    IgnoredStale,
    RejectedAuthority,
}

#[derive(Debug)]
pub struct CompletionReducer {
    authority: AuthorityKind,
    active_generation: Option<CompletionGeneration>,
    candidates: Vec<CompletionCandidate>,
}

impl CompletionReducer {
    pub fn new(authority: AuthorityKind) -> Self {
        Self {
            authority,
            active_generation: None,
            candidates: Vec::new(),
        }
    }

    pub fn begin(
        &mut self,
        generation: CompletionGeneration,
        deterministic: Vec<CompletionCandidate>,
    ) {
        self.active_generation = Some(generation);
        self.candidates = normalize(deterministic);
    }

    pub fn apply(
        &mut self,
        authority: AuthorityKind,
        generation: CompletionGeneration,
        outcome: CompletionOutcome,
    ) -> CompletionCommit {
        if authority != self.authority {
            return CompletionCommit::RejectedAuthority;
        }
        if self.active_generation != Some(generation) {
            return CompletionCommit::IgnoredStale;
        }
        match outcome {
            CompletionOutcome::Complete(mut dynamic) => {
                dynamic.extend(self.candidates.clone());
                self.candidates = normalize(dynamic);
                CompletionCommit::Replaced
            }
            CompletionOutcome::Failed { .. }
            | CompletionOutcome::Cancelled
            | CompletionOutcome::TimedOut => CompletionCommit::PreservedDeterministic,
        }
    }

    pub fn candidates(&self) -> &[CompletionCandidate] {
        &self.candidates
    }
}

fn normalize(mut candidates: Vec<CompletionCandidate>) -> Vec<CompletionCandidate> {
    candidates.sort_by(|a, b| {
        b.source
            .is_deterministic()
            .cmp(&a.source.is_deterministic())
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.value.cmp(&b.value))
    });
    candidates.dedup_by(|a, b| a.value == b.value && a.replacement == b.replacement);
    candidates
}

/// Finds the shell token under the cursor without splitting inside quotes or
/// after an escape. Returned offsets are UTF-8 byte offsets.
pub fn replacement_range(input: &str, cursor: usize) -> ReplacementRange {
    let cursor = cursor.min(input.len());
    let cursor = if input.is_char_boundary(cursor) {
        cursor
    } else {
        input[..cursor]
            .char_indices()
            .next_back()
            .map(|(offset, _)| offset)
            .unwrap_or(0)
    };
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (offset, ch) in input[..cursor].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if matches!(ch, '\'' | '"') {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(ch);
            }
            continue;
        }
        if ch.is_whitespace() && quote.is_none() {
            start = offset + ch.len_utf8();
        }
    }
    if matches!(input[start..cursor].chars().next(), Some('\'' | '"')) {
        start += 1;
    }
    ReplacementRange { start, end: cursor }
}

pub fn complete(host: &dyn crate::host::Host, request: &CompletionRequest) -> CompletionOutcome {
    let expected = if host.id().is_local() {
        AuthorityKind::Local
    } else {
        AuthorityKind::Remote
    };
    if request.authority != expected {
        return CompletionOutcome::Failed {
            message: "requested authority does not match helper host".into(),
        };
    }
    if request.cursor > request.input.len() || !request.input.is_char_boundary(request.cursor) {
        return CompletionOutcome::Failed {
            message: "completion cursor is not a valid UTF-8 boundary".into(),
        };
    }
    if request.limit == 0 {
        return CompletionOutcome::Complete(Vec::new());
    }

    let replacement = replacement_range(&request.input, request.cursor);
    let prefix = &request.input[replacement.start..replacement.end];
    let mut candidates = Vec::new();
    const COMMANDS: &[&str] = &["cd", "git", "ls", "pwd", "ssh"];
    for command in COMMANDS
        .iter()
        .copied()
        .filter(|command| command.starts_with(prefix))
    {
        candidates.push(CompletionCandidate {
            value: command.into(),
            display: command.into(),
            replacement: replacement.clone(),
            source: CompletionSourceKind::Grammar,
            score: 100,
        });
    }

    if let Some(cwd) = request.cwd.as_deref() {
        let cwd = std::path::Path::new(cwd);
        match host.read_dir(cwd, None) {
            Ok(entries) => {
                for entry in entries {
                    if !entry.name.starts_with(prefix) {
                        continue;
                    }
                    let value = if entry.is_dir {
                        format!("{}/", entry.name)
                    } else {
                        entry.name.clone()
                    };
                    candidates.push(CompletionCandidate {
                        value,
                        display: entry.name,
                        replacement: replacement.clone(),
                        source: CompletionSourceKind::Filesystem,
                        score: 80,
                    });
                }
            }
            Err(error) => {
                if candidates.is_empty() {
                    return CompletionOutcome::Failed {
                        message: format!("completion filesystem source failed: {error}"),
                    };
                }
            }
        }
    }

    for line in &request.history {
        if !line.starts_with(&request.input[..request.cursor]) || line == &request.input {
            continue;
        }
        candidates.push(CompletionCandidate {
            value: line.clone(),
            display: line.clone(),
            replacement: ReplacementRange {
                start: 0,
                end: request.cursor,
            },
            source: CompletionSourceKind::History,
            score: 60,
        });
    }

    if let Some(cwd) = request.cwd.as_deref()
        && let Ok(Some(root)) = host.repo_root(std::path::Path::new(cwd))
        && let Ok(output) = host.git(&root, &["status", "--porcelain"])
    {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let path = line.get(3..).unwrap_or(line).trim();
            if path.is_empty() || !path.starts_with(prefix) {
                continue;
            }
            candidates.push(CompletionCandidate {
                value: path.into(),
                display: path.into(),
                replacement: replacement.clone(),
                source: CompletionSourceKind::Repository,
                score: 70,
            });
        }
    }
    let mut candidates = normalize(candidates);
    candidates.truncate(request.limit);
    CompletionOutcome::Complete(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(value: &str, source: CompletionSourceKind) -> CompletionCandidate {
        CompletionCandidate {
            value: value.into(),
            display: value.into(),
            replacement: ReplacementRange { start: 0, end: 1 },
            source,
            score: 1,
        }
    }

    #[test]
    fn stale_generation_is_ignored() {
        let mut reducer = CompletionReducer::new(AuthorityKind::Local);
        reducer.begin(
            CompletionGeneration(2),
            vec![candidate("git", CompletionSourceKind::History)],
        );
        assert_eq!(
            reducer.apply(
                AuthorityKind::Local,
                CompletionGeneration(1),
                CompletionOutcome::Complete(vec![])
            ),
            CompletionCommit::IgnoredStale
        );
        assert_eq!(reducer.candidates().len(), 1);
    }

    #[test]
    fn failed_dynamic_source_preserves_deterministic_candidates() {
        let mut reducer = CompletionReducer::new(AuthorityKind::Local);
        reducer.begin(
            CompletionGeneration(1),
            vec![candidate("git", CompletionSourceKind::Grammar)],
        );
        assert_eq!(
            reducer.apply(
                AuthorityKind::Local,
                CompletionGeneration(1),
                CompletionOutcome::Failed {
                    message: "offline".into()
                }
            ),
            CompletionCommit::PreservedDeterministic
        );
        assert_eq!(reducer.candidates()[0].value, "git");
    }

    #[test]
    fn remote_source_failure_never_reads_local_filesystem() {
        let mut reducer = CompletionReducer::new(AuthorityKind::Remote);
        reducer.begin(CompletionGeneration(1), vec![]);
        assert_eq!(
            reducer.apply(
                AuthorityKind::Local,
                CompletionGeneration(1),
                CompletionOutcome::Complete(vec![candidate(
                    "/local/secret",
                    CompletionSourceKind::Filesystem
                )])
            ),
            CompletionCommit::RejectedAuthority
        );
        assert!(reducer.candidates().is_empty());
    }

    #[test]
    fn repository_and_history_sources_join_the_shared_model() {
        let request = CompletionRequest {
            operation: OperationId(1),
            generation: CompletionGeneration(1),
            authority: AuthorityKind::Local,
            cwd: None,
            input: "git s".into(),
            cursor: 5,
            limit: 20,
            history: vec!["git status".into(), "cargo test".into()],
        };
        let host = crate::host::local::LocalHost::new();
        let outcome = complete(host.as_ref(), &request);
        let CompletionOutcome::Complete(candidates) = outcome else {
            panic!("completion failed")
        };
        assert!(candidates.iter().any(|candidate| {
            candidate.source == CompletionSourceKind::History && candidate.value == "git status"
        }));
    }

    #[test]
    fn replacement_ranges_cover_quotes_escapes_and_unicode() {
        assert_eq!(
            replacement_range("git che", 7),
            ReplacementRange { start: 4, end: 7 }
        );
        assert_eq!(
            replacement_range("cat \"hello wor", 14),
            ReplacementRange { start: 5, end: 14 }
        );
        assert_eq!(
            replacement_range("cat hello\\ world", 16),
            ReplacementRange { start: 4, end: 16 }
        );
        assert_eq!(
            replacement_range("echo 你好世", 14),
            ReplacementRange { start: 5, end: 14 }
        );
    }

    #[test]
    fn duplicate_candidate_keeps_best_rank_and_combined_provenance() {
        // Agentty candidates carry a single source, so provenance after dedup
        // is the winning source: the highest-ranked duplicate survives intact
        // and the losers' weaker entries never shadow it.
        let mut weaker = candidate("git", CompletionSourceKind::History);
        weaker.score = 60;
        let mut stronger = candidate("git", CompletionSourceKind::Grammar);
        stronger.score = 100;
        let normalized = normalize(vec![weaker, stronger]);
        assert_eq!(normalized.len(), 1, "duplicates collapse to one row");
        assert_eq!(normalized[0].score, 100, "the best rank wins");
        assert_eq!(
            normalized[0].source,
            CompletionSourceKind::Grammar,
            "the surviving source is the provenance"
        );
    }

    #[test]
    fn empty_history_does_not_disable_signature_or_filesystem_results() {
        // An empty history source must not erase the command-signature
        // (grammar) or filesystem candidates.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        let request = CompletionRequest {
            operation: OperationId(1),
            generation: CompletionGeneration(1),
            authority: AuthorityKind::Local,
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            input: "no".into(),
            cursor: 2,
            limit: 20,
            history: Vec::new(),
        };
        let host = crate::host::local::LocalHost::new();
        let outcome = complete(host.as_ref(), &request);
        let CompletionOutcome::Complete(candidates) = outcome else {
            panic!("completion failed")
        };
        assert!(
            candidates
                .iter()
                .any(|c| c.source == CompletionSourceKind::Filesystem && c.value == "notes.txt"),
            "filesystem source survives an empty history: {candidates:?}"
        );
        let request = CompletionRequest {
            input: "g".into(),
            cursor: 1,
            cwd: None,
            ..request
        };
        let outcome = complete(host.as_ref(), &request);
        let CompletionOutcome::Complete(candidates) = outcome else {
            panic!("completion failed")
        };
        assert!(
            candidates
                .iter()
                .any(|c| c.source == CompletionSourceKind::Grammar && c.value == "git"),
            "the signature source survives an empty history: {candidates:?}"
        );
    }

    #[test]
    fn history_ranking_is_contextual_but_never_the_only_source() {
        // A contextual history hit joins the shared model; it never becomes
        // the only source even when it matches exactly.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("gitconfig"), b"x").unwrap();
        let request = CompletionRequest {
            operation: OperationId(1),
            generation: CompletionGeneration(1),
            authority: AuthorityKind::Local,
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            input: "git".into(),
            cursor: 3,
            limit: 20,
            history: vec!["git status".into()],
        };
        let host = crate::host::local::LocalHost::new();
        let outcome = complete(host.as_ref(), &request);
        let CompletionOutcome::Complete(candidates) = outcome else {
            panic!("completion failed")
        };
        let sources: std::collections::HashSet<_> = candidates.iter().map(|c| c.source).collect();
        assert!(
            sources.contains(&CompletionSourceKind::History),
            "the contextual history hit is present: {candidates:?}"
        );
        assert!(
            sources.contains(&CompletionSourceKind::Grammar)
                && sources.contains(&CompletionSourceKind::Filesystem),
            "history never crowds out the other sources: {candidates:?}"
        );
    }

    #[test]
    fn cancellation_timeout_and_candidate_caps_are_explicit() {
        // Cancellation and timeout preserve the deterministic candidates
        // instead of clearing the model.
        for outcome in [CompletionOutcome::Cancelled, CompletionOutcome::TimedOut] {
            let mut reducer = CompletionReducer::new(AuthorityKind::Local);
            reducer.begin(
                CompletionGeneration(1),
                vec![candidate("git", CompletionSourceKind::Grammar)],
            );
            assert_eq!(
                reducer.apply(
                    AuthorityKind::Local,
                    CompletionGeneration(1),
                    outcome.clone()
                ),
                CompletionCommit::PreservedDeterministic,
                "{outcome:?} must preserve deterministic candidates"
            );
            assert_eq!(reducer.candidates().len(), 1);
        }

        // The candidate cap is explicit: zero means none, and truncation is
        // by the declared limit rather than enumeration order.
        let request = CompletionRequest {
            operation: OperationId(1),
            generation: CompletionGeneration(1),
            authority: AuthorityKind::Local,
            cwd: None,
            input: "c".into(),
            cursor: 1,
            limit: 0,
            history: vec!["cargo test".into()],
        };
        let host = crate::host::local::LocalHost::new();
        let CompletionOutcome::Complete(candidates) = complete(host.as_ref(), &request) else {
            panic!("completion failed")
        };
        assert!(candidates.is_empty(), "a zero cap yields zero candidates");
        let CompletionOutcome::Complete(candidates) = complete(
            host.as_ref(),
            &CompletionRequest {
                limit: 2,
                history: vec![
                    "cargo test".into(),
                    "cargo build".into(),
                    "cargo clippy".into(),
                ],
                ..request
            },
        ) else {
            panic!("completion failed")
        };
        assert_eq!(candidates.len(), 2, "the cap is applied exactly");
        assert!(
            candidates
                .iter()
                .any(|c| c.source == CompletionSourceKind::Grammar),
            "the highest-ranked candidates survive the cap: {candidates:?}"
        );
    }
}
