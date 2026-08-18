use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::agent_runtime::title::SessionTitleCandidates;
use crate::core::cli_agent::CLIAgent;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OperationId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScanGeneration(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityKind {
    Local,
    Remote,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentSessionKey {
    pub provider: String,
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionRecord {
    pub key: AgentSessionKey,
    pub agent: CLIAgent,
    /// Legacy resolved title retained for persisted/wire compatibility.
    pub title: Option<String>,
    /// Separate provider and first-user evidence. Older records omit this
    /// field and are interpreted through `effective_title_candidates`.
    #[serde(default, skip_serializing_if = "title_candidates_empty")]
    pub title_candidates: SessionTitleCandidates,
    pub cwd: Option<String>,
    pub updated_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub launch_argv: Vec<String>,
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub created_at_unix_ms: Option<u64>,
}

fn title_candidates_empty(value: &SessionTitleCandidates) -> bool {
    let normalized = SessionTitleCandidates::from_raw(
        value.provider_title.as_deref(),
        value.first_user_title.as_deref(),
    );
    normalized.provider_title.is_none() && normalized.first_user_title.is_none()
}

impl AgentSessionRecord {
    /// Return typed title evidence while accepting legacy records that only
    /// persisted the resolved `title` field. The legacy value is treated as a
    /// provider candidate because its original source is unknowable.
    pub fn effective_title_candidates(&self) -> SessionTitleCandidates {
        // Struct fields remain public for the current internal API, so rebuild
        // through the canonical constructor instead of trusting a direct
        // literal to have passed the serde normalizer.
        let mut candidates = SessionTitleCandidates::from_raw(
            self.title_candidates.provider_title.as_deref(),
            self.title_candidates.first_user_title.as_deref(),
        );
        candidates.merge_legacy_title(self.title.as_deref());
        candidates
    }

    pub(crate) fn merged_title_evidence_with(&self, other: &Self) -> SessionTitleCandidates {
        let mut candidates = self.effective_title_candidates();
        candidates.merge(&other.effective_title_candidates());
        candidates
    }

    pub(crate) fn set_title_evidence(&mut self, candidates: SessionTitleCandidates) {
        self.title = candidates.resolved().map(str::to_owned);
        self.title_candidates = candidates;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryOutcome {
    Complete(Vec<AgentSessionRecord>),
    Failed { message: String },
    Cancelled,
    SourceMissing { source: String },
    Partial { failed_providers: Vec<String> },
    SourceLimitExceeded { source: String, limit: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveryCommit {
    Replaced,
    Preserved,
    IgnoredStale,
    RejectedAuthority,
}

#[derive(Clone, Debug)]
pub struct DiscoveryReducer {
    authority: AuthorityKind,
    requested: ScanGeneration,
    committed: Option<ScanGeneration>,
    rows: Vec<AgentSessionRecord>,
}

impl DiscoveryReducer {
    pub fn new(authority: AuthorityKind) -> Self {
        Self {
            authority,
            requested: ScanGeneration(0),
            committed: None,
            rows: Vec::new(),
        }
    }

    pub fn begin(&mut self, generation: ScanGeneration) -> bool {
        if generation <= self.requested {
            return false;
        }
        self.requested = generation;
        true
    }

    pub fn authority(&self) -> AuthorityKind {
        self.authority
    }

    pub fn committed_generation(&self) -> Option<ScanGeneration> {
        self.committed
    }

    pub fn rows(&self) -> &[AgentSessionRecord] {
        &self.rows
    }

    pub fn apply(
        &mut self,
        authority: AuthorityKind,
        generation: ScanGeneration,
        outcome: DiscoveryOutcome,
    ) -> DiscoveryCommit {
        if authority != self.authority {
            return DiscoveryCommit::RejectedAuthority;
        }
        if generation != self.requested {
            return DiscoveryCommit::IgnoredStale;
        }
        match outcome {
            DiscoveryOutcome::Complete(rows) => {
                self.rows = canonicalize(rows);
                self.committed = Some(generation);
                DiscoveryCommit::Replaced
            }
            DiscoveryOutcome::Failed { .. }
            | DiscoveryOutcome::Cancelled
            | DiscoveryOutcome::SourceMissing { .. }
            | DiscoveryOutcome::Partial { .. }
            | DiscoveryOutcome::SourceLimitExceeded { .. } => DiscoveryCommit::Preserved,
        }
    }
}

fn canonicalize(rows: Vec<AgentSessionRecord>) -> Vec<AgentSessionRecord> {
    let mut unique: HashMap<AgentSessionKey, AgentSessionRecord> = HashMap::new();
    for row in rows {
        if let Some(old) = unique.get_mut(&row.key) {
            // Title evidence is merged through the deterministic typed
            // resolver; ordinary metadata is still selected by freshness.
            // Compute the evidence before replacing the metadata winner so a
            // later row cannot silently discard either candidate slot.
            let candidates = old.merged_title_evidence_with(&row);
            if row.updated_at_unix_ms >= old.updated_at_unix_ms {
                *old = row;
            }
            old.set_title_evidence(candidates);
        } else {
            let mut row = row;
            let candidates = row.effective_title_candidates();
            row.set_title_evidence(candidates);
            unique.insert(row.key.clone(), row);
        }
    }
    let mut rows: Vec<_> = unique.into_values().collect();
    rows.sort_by(|a, b| {
        b.updated_at_unix_ms
            .cmp(&a.updated_at_unix_ms)
            .then_with(|| a.key.provider.cmp(&b.key.provider))
            .then_with(|| a.key.session_id.cmp(&b.key.session_id))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str) -> AgentSessionRecord {
        AgentSessionRecord {
            key: AgentSessionKey {
                provider: "codex".into(),
                session_id: id.into(),
            },
            agent: CLIAgent::Codex,
            title: Some(id.into()),
            title_candidates: SessionTitleCandidates::from_raw(Some(id), None),
            cwd: Some("/repo".into()),
            updated_at_unix_ms: Some(1),
            launch_argv: vec![],
            source_path: None,
            created_at_unix_ms: None,
        }
    }

    #[test]
    fn agent_session_record_title_candidates_default_for_legacy_json() {
        let mut current = row("typed");
        current.title = Some("Provider title".into());
        current.title_candidates =
            crate::agent_runtime::SessionTitleCandidates::from_raw(None, Some("First user"));
        let encoded = serde_json::to_value(&current).unwrap();
        assert!(encoded.get("title_candidates").is_some());

        let legacy = serde_json::json!({
            "key": {"provider": "codex", "session_id": "legacy"},
            "agent": "Codex",
            "title": "Legacy title",
            "cwd": null,
            "updated_at_unix_ms": null,
            "launch_argv": [],
            "source_path": null,
            "created_at_unix_ms": null
        });
        let decoded: AgentSessionRecord = serde_json::from_value(legacy).unwrap();
        assert!(decoded.title_candidates.provider_title.is_none());
        assert!(decoded.title_candidates.first_user_title.is_none());
        assert_eq!(
            decoded.effective_title_candidates().resolved(),
            Some("Legacy title")
        );

        current.title = Some("First user".into());
        let effective = current.effective_title_candidates();
        assert!(effective.provider_title.is_none());
        assert_eq!(effective.first_user_title.as_deref(), Some("First user"));

        current.title = Some("Provider title".into());
        let effective = current.effective_title_candidates();
        assert_eq!(effective.provider_title.as_deref(), Some("Provider title"));
        assert_eq!(effective.first_user_title.as_deref(), Some("First user"));
    }

    #[test]
    fn effective_title_candidates_normalizes_direct_legacy_and_typed_values() {
        let mut current = row("typed-raw");
        current.title = Some("Agent会话".into());
        current.title_candidates = SessionTitleCandidates {
            provider_title: Some("Agent 会话".into()),
            first_user_title: Some("  Draw a fox\nsecond line  ".into()),
        };

        let effective = current.effective_title_candidates();
        assert_eq!(effective.provider_title, None);
        assert_eq!(effective.first_user_title.as_deref(), Some("Draw a fox"));
        assert_eq!(effective.resolved(), Some("Draw a fox"));
    }

    #[test]
    fn canonicalize_merges_title_evidence_from_index_and_transcript_rows() {
        let mut provider = row("same");
        provider.updated_at_unix_ms = Some(20);
        provider.title = Some("Provider title".into());
        provider.title_candidates = SessionTitleCandidates::from_raw(Some("Provider title"), None);
        let mut first_user = row("same");
        first_user.updated_at_unix_ms = Some(10);
        first_user.title = Some("First request".into());
        first_user.title_candidates = SessionTitleCandidates::from_raw(None, Some("First request"));

        let rows = canonicalize(vec![provider, first_user]);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].title_candidates.provider_title.as_deref(),
            Some("Provider title")
        );
        assert_eq!(
            rows[0].title_candidates.first_user_title.as_deref(),
            Some("First request")
        );
    }

    #[test]
    fn canonicalize_keeps_deterministic_provider_title_but_newest_metadata() {
        let mut first = row("same");
        first.title = Some("Provider A".into());
        first.title_candidates = SessionTitleCandidates::from_raw(Some("Provider A"), None);
        first.updated_at_unix_ms = Some(10);
        first.cwd = Some("/first".into());

        let mut newer = row("same");
        newer.title = Some("Provider B".into());
        newer.title_candidates = SessionTitleCandidates::from_raw(Some("Provider B"), None);
        newer.updated_at_unix_ms = Some(20);
        newer.cwd = Some("/newer".into());

        let rows = canonicalize(vec![first, newer]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].updated_at_unix_ms, Some(20));
        assert_eq!(rows[0].cwd.as_deref(), Some("/newer"));
        assert_eq!(
            rows[0].title_candidates.provider_title.as_deref(),
            Some("Provider A")
        );
        assert_eq!(rows[0].title.as_deref(), Some("Provider A"));
    }

    fn seeded(authority: AuthorityKind) -> DiscoveryReducer {
        let mut state = DiscoveryReducer::new(authority);
        assert!(state.begin(ScanGeneration(1)));
        assert_eq!(
            state.apply(
                authority,
                ScanGeneration(1),
                DiscoveryOutcome::Complete(vec![row("old")]),
            ),
            DiscoveryCommit::Replaced
        );
        state
    }

    #[test]
    fn failed_scan_preserves_last_committed_rows() {
        let mut state = seeded(AuthorityKind::Local);
        state.begin(ScanGeneration(2));
        assert_eq!(
            state.apply(
                AuthorityKind::Local,
                ScanGeneration(2),
                DiscoveryOutcome::Failed {
                    message: "bad json".into()
                },
            ),
            DiscoveryCommit::Preserved
        );
        assert_eq!(state.rows(), &[row("old")]);
        assert_eq!(state.committed_generation(), Some(ScanGeneration(1)));
    }

    #[test]
    fn complete_scan_atomically_replaces_rows() {
        let mut state = seeded(AuthorityKind::Local);
        state.begin(ScanGeneration(2));
        assert_eq!(
            state.apply(
                AuthorityKind::Local,
                ScanGeneration(2),
                DiscoveryOutcome::Complete(vec![row("new")]),
            ),
            DiscoveryCommit::Replaced
        );
        assert_eq!(state.rows(), &[row("new")]);
    }

    #[test]
    fn stale_generation_is_ignored() {
        let mut state = seeded(AuthorityKind::Local);
        state.begin(ScanGeneration(2));
        assert_eq!(
            state.apply(
                AuthorityKind::Local,
                ScanGeneration(1),
                DiscoveryOutcome::Complete(vec![row("stale")]),
            ),
            DiscoveryCommit::IgnoredStale
        );
        assert_eq!(state.rows(), &[row("old")]);
    }

    #[test]
    fn cancelled_scan_preserves_rows() {
        let mut state = seeded(AuthorityKind::Local);
        state.begin(ScanGeneration(2));
        assert_eq!(
            state.apply(
                AuthorityKind::Local,
                ScanGeneration(2),
                DiscoveryOutcome::Cancelled
            ),
            DiscoveryCommit::Preserved
        );
        assert_eq!(state.rows(), &[row("old")]);
    }

    #[test]
    fn source_missing_preserves_rows() {
        let mut state = seeded(AuthorityKind::Local);
        state.begin(ScanGeneration(2));
        assert_eq!(
            state.apply(
                AuthorityKind::Local,
                ScanGeneration(2),
                DiscoveryOutcome::SourceMissing {
                    source: "~/.codex/sessions".into()
                }
            ),
            DiscoveryCommit::Preserved
        );
        assert_eq!(state.rows(), &[row("old")]);
    }

    #[test]
    fn partial_provider_failure_does_not_publish_subset() {
        let mut state = seeded(AuthorityKind::Local);
        state.begin(ScanGeneration(2));
        assert_eq!(
            state.apply(
                AuthorityKind::Local,
                ScanGeneration(2),
                DiscoveryOutcome::Partial {
                    failed_providers: vec!["claude".into()]
                }
            ),
            DiscoveryCommit::Preserved
        );
        assert_eq!(state.rows(), &[row("old")]);
    }

    #[test]
    fn remote_authority_never_falls_back_to_local_rows() {
        let mut state = seeded(AuthorityKind::Remote);
        state.begin(ScanGeneration(2));
        assert_eq!(
            state.apply(
                AuthorityKind::Local,
                ScanGeneration(2),
                DiscoveryOutcome::Complete(vec![row("local")]),
            ),
            DiscoveryCommit::RejectedAuthority
        );
        assert_eq!(state.authority(), AuthorityKind::Remote);
        assert_eq!(state.rows(), &[row("old")]);
    }

    #[test]
    fn passive_refresh_coalesces_with_inflight_generation() {
        // A passive refresh while a scan is in flight must reuse the in-flight
        // generation instead of starting a competing one.
        let mut state = seeded(AuthorityKind::Local);
        assert!(state.begin(ScanGeneration(2)), "first refresh starts");
        assert!(
            !state.begin(ScanGeneration(2)),
            "a duplicate passive refresh coalesces with the in-flight generation"
        );
        assert!(
            !state.begin(ScanGeneration(1)),
            "an older tick can never displace the in-flight generation"
        );
        // The in-flight result still lands.
        assert_eq!(
            state.apply(
                AuthorityKind::Local,
                ScanGeneration(2),
                DiscoveryOutcome::Complete(vec![row("new")]),
            ),
            DiscoveryCommit::Replaced
        );
        assert_eq!(state.rows(), &[row("new")]);
    }

    #[test]
    fn explicit_refresh_supersedes_stuck_generation() {
        // An explicit refresh while an older scan is stuck starts a newer
        // generation; the stuck scan's late result must not overwrite it.
        let mut state = seeded(AuthorityKind::Local);
        assert!(state.begin(ScanGeneration(2)), "scan 2 starts and stalls");
        assert!(
            state.begin(ScanGeneration(3)),
            "explicit refresh supersedes the stuck generation"
        );
        assert_eq!(
            state.apply(
                AuthorityKind::Local,
                ScanGeneration(2),
                DiscoveryOutcome::Complete(vec![row("stale")]),
            ),
            DiscoveryCommit::IgnoredStale,
            "the stuck scan's late result is ignored"
        );
        assert_eq!(state.rows(), &[row("old")]);
        assert_eq!(
            state.apply(
                AuthorityKind::Local,
                ScanGeneration(3),
                DiscoveryOutcome::Complete(vec![row("fresh")]),
            ),
            DiscoveryCommit::Replaced
        );
        assert_eq!(state.rows(), &[row("fresh")]);
        assert_eq!(state.committed_generation(), Some(ScanGeneration(3)));
    }
}
