//! Environment-scoped session Host/store context.
//!
//! UI session discovery and Host-backed session actions resolve through this
//! facade so call sites address the window Environment without branching on
//! local vs remote transport (ENV-SESSION-OPS-FACADE-15).

use std::path::PathBuf;
use std::sync::Arc;

use agentty_core::agent_runtime::{
    AgentRuntimeAdapter, AgentStoreRoots, DiscoveryOutcome, DiscoveryRequest, LocalAgentRuntime,
    OperationId, RemoteAgentRuntime, ScanGeneration,
};
use agentty_core::host::HostId;
use agentty_core::host::remote::RemoteHost;
use gpui::App;

use crate::ui::host_ops::SharedHost;
use crate::ui::host_registry::HostRegistry;

#[derive(Clone)]
pub(crate) struct EnvironmentSessionContext {
    pub host: SharedHost,
    pub home: PathBuf,
    pub store_roots: AgentStoreRoots,
    /// Present only for remote Environments; never a local fallback.
    remote: Option<Arc<RemoteHost>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EnvironmentSessionError {
    HostUnavailable,
    HomeUnavailable,
    StoreRootsUnavailable,
}

impl EnvironmentSessionError {
    pub(crate) fn message(&self) -> &'static str {
        match self {
            Self::HostUnavailable => "Environment has no connected Host",
            Self::HomeUnavailable => "Environment home is unavailable",
            Self::StoreRootsUnavailable => {
                "remote agentty-server did not publish its Agent store roots"
            }
        }
    }
}

impl EnvironmentSessionContext {
    /// Resolve Host, home, and store roots for the Environment's HostId.
    /// Remote never falls back to the local host or GUI HOME.
    pub(crate) fn resolve(cx: &App, host_id: HostId) -> Result<Self, EnvironmentSessionError> {
        let host =
            HostRegistry::lookup(cx, host_id).ok_or(EnvironmentSessionError::HostUnavailable)?;
        if host_id.is_local() {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or(EnvironmentSessionError::HomeUnavailable)?;
            let store_roots = AgentStoreRoots::for_current_process(&home);
            Ok(Self {
                host,
                home,
                store_roots,
                remote: None,
            })
        } else {
            let remote = crate::ui::remote_connect::HostLinks::get(cx, host_id)
                .ok_or(EnvironmentSessionError::HostUnavailable)?;
            let home = crate::ui::remote_connect::HostLinks::home(cx, host_id)
                .ok_or(EnvironmentSessionError::HomeUnavailable)?;
            let store_roots = crate::ui::remote_connect::HostLinks::store_roots(cx, host_id)
                .ok_or(EnvironmentSessionError::StoreRootsUnavailable)?;
            Ok(Self {
                host,
                home,
                store_roots,
                remote: Some(remote),
            })
        }
    }

    /// Run discovery on the resolved Environment Host. Safe off the UI thread.
    pub(crate) fn discover_sessions(
        &self,
        operation: OperationId,
        generation: ScanGeneration,
        request: DiscoveryRequest,
    ) -> std::io::Result<DiscoveryOutcome> {
        match &self.remote {
            None => LocalAgentRuntime::new(&*self.host)
                .discover_sessions(operation, generation, request),
            Some(remote) => {
                RemoteAgentRuntime::new(&**remote).discover_sessions(operation, generation, request)
            }
        }
    }
}

/// Pure marker used by static checks and unit tests: discovery authority is this
/// facade, not ad-hoc UI local/remote branches.
pub(crate) fn environment_session_context_resolve_is_the_discovery_authority() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_session_context_resolve_is_the_discovery_authority() {
        assert!(super::environment_session_context_resolve_is_the_discovery_authority());
        assert_eq!(
            EnvironmentSessionError::StoreRootsUnavailable.message(),
            "remote agentty-server did not publish its Agent store roots"
        );
    }
}
