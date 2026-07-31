use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use super::{AgentSessionKey, OperationId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityTrust {
    Structured,
    Heuristic,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    OperationStarted,
    ToolCall { name: String },
    ToolResult { name: String, success: bool },
    Message,
    OperationFinished { success: bool },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentActivityEvent {
    pub sequence: u64,
    pub operation: OperationId,
    pub session: AgentSessionKey,
    pub provider: String,
    pub kind: ActivityKind,
    pub summary: Option<String>,
    pub trust: ActivityTrust,
}

impl AgentActivityEvent {
    pub fn is_authoritative_tool_activity(&self) -> bool {
        self.trust == ActivityTrust::Structured
            && matches!(
                self.kind,
                ActivityKind::ToolCall { .. } | ActivityKind::ToolResult { .. }
            )
    }
}

#[derive(Debug)]
pub struct ActivityStore {
    capacity_per_operation: usize,
    streams: HashMap<OperationId, VecDeque<AgentActivityEvent>>,
    last_sequence: HashMap<OperationId, u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityAppend {
    Appended,
    IgnoredStale,
    RejectedOverflow,
}

impl ActivityStore {
    pub fn new(capacity_per_operation: usize) -> Self {
        Self {
            capacity_per_operation,
            streams: HashMap::new(),
            last_sequence: HashMap::new(),
        }
    }

    pub fn append(&mut self, event: AgentActivityEvent) -> ActivityAppend {
        if self
            .last_sequence
            .get(&event.operation)
            .is_some_and(|last| event.sequence <= *last)
        {
            return ActivityAppend::IgnoredStale;
        }
        let stream = self.streams.entry(event.operation).or_default();
        if stream.len() >= self.capacity_per_operation {
            return ActivityAppend::RejectedOverflow;
        }
        self.last_sequence.insert(event.operation, event.sequence);
        stream.push_back(event);
        ActivityAppend::Appended
    }

    pub fn events(&self, operation: OperationId) -> impl Iterator<Item = &AgentActivityEvent> {
        self.streams.get(&operation).into_iter().flatten()
    }
}

pub fn structured_activity_from_agent_event(
    sequence: u64,
    operation: OperationId,
    provider: &str,
    event: &crate::core::cli_agent::AgentEvent,
) -> Option<AgentActivityEvent> {
    let session_id = event.session_id.as_ref()?.clone();
    let kind = match event.kind {
        crate::core::cli_agent::AgentEventKind::SessionStart
        | crate::core::cli_agent::AgentEventKind::PromptSubmit => ActivityKind::OperationStarted,
        crate::core::cli_agent::AgentEventKind::ToolComplete => ActivityKind::ToolResult {
            name: "tool".into(),
            success: true,
        },
        crate::core::cli_agent::AgentEventKind::PermissionRequest
        | crate::core::cli_agent::AgentEventKind::QuestionAsked
        | crate::core::cli_agent::AgentEventKind::Notification => ActivityKind::Message,
        crate::core::cli_agent::AgentEventKind::Stop => {
            ActivityKind::OperationFinished { success: true }
        }
        crate::core::cli_agent::AgentEventKind::SessionEnd => {
            ActivityKind::OperationFinished { success: true }
        }
    };
    Some(AgentActivityEvent {
        sequence,
        operation,
        session: AgentSessionKey {
            provider: provider.into(),
            session_id,
        },
        provider: provider.into(),
        kind,
        summary: event.message.clone(),
        trust: ActivityTrust::Structured,
    })
}

pub fn activity_from_pane_state(
    sequence: u64,
    operation: OperationId,
    pane: &crate::daemon::control::PaneAgentState,
) -> Option<AgentActivityEvent> {
    let agent = pane.agent?;
    let session_id = pane.state.session_id.clone()?;
    let kind = match pane.state.status {
        crate::core::cli_agent::AgentStatus::Idle => ActivityKind::Message,
        crate::core::cli_agent::AgentStatus::Working => ActivityKind::OperationStarted,
        crate::core::cli_agent::AgentStatus::Waiting => ActivityKind::Message,
        crate::core::cli_agent::AgentStatus::Done => {
            ActivityKind::OperationFinished { success: true }
        }
    };
    Some(AgentActivityEvent {
        sequence,
        operation,
        session: AgentSessionKey {
            provider: agent.slug().into(),
            session_id,
        },
        provider: agent.slug().into(),
        kind,
        summary: pane.state.message.clone(),
        trust: if pane.state.rich {
            ActivityTrust::Structured
        } else {
            ActivityTrust::Heuristic
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: u64, trust: ActivityTrust) -> AgentActivityEvent {
        AgentActivityEvent {
            sequence,
            operation: OperationId(1),
            session: AgentSessionKey {
                provider: "codex".into(),
                session_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            },
            provider: "codex".into(),
            kind: ActivityKind::ToolCall {
                name: "shell".into(),
            },
            summary: None,
            trust,
        }
    }

    #[test]
    fn terminal_heuristic_never_becomes_authoritative_tool_activity() {
        assert!(!event(1, ActivityTrust::Heuristic).is_authoritative_tool_activity());
        assert!(event(1, ActivityTrust::Structured).is_authoritative_tool_activity());
    }

    #[test]
    fn codex_and_claude_structured_events_share_one_schema() {
        for (agent, provider) in [
            (crate::core::cli_agent::CLIAgent::Codex, "codex"),
            (crate::core::cli_agent::CLIAgent::Claude, "claude"),
        ] {
            let event = crate::core::cli_agent::AgentEvent {
                agent: Some(agent),
                kind: crate::core::cli_agent::AgentEventKind::ToolComplete,
                session_id: Some("550e8400-e29b-41d4-a716-446655440000".into()),
                message: Some("completed".into()),
                cwd: None,
                tool_name: Some("shell".into()),
            };
            let activity =
                structured_activity_from_agent_event(1, OperationId(1), provider, &event).unwrap();
            assert_eq!(activity.provider, provider);
            assert!(activity.is_authoritative_tool_activity());
        }
    }

    #[test]
    fn activity_stream_is_ordered_and_bounded() {
        let mut store = ActivityStore::new(2);
        assert_eq!(
            store.append(event(1, ActivityTrust::Structured)),
            ActivityAppend::Appended
        );
        assert_eq!(
            store.append(event(1, ActivityTrust::Structured)),
            ActivityAppend::IgnoredStale
        );
        assert_eq!(
            store.append(event(2, ActivityTrust::Structured)),
            ActivityAppend::Appended
        );
        assert_eq!(
            store.append(event(3, ActivityTrust::Structured)),
            ActivityAppend::RejectedOverflow
        );
        assert_eq!(store.events(OperationId(1)).count(), 2);
    }
}
