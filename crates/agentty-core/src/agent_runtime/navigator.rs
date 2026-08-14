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
    pub execution: Option<LiveExecutionState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveCarrier {
    pub container_id: String,
    pub tab_id: Option<String>,
    pub pane_id: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveExecutionState {
    pub state: crate::core::cli_agent::AgentSessionState,
    pub focused: bool,
    pub unread: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionBadge {
    Restoring,
    Waiting,
    Running,
    FocusedLive,
    BackgroundLive,
    CompletedUnread,
}

pub fn execution_badge(
    lifecycle: RowLifecycle,
    state: Option<&crate::core::cli_agent::AgentSessionState>,
    focused: bool,
    unread: bool,
) -> Option<ExecutionBadge> {
    use crate::core::cli_agent::AgentStatus;
    if lifecycle == RowLifecycle::Restoring {
        return Some(ExecutionBadge::Restoring);
    }
    if lifecycle != RowLifecycle::Live {
        return None;
    }
    match state.map(|state| state.status) {
        Some(AgentStatus::Waiting) => Some(ExecutionBadge::Waiting),
        Some(AgentStatus::Working) => Some(ExecutionBadge::Running),
        Some(AgentStatus::Done) if unread => Some(ExecutionBadge::CompletedUnread),
        Some(AgentStatus::Idle | AgentStatus::Done) | None if focused => {
            Some(ExecutionBadge::FocusedLive)
        }
        Some(AgentStatus::Idle | AgentStatus::Done) | None => Some(ExecutionBadge::BackgroundLive),
    }
}

pub fn execution_message(
    state: Option<&crate::core::cli_agent::AgentSessionState>,
) -> Option<&str> {
    state
        .filter(|state| state.status == crate::core::cli_agent::AgentStatus::Waiting)
        .and_then(|state| state.message.as_deref())
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
    pub source_path: Option<String>,
    pub updated_at_unix_ms: Option<u64>,
    pub created_at_unix_ms: Option<u64>,
    pub lifecycle: RowLifecycle,
    pub carrier: Option<LiveCarrier>,
    pub pinned: bool,
    pub alias: Option<String>,
    pub display_order: u64,
    pub execution: Option<LiveExecutionState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionReorderUnit {
    pub row_ids: Vec<NavigatorRowId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionReorderError {
    StalePermutation,
    SplitUnit,
}

/// Canonical Session Navigator display name.
///
/// Precedence: non-empty trimmed user alias, then non-empty trimmed resolved
/// title, then `fallback`. Session id, provider/agent identity, and viewport
/// geometry are intentionally not parameters — callers must not reintroduce them.
pub fn session_display_title(alias: Option<&str>, title: Option<&str>, fallback: &str) -> String {
    non_empty_display_candidate(alias)
        .or_else(|| non_empty_display_candidate(title))
        .unwrap_or(fallback)
        .to_owned()
}

fn non_empty_display_candidate(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn normalize_display_title(title: Option<String>) -> Option<String> {
    title.and_then(|value| {
        let trimmed = value.trim();
        (!is_non_session_title_placeholder(trimmed)).then(|| trimmed.to_owned())
    })
}

/// Terminal/tab chrome and catch-all defaults that must never replace a settled
/// session title. Canonical list lives in [`super::parse::is_absent_session_title`].
fn is_non_session_title_placeholder(title: &str) -> bool {
    super::parse::is_absent_session_title(title)
}

impl NavigatorRow {
    pub fn display_title(&self, fallback: &str) -> String {
        session_display_title(self.alias.as_deref(), self.title.as_deref(), fallback)
    }

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

/// A typed plan captured before any mutation. Carries everything needed to
/// commit or roll back a permanent deletion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletePlan {
    pub row_id: NavigatorRowId,
    pub identity: SessionIdentity,
    pub source_path: Option<String>,
    pub session_id: Option<String>,
    pub agent: CLIAgent,
    pub title: Option<String>,
    pub alias: Option<String>,
    pub pinned: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SessionNavigator {
    rows: Vec<NavigatorRow>,
    selected: Option<NavigatorRowId>,
    identity_rows: HashMap<SessionIdentity, NavigatorRowId>,
    container_rows: HashMap<String, NavigatorRowId>,
    tombstones: HashSet<NavigatorRowId>,
    deleted_identities: HashSet<SessionIdentity>,
    explicit_order: HashSet<SessionIdentity>,
    /// First-seen (refresh_generation, insertion_index) per RowId. Reinsert
    /// after a transient drop must reuse these keys — never overwrite.
    insertion_order: HashMap<NavigatorRowId, (u64, u64)>,
    /// First-seen display_order per RowId, retained across drop/reinsert.
    display_order_by_row: HashMap<NavigatorRowId, u64>,
    refresh_generation: u64,
    next_insertion: u64,
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

    pub fn detail_row(&self, row_id: &NavigatorRowId) -> Option<&NavigatorRow> {
        self.rows.iter().find(|row| &row.row_id == row_id)
    }

    pub fn refresh(&mut self, historical: &[AgentSessionRecord], live: &[LiveSession]) {
        self.refresh_generation = self.refresh_generation.saturating_add(1);
        self.next_insertion = 0;
        let mut seen = HashSet::new();
        for session in historical {
            let identity = SessionIdentity::Provider(session.key.clone());
            if self.deleted_identities.contains(&identity) {
                continue;
            }
            let row_id = live
                .iter()
                .find(|live| live.identity == identity)
                .and_then(|live| self.container_rows.get(&live.carrier.container_id).cloned())
                .unwrap_or_else(|| self.row_id_for(identity.clone()));
            seen.insert(row_id.clone());
            self.upsert(
                row_id,
                identity,
                session.agent,
                Some(session.key.session_id.clone()),
                session.title.clone(),
                session.cwd.clone(),
                session.launch_argv.clone(),
                session.source_path.clone(),
                session.updated_at_unix_ms,
                session.created_at_unix_ms,
                RowLifecycle::Virtual,
                None,
                None,
            );
        }
        for session in live {
            if self.deleted_identities.contains(&session.identity) {
                continue;
            }
            let row_id = self
                .container_rows
                .get(&session.carrier.container_id)
                .cloned()
                .unwrap_or_else(|| self.row_id_for(session.identity.clone()));
            self.container_rows
                .insert(session.carrier.container_id.clone(), row_id.clone());
            seen.insert(row_id.clone());
            self.upsert(
                row_id,
                session.identity.clone(),
                session.agent,
                session.session_id.clone(),
                session.title.clone(),
                session.cwd.clone(),
                session.launch_argv.clone(),
                None,
                None,
                None,
                RowLifecycle::Live,
                Some(session.carrier.clone()),
                session.execution.clone(),
            );
        }
        self.rows
            .retain(|row| seen.contains(&row.row_id) || row.lifecycle == RowLifecycle::Restoring);
        self.sort_rows();
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

    /// Capture a typed plan before any mutation. Returns None for live or
    /// restoring rows (those use soft Close, or [`Self::plan_close_and_delete`]).
    pub fn plan_delete(&self, row_id: &NavigatorRowId) -> Option<DeletePlan> {
        let row = self.rows.iter().find(|row| &row.row_id == row_id)?;
        if row.lifecycle == RowLifecycle::Live || row.lifecycle == RowLifecycle::Restoring {
            return None;
        }
        Some(self.delete_plan_from_row(row))
    }

    /// Capture a permanent-delete plan for a live/restoring row before the
    /// carrier closes. Historical rows use [`Self::plan_delete`] instead.
    pub fn plan_close_and_delete(&self, row_id: &NavigatorRowId) -> Option<DeletePlan> {
        let row = self.rows.iter().find(|row| &row.row_id == row_id)?;
        if row.lifecycle != RowLifecycle::Live && row.lifecycle != RowLifecycle::Restoring {
            return None;
        }
        Some(self.delete_plan_from_row(row))
    }

    fn delete_plan_from_row(&self, row: &NavigatorRow) -> DeletePlan {
        DeletePlan {
            row_id: row.row_id.clone(),
            identity: row.identity.clone(),
            source_path: row.source_path.clone(),
            session_id: row.session_id.clone(),
            agent: row.agent,
            title: row.title.clone(),
            alias: row.alias.clone(),
            pinned: row.pinned,
        }
    }

    /// Commit a delete plan: remove the row and register the tombstone.
    /// Returns false when the row no longer exists (idempotent).
    pub fn commit_delete(&mut self, plan: &DeletePlan) -> bool {
        let before = self.rows.len();
        self.rows.retain(|row| &row.row_id != &plan.row_id);
        if before == self.rows.len() {
            return false;
        }
        self.tombstones.insert(plan.row_id.clone());
        self.deleted_identities.insert(plan.identity.clone());
        self.identity_rows.retain(|_, id| id != &plan.row_id);
        self.container_rows.retain(|_, id| id != &plan.row_id);
        self.insertion_order.remove(&plan.row_id);
        self.display_order_by_row.remove(&plan.row_id);
        self.explicit_order.remove(&plan.identity);
        if self.selected.as_ref() == Some(&plan.row_id) {
            self.selected = None;
        }
        true
    }

    /// Roll back a committed delete: re-insert a Virtual row from the
    /// plan's captured state.  Used when the Host-backed file removal
    /// fails and the Navigator must reflect the still-present artifact.
    pub fn rollback_delete(&mut self, plan: &DeletePlan) -> bool {
        if self.rows.iter().any(|row| &row.row_id == &plan.row_id) {
            return false;
        }
        self.tombstones.remove(&plan.row_id);
        self.deleted_identities.remove(&plan.identity);
        self.identity_rows
            .insert(plan.identity.clone(), plan.row_id.clone());
        let order = *self
            .display_order_by_row
            .entry(plan.row_id.clone())
            .or_insert_with(|| {
                let order = self.next_order;
                self.next_order += 1;
                order
            });
        self.insertion_order
            .entry(plan.row_id.clone())
            .or_insert_with(|| {
                let key = (self.refresh_generation, self.next_insertion);
                self.next_insertion = self.next_insertion.saturating_add(1);
                key
            });
        self.rows.push(NavigatorRow {
            row_id: plan.row_id.clone(),
            identity: plan.identity.clone(),
            agent: plan.agent,
            session_id: plan.session_id.clone(),
            title: plan.title.clone(),
            cwd: None,
            launch_argv: Vec::new(),
            lifecycle: RowLifecycle::Virtual,
            carrier: None,
            pinned: plan.pinned,
            source_path: plan.source_path.clone(),
            updated_at_unix_ms: None,
            created_at_unix_ms: None,
            alias: plan.alias.clone(),
            execution: None,
            display_order: order,
        });
        self.sort_rows();
        true
    }

    /// Direct delete kept for internal use by refresh() and existing
    /// tests; prefer the plan/commit/rollback trio for user-facing
    /// permanent deletion.
    pub fn delete(&mut self, row_id: &NavigatorRowId) -> bool {
        let Some(identity) = self
            .rows
            .iter()
            .find(|row| &row.row_id == row_id)
            .map(|row| row.identity.clone())
        else {
            return false;
        };
        self.rows.retain(|row| &row.row_id != row_id);
        self.tombstones.insert(row_id.clone());
        self.deleted_identities.insert(identity.clone());
        self.identity_rows.retain(|_, id| id != row_id);
        self.container_rows.retain(|_, id| id != row_id);
        self.insertion_order.remove(row_id);
        self.display_order_by_row.remove(row_id);
        self.explicit_order.remove(&identity);
        if self.selected.as_ref() == Some(row_id) {
            self.selected = None;
        }
        true
    }

    pub fn project_pins(&mut self, pins: &[SessionIdentity]) {
        let pins: HashSet<&SessionIdentity> = pins.iter().collect();
        for row in &mut self.rows {
            row.pinned = pins.contains(&row.identity);
        }
        self.sort_rows();
    }
    pub fn project_aliases(&mut self, aliases: &[(SessionIdentity, String)]) {
        let aliases: HashMap<&SessionIdentity, &str> = aliases
            .iter()
            .map(|(identity, alias)| (identity, alias.as_str()))
            .collect();
        for row in &mut self.rows {
            row.alias = aliases.get(&row.identity).map(|alias| (*alias).to_owned());
        }
        self.sort_rows();
    }

    pub fn project_display_order(&mut self, orders: &[(SessionIdentity, u64)]) {
        let orders: HashMap<&SessionIdentity, u64> = orders
            .iter()
            .map(|(identity, order)| (identity, *order))
            .collect();
        self.explicit_order.clear();
        for row in &mut self.rows {
            if let Some(order) = orders.get(&row.identity) {
                row.display_order = *order;
                self.display_order_by_row.insert(row.row_id.clone(), *order);
                self.explicit_order.insert(row.identity.clone());
            }
        }
        if let Some(next) = self
            .rows
            .iter()
            .map(|row| row.display_order)
            .max()
            .and_then(|order| order.checked_add(1))
        {
            self.next_order = self.next_order.max(next);
        }
        self.sort_rows();
    }

    pub fn reorder_units(&self) -> Vec<SessionReorderUnit> {
        let mut units: Vec<SessionReorderUnit> = Vec::new();
        let mut tab_units: HashMap<&str, usize> = HashMap::new();
        for row in &self.rows {
            let tab_id = row
                .carrier
                .as_ref()
                .and_then(|carrier| carrier.tab_id.as_deref());
            if let Some(tab_id) = tab_id {
                if let Some(&index) = tab_units.get(tab_id) {
                    units[index].row_ids.push(row.row_id.clone());
                } else {
                    tab_units.insert(tab_id, units.len());
                    units.push(SessionReorderUnit {
                        row_ids: vec![row.row_id.clone()],
                    });
                }
            } else {
                units.push(SessionReorderUnit {
                    row_ids: vec![row.row_id.clone()],
                });
            }
        }
        units
    }

    pub fn reorder(
        &mut self,
        ordered_row_ids: &[NavigatorRowId],
    ) -> Result<Vec<(SessionIdentity, u64)>, SessionReorderError> {
        if ordered_row_ids.len() != self.rows.len() {
            return Err(SessionReorderError::StalePermutation);
        }
        let current: HashSet<&NavigatorRowId> = self.rows.iter().map(|row| &row.row_id).collect();
        let ordered: HashSet<&NavigatorRowId> = ordered_row_ids.iter().collect();
        if current.len() != self.rows.len()
            || ordered.len() != ordered_row_ids.len()
            || current != ordered
        {
            return Err(SessionReorderError::StalePermutation);
        }

        let units = self.reorder_units();
        let mut remaining: HashMap<&NavigatorRowId, &[NavigatorRowId]> = units
            .iter()
            .filter_map(|unit| {
                unit.row_ids
                    .first()
                    .map(|first| (first, unit.row_ids.as_slice()))
            })
            .collect();
        let mut cursor = 0;
        while cursor < ordered_row_ids.len() {
            let Some(unit) = remaining.remove(&ordered_row_ids[cursor]) else {
                return Err(SessionReorderError::SplitUnit);
            };
            if ordered_row_ids.get(cursor..cursor + unit.len()) != Some(unit) {
                return Err(SessionReorderError::SplitUnit);
            }
            cursor += unit.len();
        }
        if !remaining.is_empty() {
            return Err(SessionReorderError::SplitUnit);
        }

        let positions: HashMap<&NavigatorRowId, u64> = ordered_row_ids
            .iter()
            .enumerate()
            .map(|(index, row_id)| (row_id, index as u64))
            .collect();
        for row in &mut self.rows {
            row.display_order = positions[&row.row_id];
            self.display_order_by_row
                .insert(row.row_id.clone(), row.display_order);
            self.explicit_order.insert(row.identity.clone());
        }
        self.next_order = self.next_order.max(self.rows.len() as u64);
        self.sort_rows();
        Ok(self
            .rows
            .iter()
            .map(|row| (row.identity.clone(), row.display_order))
            .collect())
    }

    fn sort_rows(&mut self) {
        struct Unit {
            rows: Vec<NavigatorRow>,
            pinned: bool,
            explicit: bool,
            order: u64,
            insertion_generation: u64,
            insertion_index: u64,
            original: usize,
        }

        let mut units: Vec<Unit> = Vec::new();
        let mut tab_units: HashMap<String, usize> = HashMap::new();
        for (original, row) in std::mem::take(&mut self.rows).into_iter().enumerate() {
            let explicit = self.explicit_order.contains(&row.identity);
            let (insertion_generation, insertion_index) = self
                .insertion_order
                .get(&row.row_id)
                .copied()
                .unwrap_or_default();
            let tab_id = row
                .carrier
                .as_ref()
                .and_then(|carrier| carrier.tab_id.clone());
            if let Some(tab_id) = tab_id {
                if let Some(&index) = tab_units.get(&tab_id) {
                    let unit = &mut units[index];
                    unit.pinned |= row.pinned;
                    unit.explicit |= explicit;
                    unit.order = unit.order.min(row.display_order);
                    unit.insertion_generation = unit.insertion_generation.min(insertion_generation);
                    unit.insertion_index = unit.insertion_index.min(insertion_index);
                    unit.rows.push(row);
                } else {
                    let index = units.len();
                    tab_units.insert(tab_id, index);
                    units.push(Unit {
                        pinned: row.pinned,
                        explicit,
                        order: row.display_order,
                        insertion_generation,
                        insertion_index,
                        rows: vec![row],
                        original,
                    });
                }
            } else {
                units.push(Unit {
                    pinned: row.pinned,
                    explicit,
                    order: row.display_order,
                    insertion_generation,
                    insertion_index,
                    rows: vec![row],
                    original,
                });
            }
        }
        for unit in &mut units {
            unit.rows.sort_by_key(|row| row.display_order);
        }
        units.sort_by(|left, right| {
            (!left.pinned)
                .cmp(&(!right.pinned))
                .then_with(|| left.explicit.cmp(&right.explicit))
                .then_with(|| {
                    if left.explicit && right.explicit {
                        left.order.cmp(&right.order)
                    } else if !left.explicit && !right.explicit {
                        right
                            .insertion_generation
                            .cmp(&left.insertion_generation)
                            .then_with(|| left.insertion_index.cmp(&right.insertion_index))
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .then_with(|| left.original.cmp(&right.original))
        });
        self.rows = units.into_iter().flat_map(|unit| unit.rows).collect();
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
        source_path: Option<String>,
        updated_at_unix_ms: Option<u64>,
        created_at_unix_ms: Option<u64>,
        lifecycle: RowLifecycle,
        carrier: Option<LiveCarrier>,
        execution: Option<LiveExecutionState>,
    ) {
        self.identity_rows.retain(|_, existing| existing != &row_id);
        self.identity_rows.insert(identity.clone(), row_id.clone());
        let title = normalize_display_title(title);
        if let Some(row) = self.rows.iter_mut().find(|row| row.row_id == row_id) {
            row.identity = identity;
            row.agent = agent;
            row.session_id = session_id;
            // Missing/blank refresh must not wipe a previously settled meaningful
            // title; stuck catch-all placeholders are cleared when still absent.
            row.title = title.or_else(|| normalize_display_title(row.title.clone()));
            row.cwd = cwd.or_else(|| row.cwd.clone());
            row.launch_argv = launch_argv;
            row.source_path = source_path.or_else(|| row.source_path.clone());
            row.updated_at_unix_ms = updated_at_unix_ms.or(row.updated_at_unix_ms);
            row.created_at_unix_ms = created_at_unix_ms.or(row.created_at_unix_ms);
            row.lifecycle = lifecycle;
            row.carrier = carrier;
            if execution.as_ref().is_none_or(|incoming| {
                row.execution
                    .as_ref()
                    .is_none_or(|current| incoming.state.activity_seq >= current.state.activity_seq)
            }) {
                row.execution = execution;
            }
            return;
        }
        // True FirstObserved only: reuse first-seen position on RowId reinsert.
        let order = *self
            .display_order_by_row
            .entry(row_id.clone())
            .or_insert_with(|| {
                let order = self.next_order;
                self.next_order += 1;
                order
            });
        self.insertion_order
            .entry(row_id.clone())
            .or_insert_with(|| {
                let key = (self.refresh_generation, self.next_insertion);
                self.next_insertion = self.next_insertion.saturating_add(1);
                key
            });
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
            source_path,
            updated_at_unix_ms,
            created_at_unix_ms,
            alias: None,
            execution,
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
            source_path: None,
            created_at_unix_ms: None,
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
                container_id: format!("container-{id}"),
                tab_id: Some(tab.into()),
                pane_id: Some(pane),
            },
            execution: None,
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
    fn temporary_live_carrier_row_is_visible_before_provider_history() {
        let mut model = SessionNavigator::default();
        let temporary = LiveSession {
            identity: SessionIdentity::Durable("carrier-1".into()),
            agent: CLIAgent::Codex,
            session_id: None,
            title: None,
            cwd: Some("/repo".into()),
            launch_argv: vec!["codex".into()],
            carrier: LiveCarrier {
                container_id: "carrier-1".into(),
                tab_id: Some("tab".into()),
                pane_id: Some(1),
            },
            execution: None,
        };
        model.refresh(&[], &[temporary]);
        assert_eq!(model.rows.len(), 1);
        assert_eq!(model.rows[0].lifecycle, RowLifecycle::Live);
        assert_eq!(model.rows[0].session_id, None);
        assert_eq!(model.rows[0].source_path, None);
    }

    #[test]
    fn temporary_live_carrier_upgrades_in_place_when_history_arrives() {
        let mut model = SessionNavigator::default();
        let temporary = LiveSession {
            identity: SessionIdentity::Durable("carrier-1".into()),
            agent: CLIAgent::Codex,
            session_id: None,
            title: None,
            cwd: None,
            launch_argv: vec![],
            carrier: LiveCarrier {
                container_id: "carrier-1".into(),
                tab_id: None,
                pane_id: None,
            },
            execution: None,
        };
        model.refresh(&[], &[temporary]);
        let row_id = model.rows[0].row_id.clone();
        let mut upgraded = live("s1", "tab", 1);
        upgraded.carrier.container_id = "carrier-1".into();
        model.refresh(&[history("s1")], &[upgraded]);
        assert_eq!(model.rows.len(), 1);
        assert_eq!(model.rows[0].row_id, row_id);
        assert_eq!(model.rows[0].session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn opaque_row_identity_survives_carrier_replacement() {
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("s1", "old", 1)]);
        let id = model.rows[0].row_id.clone();
        model.project_pins(&[model.rows[0].identity.clone()]);
        model.project_aliases(&[(model.rows[0].identity.clone(), "mine".into())]);
        model.refresh(&[], &[live("s1", "new", 9)]);
        assert_eq!(model.rows[0].row_id, id);
        assert_eq!(
            model.rows[0].carrier.as_ref().unwrap().tab_id.as_deref(),
            Some("new")
        );
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

    #[test]
    fn navigator_render_identity_uses_canonical_row_id() {
        // Render identity is the canonical row id allocated from the session
        // identity — never the physical carrier coordinates. Two sessions
        // reusing the same tab/pane coordinates get distinct row ids, and one
        // session keeps its row id when its carrier moves.
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("s1", "t", 1)]);
        let s1 = model.rows[0].row_id.clone();
        model.refresh(&[], &[live("s1", "t", 1), live("s2", "t", 1)]);
        let s2 = model
            .rows
            .iter()
            .find(|row| row.session_id.as_deref() == Some("s2"))
            .expect("s2 row")
            .row_id
            .clone();
        assert_ne!(
            s1, s2,
            "coordinate reuse across sessions must not collapse identity"
        );
        model.refresh(&[], &[live("s1", "moved", 42)]);
        let s1_after = model
            .rows
            .iter()
            .find(|row| row.session_id.as_deref() == Some("s1"))
            .expect("s1 row")
            .row_id
            .clone();
        assert_eq!(s1, s1_after, "a moved carrier keeps the canonical row id");
    }

    #[test]
    fn inline_edit_identity_survives_carrier_replacement() {
        // Committed inline edits (alias, pin) ride on the row identity, so a
        // full live -> history -> live carrier cycle keeps them.
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("s1", "t", 1)]);
        let id = model.rows[0].row_id.clone();
        let alias_state = [(model.rows[0].identity.clone(), "renamed".into())];
        model.project_aliases(&alias_state);
        model.project_pins(&[model.rows[0].identity.clone()]);
        model.refresh(&[history("s1")], &[]);
        model.project_aliases(&alias_state);
        assert_eq!(model.rows[0].row_id, id);
        assert_eq!(model.rows[0].lifecycle, RowLifecycle::Virtual);
        model.refresh(&[], &[live("s1", "new-tab", 99)]);
        model.project_aliases(&alias_state);
        assert_eq!(model.rows[0].row_id, id);
        assert_eq!(model.rows[0].lifecycle, RowLifecycle::Live);
        assert_eq!(model.rows[0].alias.as_deref(), Some("renamed"));
        assert!(model.rows[0].pinned);
    }

    #[test]
    fn carrier_replacement_preserves_row_selection_pin_alias_and_order() {
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("s1", "t", 1), live("s2", "t", 2)]);
        let s1 = model
            .rows
            .iter()
            .find(|row| row.session_id.as_deref() == Some("s1"))
            .expect("s1 row")
            .row_id
            .clone();
        let order_before = model
            .rows
            .iter()
            .find(|row| row.row_id == s1)
            .expect("s1 row")
            .display_order;
        model.select(&s1);
        model.project_pins(&[model
            .rows
            .iter()
            .find(|row| row.row_id == s1)
            .unwrap()
            .identity
            .clone()]);
        let alias_state = [(model.rows[0].identity.clone(), "mine".into())];
        model.project_aliases(&alias_state);

        model.refresh(&[], &[live("s2", "t", 2), live("s1", "replaced", 9)]);
        model.project_aliases(&alias_state);
        let row = model
            .rows
            .iter()
            .find(|row| row.row_id == s1)
            .expect("s1 row survives carrier replacement");
        assert_eq!(model.selected(), Some(&s1), "selection survives");
        assert!(row.pinned, "pin survives");
        assert_eq!(row.alias.as_deref(), Some("mine"), "alias survives");
        assert_eq!(row.display_order, order_before, "display order survives");
        assert_eq!(
            row.carrier.as_ref().unwrap().tab_id.as_deref(),
            Some("replaced")
        );
    }

    #[test]
    fn metadata_failure_preserves_entity_rows_and_cached_enrichment() {
        // A refresh whose records lost their metadata (title/cwd) must keep
        // the entity row and the previously committed enrichment instead of
        // blanking it.
        let mut model = SessionNavigator::default();
        model.refresh(&[history("s1")], &[]);
        let id = model.rows[0].row_id.clone();
        assert_eq!(model.rows[0].title.as_deref(), Some("s1"));
        assert_eq!(model.rows[0].cwd.as_deref(), Some("/repo"));

        let mut degraded = history("s1");
        degraded.title = None;
        degraded.cwd = None;
        model.refresh(&[degraded], &[]);
        assert_eq!(model.rows.len(), 1, "the entity row is preserved");
        assert_eq!(model.rows[0].row_id, id);
        assert_eq!(
            model.rows[0].title.as_deref(),
            Some("s1"),
            "cached title survives a metadata failure"
        );
        assert_eq!(
            model.rows[0].cwd.as_deref(),
            Some("/repo"),
            "cached cwd survives a metadata failure"
        );
    }

    #[test]
    fn delete_plan_captures_source_and_identity_before_mutation() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("s1")], &[]);
        let id = model.rows[0].row_id.clone();
        let plan = model
            .plan_delete(&id)
            .expect("plan must exist for Virtual row");
        assert_eq!(plan.row_id, id);
        assert_eq!(plan.session_id.as_deref(), Some("s1"));
        assert_eq!(plan.agent, CLIAgent::Codex);
        // Plan must not mutate the model
        assert_eq!(model.rows.len(), 1);
        assert_eq!(model.rows[0].row_id, id);
    }

    #[test]
    fn commit_delete_removes_row_and_file_atomically() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("s1")], &[]);
        let id = model.rows[0].row_id.clone();
        let plan = model.plan_delete(&id).expect("plan exists");
        assert!(model.commit_delete(&plan));
        assert!(model.rows.is_empty());
        assert!(model.tombstones.contains(&id));
        // Committing again is idempotent
        assert!(!model.commit_delete(&plan));
    }

    #[test]
    fn rollback_delete_restores_row_on_file_failure() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("s1")], &[]);
        let id = model.rows[0].row_id.clone();
        let plan = model.plan_delete(&id).expect("plan exists");
        assert!(model.commit_delete(&plan));
        assert!(model.rows.is_empty());
        assert!(model.rollback_delete(&plan));
        assert_eq!(model.rows.len(), 1);
        assert_eq!(model.rows[0].row_id, id);
        assert!(!model.tombstones.contains(&id));
        // Rollback is idempotent
        assert!(!model.rollback_delete(&plan));
    }

    #[test]
    fn tombstone_prevents_reappearance_in_same_generation() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("s1")], &[]);
        let id = model.rows[0].row_id.clone();
        let plan = model.plan_delete(&id).expect("plan exists");
        assert!(model.commit_delete(&plan));
        // Same session identity must not reappear in the same generation
        model.refresh(&[history("s1")], &[]);
        assert!(model.rows.is_empty(), "tombstone blocks re-insertion");
    }

    #[test]
    fn plan_delete_rejects_live_and_restoring_rows() {
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("s1", "t", 1)]);
        let live_id = model.rows[0].row_id.clone();
        assert!(model.plan_delete(&live_id).is_none(), "live row rejected");
        model.refresh(&[history("s2")], &[]);
        let virt_id = model
            .rows
            .iter()
            .find(|r| r.session_id.as_deref() == Some("s2"))
            .unwrap()
            .row_id
            .clone();
        model.begin_restore(&virt_id);
        assert!(
            model.plan_delete(&virt_id).is_none(),
            "restoring row rejected"
        );
    }

    #[test]
    fn plan_close_and_delete_captures_live_row() {
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("s1", "t", 1)]);
        let live_id = model.rows[0].row_id.clone();
        let plan = model
            .plan_close_and_delete(&live_id)
            .expect("live close-and-delete plans");
        assert_eq!(plan.session_id.as_deref(), Some("s1"));
        assert_eq!(plan.row_id, live_id);
        assert!(model.plan_delete(&live_id).is_none());
    }

    #[test]
    fn plan_close_and_delete_rejects_historical_rows() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("s1")], &[]);
        let id = model.rows[0].row_id.clone();
        assert!(
            model.plan_close_and_delete(&id).is_none(),
            "historical rows use plan_delete, not plan_close_and_delete"
        );
        assert!(model.plan_delete(&id).is_some());
    }

    #[test]
    fn historical_delete_still_plans_provider_transaction() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("s1")], &[]);
        let id = model.rows[0].row_id.clone();
        let plan = model
            .plan_delete(&id)
            .expect("virtual historical rows still plan permanent delete");
        assert_eq!(plan.session_id.as_deref(), Some("s1"));
        assert_eq!(plan.row_id, id);
    }

    #[test]
    fn activate_selects_row_before_carrier_focus() {
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("s1", "t", 1)]);
        let id = model.rows[0].row_id.clone();
        assert!(model.select(&id));
        assert_eq!(model.selected(), Some(&id));
        model.refresh(&[], &[live("s1", "t2", 9)]);
        assert_eq!(model.selected(), Some(&id));
    }

    #[test]
    fn live_only_alias_is_immediately_visible() {
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("s1", "tab", 1)]);
        let aliases = [(model.rows[0].identity.clone(), "Immediate".into())];
        model.project_aliases(&aliases);
        assert_eq!(model.rows[0].alias.as_deref(), Some("Immediate"));
    }

    #[test]
    fn provider_source_appearance_preserves_explicit_alias() {
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("s1", "tab", 1)]);
        let row_id = model.rows[0].row_id.clone();
        let aliases = [(model.rows[0].identity.clone(), "Explicit".into())];
        model.project_aliases(&aliases);
        model.refresh(&[history("s1")], &[live("s1", "tab", 1)]);
        model.project_aliases(&aliases);
        assert_eq!(model.rows[0].row_id, row_id);
        assert_eq!(model.rows[0].alias.as_deref(), Some("Explicit"));
    }

    #[test]
    fn execution_badge_precedence_is_restoring_then_waiting_then_running_then_focused_live_then_background_live()
     {
        use crate::core::cli_agent::{AgentSessionState, AgentStatus};
        let mut state = AgentSessionState {
            status: AgentStatus::Waiting,
            message: Some("Approve Bash".into()),
            activity_seq: 4,
            ..Default::default()
        };
        assert_eq!(
            execution_badge(RowLifecycle::Restoring, Some(&state), true, true),
            Some(ExecutionBadge::Restoring)
        );
        assert_eq!(
            execution_badge(RowLifecycle::Live, Some(&state), true, true),
            Some(ExecutionBadge::Waiting)
        );
        state.status = AgentStatus::Working;
        assert_eq!(
            execution_badge(RowLifecycle::Live, Some(&state), true, false),
            Some(ExecutionBadge::Running)
        );
        state.status = AgentStatus::Idle;
        assert_eq!(
            execution_badge(RowLifecycle::Live, Some(&state), true, false),
            Some(ExecutionBadge::FocusedLive)
        );
        assert_eq!(
            execution_badge(RowLifecycle::Live, Some(&state), false, false),
            Some(ExecutionBadge::BackgroundLive)
        );
    }

    #[test]
    fn historical_rows_never_show_live_execution() {
        let state = crate::core::cli_agent::AgentSessionState {
            status: crate::core::cli_agent::AgentStatus::Working,
            activity_seq: 9,
            ..Default::default()
        };
        assert_eq!(
            execution_badge(RowLifecycle::Virtual, Some(&state), false, true),
            None
        );
    }

    #[test]
    fn stale_execution_event_cannot_replace_current_state() {
        let mut model = SessionNavigator::default();
        let mut fresh = live("s1", "tab", 1);
        fresh.execution = Some(LiveExecutionState {
            state: crate::core::cli_agent::AgentSessionState {
                status: crate::core::cli_agent::AgentStatus::Waiting,
                activity_seq: 8,
                ..Default::default()
            },
            focused: false,
            unread: false,
        });
        model.refresh(&[], &[fresh.clone()]);
        fresh.execution.as_mut().unwrap().state.activity_seq = 7;
        fresh.execution.as_mut().unwrap().state.status = crate::core::cli_agent::AgentStatus::Idle;
        model.refresh(&[], &[fresh]);
        assert_eq!(
            model.rows[0].execution.as_ref().unwrap().state.status,
            crate::core::cli_agent::AgentStatus::Waiting
        );
    }

    #[test]
    fn waiting_message_remains_actionable() {
        let state = crate::core::cli_agent::AgentSessionState {
            status: crate::core::cli_agent::AgentStatus::Waiting,
            message: Some("Confirm operation".into()),
            ..Default::default()
        };
        assert_eq!(execution_message(Some(&state)), Some("Confirm operation"));
    }

    #[test]
    fn hover_target_survives_carrier_replacement() {
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("s1", "tab-a", 1)]);
        let target = model.rows()[0].row_id.clone();
        model.refresh(&[], &[live("s1", "tab-b", 9)]);
        assert_eq!(
            model
                .detail_row(&target)
                .unwrap()
                .carrier
                .as_ref()
                .unwrap()
                .tab_id
                .as_deref(),
            Some("tab-b")
        );
    }

    #[test]
    fn stale_hover_target_closes_fail_closed() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("s1")], &[]);
        let target = model.rows()[0].row_id.clone();
        model.refresh(&[], &[]);
        assert!(model.detail_row(&target).is_none());
    }

    #[test]
    fn provider_identity_rebind_preserves_container_row() {
        let mut model = SessionNavigator::default();
        let mut session = live("s1", "tab", 1);
        session.identity = SessionIdentity::Durable("container-a".into());
        session.session_id = None;
        session.carrier.container_id = "container-a".into();
        model.refresh(&[], &[session.clone()]);
        let row_id = model.rows()[0].row_id.clone();
        model.project_pins(&[model.rows()[0].identity.clone()]);
        session.identity = SessionIdentity::Provider(history("s1").key);
        session.session_id = Some("s1".into());
        model.refresh(&[history("s1")], &[session]);
        model.project_pins(&[model.rows()[0].identity.clone()]);
        assert_eq!(model.rows().len(), 1);
        assert_eq!(model.rows()[0].row_id, row_id);
        assert!(model.rows()[0].pinned);
    }

    #[test]
    fn reorder_preserves_active_selection_and_row_identity() {
        let mut model = SessionNavigator::default();
        model.refresh(
            &[],
            &[
                live("s1", "tab-a", 1),
                live("s2", "tab-b", 2),
                live("s3", "tab-c", 3),
            ],
        );
        let original_ids: HashSet<_> = model.rows().iter().map(|row| row.row_id.clone()).collect();
        let selected = model.rows()[1].row_id.clone();
        assert!(model.select(&selected));
        let ordered: Vec<_> = model
            .rows()
            .iter()
            .rev()
            .map(|row| row.row_id.clone())
            .collect();

        let persisted = model.reorder(&ordered).expect("exact RowId permutation");

        assert_eq!(model.selected(), Some(&selected));
        assert_eq!(
            model
                .rows()
                .iter()
                .map(|row| &row.row_id)
                .collect::<Vec<_>>(),
            ordered.iter().collect::<Vec<_>>()
        );
        assert_eq!(
            model
                .rows()
                .iter()
                .map(|row| row.row_id.clone())
                .collect::<HashSet<_>>(),
            original_ids
        );
        assert_eq!(persisted.len(), 3);
    }

    #[test]
    fn split_siblings_move_as_one_reorder_unit() {
        let mut model = SessionNavigator::default();
        model.refresh(
            &[],
            &[
                live("split-a", "tab-split", 1),
                live("split-b", "tab-split", 2),
                live("single", "tab-single", 3),
            ],
        );
        let units = model.reorder_units();
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].row_ids.len(), 2);
        let split = units[0].row_ids.clone();
        let single = units[1].row_ids[0].clone();

        let interleaved = vec![split[0].clone(), single.clone(), split[1].clone()];
        assert!(model.reorder(&interleaved).is_err());

        let grouped = vec![single, split[0].clone(), split[1].clone()];
        model.reorder(&grouped).expect("whole split unit can move");
        assert_eq!(
            model
                .rows()
                .iter()
                .map(|row| &row.row_id)
                .collect::<Vec<_>>(),
            grouped.iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn newly_discovered_unit_is_inserted_at_top_without_rewriting_manual_order() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("older-a"), history("older-b")], &[]);
        let manual: Vec<_> = model
            .rows()
            .iter()
            .rev()
            .map(|row| row.row_id.clone())
            .collect();
        model.reorder(&manual).expect("manual order is valid");

        model.refresh(
            &[history("newest"), history("older-a"), history("older-b")],
            &[],
        );

        assert_eq!(
            model
                .rows()
                .iter()
                .map(|row| row.session_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["newest", "older-b", "older-a"]
        );
    }

    #[test]
    fn delete_preserves_survivor_relative_order() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("a"), history("b"), history("c")], &[]);
        let before: Vec<_> = model
            .rows()
            .iter()
            .map(|row| row.session_id.clone().unwrap())
            .collect();
        // Same refresh generation: lower first-seen insertion_index sorts first.
        assert_eq!(before, vec!["a", "b", "c"]);

        let b_id = model
            .rows()
            .iter()
            .find(|row| row.session_id.as_deref() == Some("b"))
            .map(|row| row.row_id.clone())
            .expect("b");
        let plan = model.plan_delete(&b_id).expect("historical delete plan");
        assert!(model.commit_delete(&plan));

        // Model a post-delete scan that briefly drops then rediscovers survivors
        // in shuffled order (Ashide: DeleteCommitted must not rewrite positions).
        model.refresh(&[], &[]);
        model.refresh(&[history("c"), history("a")], &[]);
        let after: Vec<_> = model
            .rows()
            .iter()
            .map(|row| row.session_id.clone().unwrap())
            .collect();
        assert_eq!(
            after,
            vec!["a", "c"],
            "delete + reshuffled refresh must keep first-seen relative order of survivors"
        );
    }

    #[test]
    fn refresh_reinsert_preserves_first_observed_insertion_order() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("a"), history("b")], &[]);
        let first_seen: Vec<_> = model
            .rows()
            .iter()
            .map(|row| row.session_id.clone().unwrap())
            .collect();
        assert_eq!(first_seen, vec!["a", "b"]);

        model.refresh(&[], &[]);
        assert!(model.rows().is_empty());

        // Reappear in reversed discovery order — first-seen relative order must win.
        model.refresh(&[history("b"), history("a")], &[]);
        let after: Vec<_> = model
            .rows()
            .iter()
            .map(|row| row.session_id.clone().unwrap())
            .collect();
        assert_eq!(
            after, first_seen,
            "RowId reinsert must reuse first-seen insertion keys, not treat rediscovery as new head units"
        );
    }

    #[test]
    fn session_display_title_prefers_alias_over_title_and_fallback() {
        assert_eq!(
            session_display_title(Some("Mine"), Some("Provider"), "Unnamed"),
            "Mine"
        );
        assert_eq!(
            session_display_title(None, Some("Provider"), "Unnamed"),
            "Provider"
        );
        assert_eq!(session_display_title(None, None, "Unnamed"), "Unnamed");
    }

    #[test]
    fn session_display_title_ignores_blank_candidates() {
        assert_eq!(
            session_display_title(Some("  "), Some("  First request "), "Unnamed"),
            "First request"
        );
        assert_eq!(
            session_display_title(Some("\t"), Some(""), "Unnamed"),
            "Unnamed"
        );
    }

    #[test]
    fn session_display_title_never_uses_session_id() {
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("019fa76a-6276-7b03-b302-c640686b2033", "t", 1)]);
        model.rows[0].title = None;
        let fallback = "Unnamed session";
        let display = model.rows[0].display_title(fallback);
        assert_eq!(display, fallback);
        assert_ne!(
            display,
            model.rows[0].session_id.as_deref().unwrap_or_default()
        );
    }

    #[test]
    fn refresh_blank_title_preserves_last_good_title_and_alias_display() {
        let mut model = SessionNavigator::default();
        model.refresh(&[], &[live("s1", "t", 1)]);
        model.rows[0].title = Some("Settled title".into());
        model.project_aliases(&[(model.rows[0].identity.clone(), "Local alias".into())]);
        let before = model.rows[0].display_title("Unnamed");

        let mut blank = live("s1", "t2", 9);
        blank.title = Some("   ".into());
        model.refresh(&[], &[blank]);
        model.project_aliases(&[(model.rows[0].identity.clone(), "Local alias".into())]);

        assert_eq!(model.rows[0].title.as_deref(), Some("Settled title"));
        assert_eq!(model.rows[0].alias.as_deref(), Some("Local alias"));
        assert_eq!(model.rows[0].display_title("Unnamed"), before);
        assert_eq!(before, "Local alias");
    }

    #[test]
    fn refresh_product_placeholder_title_preserves_settled_session_title() {
        let mut model = SessionNavigator::default();
        let mut hist = history("s1");
        hist.title = Some("First useful request".into());
        model.refresh(&[hist.clone()], &[]);
        let before = model.rows[0].display_title("Unnamed");
        assert_eq!(before, "First useful request");

        let mut polluted = live("s1", "t", 1);
        polluted.title = Some("agentty".into());
        model.refresh(&[hist.clone()], &[polluted.clone()]);
        assert_eq!(model.rows[0].display_title("Unnamed"), before);

        polluted.title = Some("agentty — disconnected".into());
        model.refresh(&[hist], &[polluted]);
        assert_eq!(model.rows[0].title.as_deref(), Some("First useful request"));
        assert_eq!(model.rows[0].display_title("Unnamed"), before);
    }

    #[test]
    fn catch_all_title_clears_so_first_user_message_can_name_the_row() {
        let mut model = SessionNavigator::default();
        let mut stuck = history("s1");
        stuck.title = Some("Agent 会话".into());
        model.refresh(&[stuck], &[]);
        assert!(
            model.rows[0].title.is_none(),
            "catch-all must not stick as the row title"
        );
        assert_eq!(model.rows[0].display_title("Agent 会话"), "Agent 会话");

        let mut named = history("s1");
        named.title = Some("看看这台机器有没有 grok".into());
        model.refresh(&[named], &[]);
        assert_eq!(
            model.rows[0].display_title("Agent 会话"),
            "看看这台机器有没有 grok"
        );
    }

    #[test]
    fn display_title_stable_across_identical_ui_inputs() {
        let mut model = SessionNavigator::default();
        model.refresh(&[history("s1")], &[]);
        model.project_aliases(&[(model.rows[0].identity.clone(), "Sticky".into())]);
        let first = model.rows[0].display_title("Unnamed");
        // Simulate UI churn: selection, hover identity, filter do not touch alias/title.
        model.select(&model.rows[0].row_id.clone());
        model.refresh(&[history("s1")], &[]);
        model.project_aliases(&[(model.rows[0].identity.clone(), "Sticky".into())]);
        let second = model.rows[0].display_title("Unnamed");
        assert_eq!(first, second);
        assert_eq!(first, "Sticky");
    }
}
