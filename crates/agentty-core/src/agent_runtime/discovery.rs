use std::collections::HashMap;

use serde::{Deserialize, Serialize};

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
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub updated_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub launch_argv: Vec<String>,
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
    let mut unique = HashMap::new();
    for row in rows {
        let replace = unique.get(&row.key).is_none_or(|old: &AgentSessionRecord| {
            row.updated_at_unix_ms >= old.updated_at_unix_ms
        });
        if replace {
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
            cwd: Some("/repo".into()),
            updated_at_unix_ms: Some(1),
            launch_argv: vec![],
        }
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
