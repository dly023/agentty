use std::collections::HashMap;
use std::path::PathBuf;

use agentty_core::agent_runtime::{
    AgentSessionRecord, AgentStoreRoots, NavigatorRow, RowLifecycle, SessionNavigator,
    SessionUserStateStore,
};
use agentty_core::core::environment::EnvironmentId;

/// Per-environment Session Navigator stash for the single-window environment
/// rail (ENV-SINGLE-WINDOW-RAIL-47). The content column still binds one
/// Environment authority at a time; this cache preserves committed Navigator
/// projections so switching back restores rows immediately and non-current
/// rail headers can show live/total counts without promoting local data as
/// remote authority.
#[derive(Default)]
pub(crate) struct EnvironmentNavigatorCache {
    entries: HashMap<EnvironmentId, CachedEnvironmentNavigator>,
}

#[derive(Clone)]
pub(crate) struct CachedEnvironmentNavigator {
    pub navigator: SessionNavigator,
    pub history: Vec<AgentSessionRecord>,
    pub history_environment: Option<EnvironmentId>,
    pub user_state: SessionUserStateStore,
    pub user_state_path: Option<PathBuf>,
    pub store_roots: Option<AgentStoreRoots>,
    pub alias_environment: Option<EnvironmentId>,
    pub scan_error: Option<String>,
    pub scan_started: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EnvironmentSessionCounts {
    pub live: usize,
    pub total: usize,
}

impl EnvironmentNavigatorCache {
    pub(crate) fn insert(&mut self, id: EnvironmentId, entry: CachedEnvironmentNavigator) {
        self.entries.insert(id, entry);
    }

    pub(crate) fn take(&mut self, id: &EnvironmentId) -> Option<CachedEnvironmentNavigator> {
        self.entries.remove(id)
    }

    pub(crate) fn get(&self, id: &EnvironmentId) -> Option<&CachedEnvironmentNavigator> {
        self.entries.get(id)
    }

    pub(crate) fn counts(&self, id: &EnvironmentId) -> Option<EnvironmentSessionCounts> {
        self.get(id)
            .map(|entry| counts_from_navigator(&entry.navigator))
    }

    pub(crate) fn preview_rows(&self, id: &EnvironmentId) -> Vec<&NavigatorRow> {
        self.get(id)
            .map(|entry| entry.navigator.rows().iter().collect())
            .unwrap_or_default()
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

pub(crate) fn counts_from_navigator(navigator: &SessionNavigator) -> EnvironmentSessionCounts {
    counts_from_rows(navigator.rows())
}

pub(crate) fn counts_from_rows(rows: &[NavigatorRow]) -> EnvironmentSessionCounts {
    let live = rows
        .iter()
        .filter(|row| matches!(row.lifecycle, RowLifecycle::Live | RowLifecycle::Restoring))
        .count();
    EnvironmentSessionCounts {
        live,
        total: rows.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentty_core::agent_runtime::{
        AgentSessionKey, AgentSessionRecord, LiveCarrier, LiveSession, SessionIdentity,
        SessionTitleCandidates,
    };
    use agentty_core::core::cli_agent::CLIAgent;

    fn history(id: &str) -> AgentSessionRecord {
        AgentSessionRecord {
            key: AgentSessionKey {
                provider: "codex".into(),
                session_id: id.into(),
            },
            agent: CLIAgent::Codex,
            title: Some(id.into()),
            title_candidates: SessionTitleCandidates::default(),
            cwd: Some("/repo".into()),
            updated_at_unix_ms: Some(1),
            launch_argv: vec![],
            source_path: None,
            created_at_unix_ms: None,
        }
    }

    fn live(id: &str) -> LiveSession {
        LiveSession {
            identity: SessionIdentity::Provider(history(id).key.clone()),
            agent: CLIAgent::Codex,
            session_id: Some(id.into()),
            title: Some(id.into()),
            title_candidates: SessionTitleCandidates::default(),
            cwd: Some("/repo".into()),
            launch_argv: vec![],
            carrier: LiveCarrier {
                container_id: format!("container-{id}"),
                tab_id: Some("tab".into()),
                pane_id: Some(1),
            },
            execution: None,
        }
    }

    #[test]
    fn stash_and_take_round_trips_counts() {
        let mut navigator = SessionNavigator::default();
        navigator.refresh(&[history("h1"), history("h2")], &[live("h1")]);
        let mut cache = EnvironmentNavigatorCache::default();
        let env = EnvironmentId::local();
        cache.insert(
            env.clone(),
            CachedEnvironmentNavigator {
                navigator: navigator.clone(),
                history: vec![history("h1"), history("h2")],
                history_environment: Some(env.clone()),
                user_state: Default::default(),
                user_state_path: None,
                store_roots: None,
                alias_environment: Some(env.clone()),
                scan_error: None,
                scan_started: true,
            },
        );
        let counts = cache.counts(&env).expect("cached counts");
        assert_eq!(counts.live, 1);
        assert_eq!(counts.total, 2);
        let restored = cache.take(&env).expect("cached entry");
        assert_eq!(restored.history.len(), 2);
        assert!(cache.get(&env).is_none());
    }

    #[test]
    fn empty_navigator_counts_are_zero() {
        let counts = counts_from_navigator(&SessionNavigator::default());
        assert_eq!(counts, EnvironmentSessionCounts { live: 0, total: 0 });
    }
}
