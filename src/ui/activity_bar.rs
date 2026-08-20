use gpui::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};

use crate::core::cli_agent::{AgentSessionState, AgentStatus};
use crate::ui::app::AgenttyApp;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityPresentation {
    pub title_key: &'static str,
    pub detail: Option<String>,
    /// Name of the most recent authoritative tool call, surfaced when the
    /// pane does not have a more urgent message (e.g. a permission prompt).
    pub tool: Option<String>,
    pub structured: bool,
    pub color: u32,
}

pub fn presentation(state: Option<&AgentSessionState>) -> ActivityPresentation {
    let Some(state) = state else {
        return ActivityPresentation {
            title_key: "activity.idle",
            detail: None,
            tool: None,
            structured: false,
            color: 0x64748B,
        };
    };
    let (title_key, color) = match state.status {
        AgentStatus::Idle => ("activity.idle", 0x64748B),
        AgentStatus::Working => ("activity.working", 0x3B82F6),
        AgentStatus::Waiting => ("activity.waiting", 0xF59E0B),
        AgentStatus::Done => ("activity.done", 0x22C55E),
    };
    // A waiting/attention message always wins; tool activity is context, not
    // a cover-up for something the user must act on.
    let tool = if state.message.is_some() {
        None
    } else {
        latest_tool_entry(state).and_then(|entry| entry.tool_name)
    };
    ActivityPresentation {
        title_key,
        detail: state.message.clone(),
        tool,
        structured: state.rich,
        color,
    }
}

pub(crate) struct ActivityEntryView {
    pub label_key: &'static str,
    pub detail: Option<String>,
    pub color: u32,
}

/// Map one structured activity entry to its presentation. Pure keys/colors
/// here; translation happens at render so the mapping stays unit-testable.
pub(crate) fn activity_entry_view(
    entry: &crate::core::cli_agent::AgentActivityEntry,
) -> ActivityEntryView {
    use crate::core::cli_agent::AgentEventKind as Kind;
    const BLUE: u32 = 0x3B82F6;
    const AMBER: u32 = 0xF59E0B;
    const GREEN: u32 = 0x22C55E;
    const GRAY: u32 = 0x6B7280;
    match entry.kind {
        Kind::SessionStart => ActivityEntryView {
            label_key: "activity.kind.session_start",
            detail: None,
            color: GREEN,
        },
        Kind::PromptSubmit => ActivityEntryView {
            label_key: "activity.kind.prompt_submit",
            detail: None,
            color: BLUE,
        },
        Kind::ToolComplete => ActivityEntryView {
            label_key: "activity.kind.tool_complete",
            detail: entry.tool_name.clone(),
            color: BLUE,
        },
        Kind::PermissionRequest | Kind::QuestionAsked => ActivityEntryView {
            label_key: "activity.kind.attention",
            detail: entry.message.clone(),
            color: AMBER,
        },
        Kind::Notification => ActivityEntryView {
            label_key: "activity.kind.notification",
            detail: entry.message.clone(),
            color: GRAY,
        },
        Kind::Stop | Kind::SessionEnd => ActivityEntryView {
            label_key: "activity.kind.finished",
            detail: entry.message.clone(),
            color: GREEN,
        },
    }
}

/// The most recent authoritative tool entry, if any — what the Activity Bar
/// surfaces as "what is the agent doing with tools right now".
pub(crate) fn latest_tool_entry(
    state: &crate::core::cli_agent::AgentSessionState,
) -> Option<crate::core::cli_agent::AgentActivityEntry> {
    state
        .recent_activity
        .iter()
        .rev()
        .find(|e| e.kind == crate::core::cli_agent::AgentEventKind::ToolComplete)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_activity_is_visibly_not_structured() {
        let state = AgentSessionState {
            status: AgentStatus::Working,
            rich: false,
            ..Default::default()
        };
        let view = presentation(Some(&state));
        assert!(!view.structured);
        assert_eq!(view.title_key, "activity.working");
    }

    #[test]
    fn latest_tool_entry_is_the_most_recent_tool_complete() {
        use crate::core::cli_agent::{AgentActivityEntry, AgentEventKind};
        let entry = |sequence, tool: Option<&str>| AgentActivityEntry {
            sequence,
            kind: AgentEventKind::ToolComplete,
            tool_name: tool.map(String::from),
            message: None,
        };
        let mut state = AgentSessionState::default();
        assert!(latest_tool_entry(&state).is_none());
        state.recent_activity.push_back(entry(1, Some("Bash")));
        state.recent_activity.push_back(AgentActivityEntry {
            sequence: 2,
            kind: AgentEventKind::Notification,
            tool_name: None,
            message: Some("note".into()),
        });
        state.recent_activity.push_back(entry(3, Some("Read")));
        let latest = latest_tool_entry(&state).unwrap();
        assert_eq!(latest.tool_name.as_deref(), Some("Read"));
    }

    #[test]
    fn tool_activity_surfaces_only_without_an_actionable_message() {
        use crate::core::cli_agent::{AgentActivityEntry, AgentEventKind};
        let mut state = AgentSessionState {
            status: AgentStatus::Working,
            rich: true,
            ..Default::default()
        };
        state.recent_activity.push_back(AgentActivityEntry {
            sequence: 1,
            kind: AgentEventKind::ToolComplete,
            tool_name: Some("Bash".into()),
            message: None,
        });
        let view = presentation(Some(&state));
        assert_eq!(view.tool.as_deref(), Some("Bash"));
        assert_eq!(view.detail, None);

        state.message = Some("Approve shell".into());
        state.status = AgentStatus::Waiting;
        let view = presentation(Some(&state));
        assert_eq!(view.tool, None, "permission prompt must not be covered up");
        assert_eq!(view.detail.as_deref(), Some("Approve shell"));
    }

    #[test]
    fn activity_entry_view_maps_every_kind() {
        use crate::core::cli_agent::{AgentActivityEntry, AgentEventKind};
        for kind in [
            AgentEventKind::SessionStart,
            AgentEventKind::PromptSubmit,
            AgentEventKind::ToolComplete,
            AgentEventKind::PermissionRequest,
            AgentEventKind::QuestionAsked,
            AgentEventKind::Notification,
            AgentEventKind::Stop,
            AgentEventKind::SessionEnd,
        ] {
            let view = activity_entry_view(&AgentActivityEntry {
                sequence: 1,
                kind,
                tool_name: None,
                message: None,
            });
            assert!(
                view.label_key.starts_with("activity.kind."),
                "{kind:?} has no presentation"
            );
        }
    }

    #[test]
    fn rich_activity_is_presented_as_structured() {
        let state = AgentSessionState {
            status: AgentStatus::Waiting,
            rich: true,
            message: Some("Approve shell".into()),
            ..Default::default()
        };
        let view = presentation(Some(&state));
        assert!(view.structured);
        assert_eq!(view.detail.as_deref(), Some("Approve shell"));
    }
}
