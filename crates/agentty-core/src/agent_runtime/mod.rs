pub mod activity;
pub mod adapter;
pub mod completion;
pub mod delete;
pub mod discovery;
pub mod navigator;
pub mod operation;
pub mod parse;
pub mod provider;
pub mod resume;
pub mod service;
pub mod stores;
pub mod user_state;

pub use activity::{
    ActivityAppend, ActivityKind, ActivityStore, ActivityTrust, AgentActivityEvent,
    activity_from_pane_state, structured_activity_from_agent_event,
};
pub use adapter::{AgentRuntimeAdapter, LocalAgentRuntime, RemoteAgentClient, RemoteAgentRuntime};
pub use completion::{
    CompletionCandidate, CompletionCommit, CompletionGeneration, CompletionOutcome,
    CompletionReducer, CompletionRequest, CompletionSourceKind, ReplacementRange, complete,
    replacement_range,
};
pub use delete::{
    SessionDeleteSource, apply_session_delete_source, apply_session_delete_transaction,
    apply_session_user_state_delete, plan_close_and_delete_source, plan_session_delete_source,
};
pub use discovery::{
    AgentSessionKey, AgentSessionRecord, AuthorityKind, DiscoveryCommit, DiscoveryOutcome,
    DiscoveryReducer, OperationId, ScanGeneration,
};
pub use navigator::{
    ExecutionBadge, LiveCarrier, LiveExecutionState, LiveSession, NavigatorRow, NavigatorRowId,
    RestoreOutcome, RowLifecycle, SessionIdentity, SessionNavigator, SessionReorderUnit,
    execution_badge, execution_message, session_display_title,
};
pub use operation::{BoundedBatch, OperationLimits, OperationRegistry};
pub use parse::first_user_title_candidate;
pub use provider::{
    PERSISTED_PROVIDER_DESCRIPTORS, ProviderDescriptor, ProviderId, ProviderScanner,
    descriptor_for_agent, descriptor_for_id,
};
pub use resume::{ResumeInvocation, shell_command, shell_line};
pub use service::{DiscoveryRequest, discover};
pub use stores::AgentStoreRoots;
pub use user_state::{AliasError, SessionUserStateStore};

pub const HELPER_PROTOCOL_VERSION: u32 = 2;

pub fn helper_binary_hash() -> String {
    option_env!("AGENTTY_HELPER_BINARY_HASH")
        .unwrap_or(env!("CARGO_PKG_VERSION"))
        .to_string()
}

pub mod capability {
    pub const SESSION_DISCOVERY: &str = "agent.session-discovery.v1";
    pub const TYPED_RESUME: &str = "agent.typed-resume.v1";
    pub const STRUCTURED_ACTIVITY: &str = "agent.structured-activity.v1";
    pub const COMPLETION: &str = "agent.completion.v1";
    pub const CANCELLATION: &str = "agent.cancellation.v1";
}

pub fn capabilities() -> Vec<String> {
    vec![
        capability::SESSION_DISCOVERY.to_string(),
        capability::TYPED_RESUME.to_string(),
        capability::STRUCTURED_ACTIVITY.to_string(),
        capability::COMPLETION.to_string(),
        capability::CANCELLATION.to_string(),
    ]
}
