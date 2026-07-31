pub mod activity;
pub mod adapter;
pub mod completion;
pub mod discovery;
pub mod navigator;
pub mod operation;
pub mod parse;
pub mod provider;
pub mod resume;
pub mod service;
pub mod stores;

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
pub use discovery::{
    AgentSessionKey, AgentSessionRecord, AuthorityKind, DiscoveryCommit, DiscoveryOutcome,
    DiscoveryReducer, OperationId, ScanGeneration,
};
pub use navigator::{
    LiveCarrier, LiveSession, NavigatorRow, NavigatorRowId, RestoreOutcome, RowLifecycle,
    SessionIdentity, SessionNavigator,
};
pub use operation::{BoundedBatch, OperationLimits, OperationRegistry};
pub use provider::{AgentSessionProvider, ProviderId, ProviderRegistry, RegistryError};
pub use resume::{ResumeInvocation, shell_line};
pub use service::{DiscoveryRequest, discover};
pub use stores::AgentStoreRoots;

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
