use std::io;

use crate::daemon::control::{ControlRequest, ReplyOk, feature};
use crate::host::Host;
use crate::host::remote::RemoteHost;

use super::{
    AgentActivityEvent, AuthorityKind, CompletionOutcome, CompletionRequest, DiscoveryOutcome,
    DiscoveryRequest, OperationId, ScanGeneration,
};

pub trait AgentRuntimeAdapter {
    fn activity(&self, operation: OperationId, limit: usize)
    -> io::Result<Vec<AgentActivityEvent>>;

    fn complete(&self, request: CompletionRequest) -> io::Result<CompletionOutcome>;

    fn discover_sessions(
        &self,
        operation: OperationId,
        generation: ScanGeneration,
        request: DiscoveryRequest,
    ) -> io::Result<DiscoveryOutcome>;
}

pub struct LocalAgentRuntime<'a> {
    host: &'a dyn Host,
}

impl<'a> LocalAgentRuntime<'a> {
    pub fn new(host: &'a dyn Host) -> Self {
        Self { host }
    }
}

impl AgentRuntimeAdapter for LocalAgentRuntime<'_> {
    fn activity(
        &self,
        _operation: OperationId,
        _limit: usize,
    ) -> io::Result<Vec<AgentActivityEvent>> {
        Ok(Vec::new())
    }

    fn complete(&self, request: CompletionRequest) -> io::Result<CompletionOutcome> {
        Ok(super::complete(self.host, &request))
    }

    fn discover_sessions(
        &self,
        _operation: OperationId,
        _generation: ScanGeneration,
        request: DiscoveryRequest,
    ) -> io::Result<DiscoveryOutcome> {
        Ok(super::discover(self.host, &request))
    }
}

pub trait RemoteAgentClient {
    fn activity_remote(
        &self,
        operation: OperationId,
        limit: usize,
    ) -> io::Result<Vec<AgentActivityEvent>>;
    fn complete_remote(&self, request: CompletionRequest) -> io::Result<CompletionOutcome>;
    fn helper_available(&self) -> bool;
    /// True when DiscoverAgentSessions may be called. Prefer the agent-helper
    /// feature; fall back to HelloOk store_roots for peers that publish roots
    /// but omit the later feature flag (pre-flag remotes still speak discovery).
    fn discovery_available(&self) -> bool {
        self.helper_available()
    }
    fn discover_remote(
        &self,
        operation: OperationId,
        generation: ScanGeneration,
        request: DiscoveryRequest,
    ) -> io::Result<DiscoveryOutcome>;
}

impl RemoteAgentClient for RemoteHost {
    fn activity_remote(
        &self,
        operation: OperationId,
        limit: usize,
    ) -> io::Result<Vec<AgentActivityEvent>> {
        match self.client().call(ControlRequest::AgentActivity {
            operation,
            limit: limit.try_into().unwrap_or(u64::MAX),
        })? {
            ReplyOk::AgentActivity(events) => Ok(events),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("remote helper returned {other:?} for activity"),
            )),
        }
    }

    fn complete_remote(&self, request: CompletionRequest) -> io::Result<CompletionOutcome> {
        match self
            .client()
            .call(ControlRequest::CompleteAgentInput { request })?
        {
            ReplyOk::AgentCompletion(outcome) => Ok(outcome),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("remote helper returned {other:?} for completion"),
            )),
        }
    }

    fn helper_available(&self) -> bool {
        self.peer().has_feature(feature::AGENT_HELPER)
    }

    /// Session discovery is available when the peer advertises agent-helper, or
    /// when target-owned store roots resolve from HelloOk (published roots or
    /// non-empty peer home). Older remotes speak DiscoverAgentSessions but omit
    /// the later agent-helper feature flag and/or the store_roots Hello field.
    fn discovery_available(&self) -> bool {
        self.helper_available() || self.store_roots().is_some()
    }

    fn discover_remote(
        &self,
        operation: OperationId,
        generation: ScanGeneration,
        request: DiscoveryRequest,
    ) -> io::Result<DiscoveryOutcome> {
        match self.client().call(ControlRequest::DiscoverAgentSessions {
            operation,
            generation,
            authority: AuthorityKind::Remote,
            roots: request.roots,
            providers: request.providers,
            logical_limit: request.logical_limit.try_into().unwrap_or(u64::MAX),
            physical_source_limit: request.physical_source_limit.try_into().unwrap_or(u64::MAX),
        })? {
            ReplyOk::AgentSessionDiscovery(outcome) => Ok(outcome),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("remote helper returned {other:?} for session discovery"),
            )),
        }
    }
}

pub struct RemoteAgentRuntime<'a> {
    client: &'a dyn RemoteAgentClient,
}

impl<'a> RemoteAgentRuntime<'a> {
    pub fn new(client: &'a dyn RemoteAgentClient) -> Self {
        Self { client }
    }

    pub fn is_available(&self) -> bool {
        self.client.helper_available()
    }

    pub fn discovery_available(&self) -> bool {
        self.client.discovery_available()
    }
}

impl AgentRuntimeAdapter for RemoteAgentRuntime<'_> {
    fn activity(
        &self,
        operation: OperationId,
        limit: usize,
    ) -> io::Result<Vec<AgentActivityEvent>> {
        if !self.is_available() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "remote agentty-server does not advertise agent-helper",
            ));
        }
        self.client.activity_remote(operation, limit)
    }

    fn complete(&self, request: CompletionRequest) -> io::Result<CompletionOutcome> {
        if !self.is_available() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "remote agentty-server does not advertise agent-helper",
            ));
        }
        self.client.complete_remote(request)
    }

    fn discover_sessions(
        &self,
        operation: OperationId,
        generation: ScanGeneration,
        request: DiscoveryRequest,
    ) -> io::Result<DiscoveryOutcome> {
        if !self.discovery_available() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "remote agentty-server does not advertise session discovery \
                 (missing agent-helper and Agent store roots); update the remote server",
            ));
        }
        self.client.discover_remote(operation, generation, request)
    }
}
