use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{AgentSessionKey, AgentSessionRecord, ResumeInvocation};
use crate::core::cli_agent::CLIAgent;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NavigatorRowId(String);

impl NavigatorRowId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SessionIdentity {
    Provider(AgentSessionKey),
    Durable(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveSession {
    pub identity: SessionIdentity,
    pub agent: CLIAgent,
    pub session_id: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub launch_argv: Vec<String>,
    pub carrier: LiveCarrier,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveCarrier {
    pub tab_id: String,
    pub pane_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowLifecycle {
    Virtual,
    Restoring,
    Live,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigatorRow {
    pub row_id: NavigatorRowId,
    pub identity: SessionIdentity,
    pub agent: CLIAgent,
    pub session_id: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub launch_argv: Vec<String>,
    pub lifecycle: RowLifecycle,
    pub carrier: Option<LiveCarrier>,
    pub pinned: bool,
    pub alias: Option<String>,
    pub display_order: u64,
}

impl NavigatorRow {
    pub fn resume_invocation(&self) -> Option<ResumeInvocation> {
        if self.lifecycle == RowLifecycle::Live {
            return None;
        }
        self.agent.resume_invocation(
            self.session_id.as_deref()?,
            (!self.launch_argv.is_empty()).then_some(self.launch_argv.as_slice()),
            self.cwd.clone(),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    Success(LiveCarrier),
    Retryable(String),
    Cancelled,
    ModelBroken(String),
}

#[derive(Clone, Debug, Default)]
pub struct SessionNavigator {
    rows: Vec<NavigatorRow>,
    selected: Option<NavigatorRowId>,
    identity_rows: HashMap<SessionIdentity, NavigatorRowId>,
    tombstones: HashSet<NavigatorRowId>,
    next_row: u64,
    next_order: u64,
}

impl SessionNavigator {
    pub fn rows(&self) -> &[NavigatorRow] {
        &self.rows
    }
    pub fn selected(&self) -> Option<&NavigatorRowId> {
        self.selected.as_ref()
    }

    pub fn refresh(&mut self, historical: &[AgentSessionRecord], live: &[LiveSession]) {
        let mut seen = HashSet::new();
        for session in historical {
            let identity = SessionIdentity::Provider(session.key.clone());
            let row_id = self.row_id_for(identity.clone());
            seen.insert(row_id.clone());
            self.upsert(
                row_id,
                identity,
                session.agent,
                Some(session.key.session_id.clone()),
                session.title.clone(),
                session.cwd.clone(),
                session.launch_argv.clone(),
                RowLifecycle::Virtual,
                None,
            );
        }
        for session in live {
            let row_id = self.row_id_for(session.identity.clone());
            seen.insert(row_id.clone());
            self.upsert(
                row_id,
                session.identity.clone(),
                session.agent,
                session.session_id.clone(),
                session.title.clone(),
                session.cwd.clone(),
                session.launch_argv.clone(),
                RowLifecycle::Live,
                Some(session.carrier.clone()),
            );
        }
        self.rows
            .retain(|row| seen.contains(&row.row_id) || row.lifecycle == RowLifecycle::Restoring);
        self.rows.sort_by_key(|row| row.display_order);
        if self
            .selected
            .as_ref()
            .is_some_and(|id| !self.rows.iter().any(|row| &row.row_id == id))
        {
            self.selected = None;
        }
    }

    pub fn select(&mut self, row_id: &NavigatorRowId) -> bool {
        if self.rows.iter().any(|row| &row.row_id == row_id) {
            self.selected = Some(row_id.clone());
            true
        } else {
            false
        }
    }

    pub fn begin_restore(&mut self, row_id: &NavigatorRowId) -> Option<ResumeInvocation> {
        let row = self.rows.iter_mut().find(|row| &row.row_id == row_id)?;
        let invocation = row.resume_invocation()?;
        row.lifecycle = RowLifecycle::Restoring;
        self.selected = Some(row_id.clone());
        Some(invocation)
    }

    pub fn finish_restore(&mut self, row_id: &NavigatorRowId, outcome: RestoreOutcome) -> bool {
        let Some(row) = self.rows.iter_mut().find(|row| &row.row_id == row_id) else {
            return false;
        };
        if row.lifecycle != RowLifecycle::Restoring {
            return false;
        }
        match outcome {
            RestoreOutcome::Success(carrier) => {
                row.lifecycle = RowLifecycle::Live;
                row.carrier = Some(carrier);
            }
            RestoreOutcome::Retryable(_) | RestoreOutcome::Cancelled => {
                row.lifecycle = RowLifecycle::Virtual;
                row.carrier = None;
            }
            RestoreOutcome::ModelBroken(_) => {
                row.lifecycle = RowLifecycle::Virtual;
                row.carrier = None;
            }
        }
        true
    }

    pub fn delete(&mut self, row_id: &NavigatorRowId) -> bool {
        let before = self.rows.len();
        self.rows.retain(|row| &row.row_id != row_id);
        if before == self.rows.len() {
            return false;
        }
        self.tombstones.insert(row_id.clone());
        self.identity_rows.retain(|_, id| id != row_id);
        if self.selected.as_ref() == Some(row_id) {
            self.selected = None;
        }
        true
    }

    pub fn set_pin(&mut self, row_id: &NavigatorRowId, pinned: bool) -> bool {
        let Some(row) = self.rows.iter_mut().find(|row| &row.row_id == row_id) else {
            return false;
        };
        row.pinned = pinned;
        true
    }

    pub fn set_alias(&mut self, row_id: &NavigatorRowId, alias: Option<String>) -> bool {
        let Some(row) = self.rows.iter_mut().find(|row| &row.row_id == row_id) else {
            return false;
        };
        row.alias = alias.filter(|alias| !alias.trim().is_empty());
        true
    }

    fn row_id_for(&mut self, identity: SessionIdentity) -> NavigatorRowId {
        if let Some(id) = self.identity_rows.get(&identity) {
            return id.clone();
        }
        let id = NavigatorRowId(format!("row-{}", self.next_row));
        self.next_row += 1;
        self.identity_rows.insert(identity, id.clone());
        id
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert(
        &mut self,
        row_id: NavigatorRowId,
        identity: SessionIdentity,
        agent: CLIAgent,
        session_id: Option<String>,
        title: Option<String>,
        cwd: Option<String>,
        launch_argv: Vec<String>,
        lifecycle: RowLifecycle,
        carrier: Option<LiveCarrier>,
    ) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.row_id == row_id) {
            row.identity = identity;
            row.agent = agent;
            row.session_id = session_id;
            row.title = title.or_else(|| row.title.clone());
            row.cwd = cwd.or_else(|| row.cwd.clone());
            row.launch_argv = launch_argv;
            row.lifecycle = lifecycle;
            row.carrier = carrier;
            return;
        }
        let order = self.next_order;
        self.next_order += 1;
        self.rows.push(NavigatorRow {
            row_id,
            identity,
            agent,
            session_id,
            title,
            cwd,
            launch_argv,
            lifecycle,
            carrier,
            pinned: false,
            alias: None,
            display_order: order,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(id: &str) -> AgentSessionRecord {
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
    fn live(id: &str, tab: &str, pane: u64) -> LiveSession {
        LiveSession {
            identity: SessionIdentity::Provider(history(id).key),
            agent: CLIAgent::Codex,
            session_id: Some(id.into()),
            title: Some(id.into()),
            cwd: Some("/repo".into()),
            launch_argv: vec![],
            carrier: LiveCarrier {
                tab_id: tab.into(),
                pane_id: pane,
            },
        }
    }

    #[test]
    fn live_and_history_merge_into_one_logical_row() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("s1")], &[live("s1", "t1", 7)]);
        assert_eq!(model.rows.len(), 1);
        assert_eq!(model.rows[0].lifecycle, RowLifecycle::Live);
    }

    #[test]
    fn opaque_row_identity_survives_carrier_replacement() {
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("s1", "old", 1)]);
        let id = model.rows[0].row_id.clone();
        model.set_pin(&id, true);
        model.set_alias(&id, Some("mine".into()));
        model.refresh(&[], &[live("s1", "new", 9)]);
        assert_eq!(model.rows[0].row_id, id);
        assert_eq!(model.rows[0].carrier.as_ref().unwrap().tab_id, "new");
        assert!(model.rows[0].pinned);
        assert_eq!(model.rows[0].alias.as_deref(), Some("mine"));
    }

    #[test]
    fn coordinate_reuse_after_delete_allocates_new_identity() {
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("s1", "t", 1)]);
        let old = model.rows[0].row_id.clone();
        model.delete(&old);
        model.refresh(&[], &[live("s2", "t", 1)]);
        assert_ne!(model.rows[0].row_id, old);
        assert!(model.tombstones.contains(&old));
    }

    #[test]
    fn restore_failure_rolls_back_without_duplicate_row() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("s1")], &[]);
        let id = model.rows[0].row_id.clone();
        assert!(model.begin_restore(&id).is_some());
        assert!(model.finish_restore(&id, RestoreOutcome::Retryable("spawn failed".into())));
        assert_eq!(model.rows.len(), 1);
        assert_eq!(model.rows[0].lifecycle, RowLifecycle::Virtual);
        assert_eq!(model.rows[0].row_id, id);
    }

    #[test]
    fn missing_restore_content_has_zero_side_effects() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("s1")], &[]);
        let before = model.clone();
        assert!(
            model
                .begin_restore(&NavigatorRowId("missing".into()))
                .is_none()
        );
        assert_eq!(model.rows, before.rows);
        assert_eq!(model.selected, before.selected);
    }

    #[test]
    fn closing_live_carrier_hands_off_to_native_history() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("s1")], &[live("s1", "t", 1)]);
        let id = model.rows[0].row_id.clone();
        model.refresh(&[history("s1")], &[]);
        assert_eq!(model.rows[0].row_id, id);
        assert_eq!(model.rows[0].lifecycle, RowLifecycle::Virtual);
    }
}
