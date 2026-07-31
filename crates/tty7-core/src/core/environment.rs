use serde::{Deserialize, Serialize};

use crate::core::session::RemoteTarget;

/// Stable product-level identity for an execution authority.
///
/// Unlike `WorkspaceId`, this identifies a machine/transport authority rather
/// than a daemon collection or a persisted tab set. A managed Environment can
/// therefore own one window containing many repositories and Agent sessions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EnvironmentId(String);

impl EnvironmentId {
    pub const LOCAL_KEY: &'static str = "local";

    pub fn local() -> Self {
        Self(Self::LOCAL_KEY.to_string())
    }

    pub fn for_remote(target: &RemoteTarget) -> Self {
        Self(target.connection_key())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_local(&self) -> bool {
        self.0 == Self::LOCAL_KEY
    }
}

impl Default for EnvironmentId {
    fn default() -> Self {
        Self::local()
    }
}

impl std::fmt::Display for EnvironmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for EnvironmentId {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentKind {
    Local,
    Ssh,
    Wsl,
    LocalStdio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Local,
    Connecting,
    Connected,
    Disconnected,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentDescriptor {
    pub id: EnvironmentId,
    pub kind: EnvironmentKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<RemoteTarget>,
}

impl EnvironmentDescriptor {
    pub fn local() -> Self {
        Self {
            id: EnvironmentId::local(),
            kind: EnvironmentKind::Local,
            label: "This Mac".to_string(),
            target: None,
        }
    }

    pub fn remote(target: RemoteTarget) -> Self {
        let kind = match &target {
            RemoteTarget::Profile { .. }
            | RemoteTarget::Alias { .. }
            | RemoteTarget::Direct { .. } => EnvironmentKind::Ssh,
            RemoteTarget::Wsl { .. } => EnvironmentKind::Wsl,
            RemoteTarget::LocalStdio { .. } => EnvironmentKind::LocalStdio,
        };
        Self {
            id: EnvironmentId::for_remote(&target),
            label: target.to_string(),
            kind,
            target: Some(target),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_id_round_trip() {
        let id = EnvironmentId::for_remote(&RemoteTarget::direct("me", "box.local", 2222));
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<EnvironmentId>(&json).unwrap(), id);
    }

    #[test]
    fn same_remote_target_has_stable_environment_identity() {
        let a = EnvironmentDescriptor::remote(RemoteTarget::direct("me", "BOX.local", 22));
        let b = EnvironmentDescriptor::remote(RemoteTarget::direct("me", "box.LOCAL", 22));
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn different_remote_targets_do_not_share_environment_identity() {
        let a = EnvironmentId::for_remote(&RemoteTarget::direct("me", "box.local", 22));
        let b = EnvironmentId::for_remote(&RemoteTarget::direct("me", "box.local", 2222));
        assert_ne!(a, b);
        assert_ne!(a, EnvironmentId::local());
    }

    #[test]
    fn descriptors_classify_remote_transports() {
        assert_eq!(
            EnvironmentDescriptor::remote(RemoteTarget::Alias {
                alias: "build".into(),
            })
            .kind,
            EnvironmentKind::Ssh
        );
        assert_eq!(
            EnvironmentDescriptor::remote(RemoteTarget::Wsl {
                distro: "Ubuntu".into(),
            })
            .kind,
            EnvironmentKind::Wsl
        );
    }
}
