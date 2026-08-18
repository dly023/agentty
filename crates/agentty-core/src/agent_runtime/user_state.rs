use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::environment::EnvironmentId;
use crate::host::Host;

use super::navigator::SessionIdentity;
use super::stores::AgentStoreRoots;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUserStateStore {
    aliases: HashMap<String, String>,
    #[serde(default)]
    pins: std::collections::HashSet<String>,
    #[serde(default)]
    display_order: HashMap<String, u64>,
}

impl SessionUserStateStore {
    /// Resolve the target-owned alias/pin/order file from the same immutable
    /// roots snapshot used by discovery and hooks.
    pub fn path_from_roots(host: &dyn Host, roots: &AgentStoreRoots) -> PathBuf {
        host.join(&roots.agentty_config_dir, "session-aliases.json")
    }

    /// Legacy home-based wrapper retained for old callers and persisted
    /// clients. New Environment-scoped callers must pass the target snapshot
    /// through [`Self::path_from_roots`].
    pub fn path(host: &dyn Host, home: &Path) -> std::path::PathBuf {
        let roots = AgentStoreRoots::for_home(home.to_path_buf());
        Self::path_from_roots(host, &roots)
    }

    pub fn alias<'a>(
        &'a self,
        environment: &EnvironmentId,
        identity: &SessionIdentity,
    ) -> Option<&'a str> {
        self.aliases
            .get(&alias_key(environment, identity))
            .map(String::as_str)
    }

    pub fn set(
        &mut self,
        environment: EnvironmentId,
        identity: SessionIdentity,
        alias: Option<String>,
    ) -> Result<(), AliasError> {
        let key = alias_key(&environment, &identity);
        let normalized = alias
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if let Some(alias) = normalized {
            let prefix = format!("{}\u{1f}", environment.as_str());
            if self.aliases.iter().any(|(other_key, other_alias)| {
                other_key != &key && other_key.starts_with(&prefix) && other_alias == &alias
            }) {
                return Err(AliasError::Conflict(alias));
            }
            self.aliases.insert(key, alias);
        } else {
            self.aliases.remove(&key);
        }
        Ok(())
    }

    pub fn delete(&mut self, environment: &EnvironmentId, identity: &SessionIdentity) -> bool {
        let key = alias_key(environment, identity);
        let alias = self.aliases.remove(&key).is_some();
        let pin = self.pins.remove(&key);
        let order = self.display_order.remove(&key).is_some();
        alias || pin || order
    }

    pub fn is_pinned(&self, environment: &EnvironmentId, identity: &SessionIdentity) -> bool {
        self.pins.contains(&alias_key(environment, identity))
    }

    pub fn set_pin(&mut self, environment: EnvironmentId, identity: SessionIdentity, pinned: bool) {
        let key = alias_key(&environment, &identity);
        if pinned {
            self.pins.insert(key);
        } else {
            self.pins.remove(&key);
        }
    }

    pub fn pins_for_environment(&self, environment: &EnvironmentId) -> Vec<SessionIdentity> {
        let prefix = format!("{}\u{1f}", environment.as_str());
        self.pins
            .iter()
            .filter_map(|key| parse_identity(key.strip_prefix(&prefix)?))
            .collect()
    }

    pub fn display_order(
        &self,
        environment: &EnvironmentId,
        identity: &SessionIdentity,
    ) -> Option<u64> {
        self.display_order
            .get(&alias_key(environment, identity))
            .copied()
    }

    pub fn display_orders_for_environment(
        &self,
        environment: &EnvironmentId,
    ) -> Vec<(SessionIdentity, u64)> {
        let prefix = format!("{}\u{1f}", environment.as_str());
        self.display_order
            .iter()
            .filter_map(|(key, order)| Some((parse_identity(key.strip_prefix(&prefix)?)?, *order)))
            .collect()
    }

    pub fn replace_display_order(
        &mut self,
        environment: EnvironmentId,
        orders: Vec<(SessionIdentity, u64)>,
    ) {
        let prefix = format!("{}\u{1f}", environment.as_str());
        self.display_order
            .retain(|key, _| !key.starts_with(&prefix));
        self.display_order.extend(
            orders
                .into_iter()
                .map(|(identity, order)| (alias_key(&environment, &identity), order)),
        );
    }

    pub fn rebind_identity(
        &mut self,
        environment: &EnvironmentId,
        from: &SessionIdentity,
        to: &SessionIdentity,
    ) -> bool {
        if from == to {
            return false;
        }
        let from_key = alias_key(environment, from);
        let to_key = alias_key(environment, to);
        let mut changed = false;
        if !self.aliases.contains_key(&to_key)
            && let Some(alias) = self.aliases.remove(&from_key)
        {
            self.aliases.insert(to_key.clone(), alias);
            changed = true;
        } else {
            changed |= self.aliases.remove(&from_key).is_some();
        }
        if self.pins.remove(&from_key) {
            self.pins.insert(to_key.clone());
            changed = true;
        }
        if !self.display_order.contains_key(&to_key)
            && let Some(order) = self.display_order.remove(&from_key)
        {
            self.display_order.insert(to_key.clone(), order);
            changed = true;
        } else {
            changed |= self.display_order.remove(&from_key).is_some();
        }
        changed
    }

    pub fn load(host: &dyn Host, path: &Path) -> io::Result<Self> {
        let bytes = match host.read_file(path, 4 * 1024 * 1024) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid session user state store: {error}"),
            )
        })
    }

    pub fn aliases_for_environment(
        &self,
        environment: &EnvironmentId,
    ) -> Vec<(SessionIdentity, String)> {
        let prefix = format!("{}\u{1f}", environment.as_str());
        self.aliases
            .iter()
            .filter_map(|(key, alias)| {
                parse_identity(key.strip_prefix(&prefix)?).map(|identity| (identity, alias.clone()))
            })
            .collect()
    }

    pub fn save(&self, host: &dyn Host, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cannot serialize session alias store: {error}"),
            )
        })?;
        if let Some(parent) = path.parent() {
            host.create_dir(parent, true)?;
        }
        host.write_file(path, &bytes).map(|_| ())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AliasError {
    Conflict(String),
}

impl std::fmt::Display for AliasError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AliasError::Conflict(alias) => {
                write!(formatter, "another session already uses alias `{alias}`")
            }
        }
    }
}

impl std::error::Error for AliasError {}

fn alias_key(environment: &EnvironmentId, identity: &SessionIdentity) -> String {
    let identity = match identity {
        SessionIdentity::Provider(key) => format!("provider:{}:{}", key.provider, key.session_id),
        SessionIdentity::Durable(key) => format!("durable:{key}"),
    };
    format!("{}\u{1f}{identity}", environment.as_str())
}

fn parse_identity(value: &str) -> Option<SessionIdentity> {
    if let Some(rest) = value.strip_prefix("provider:") {
        let (provider, session_id) = rest.split_once(':')?;
        return Some(SessionIdentity::Provider(super::AgentSessionKey {
            provider: provider.to_owned(),
            session_id: session_id.to_owned(),
        }));
    }
    value
        .strip_prefix("durable:")
        .map(|key| SessionIdentity::Durable(key.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::AgentSessionKey;
    use crate::host::local::LocalHost;

    fn identity() -> SessionIdentity {
        SessionIdentity::Provider(AgentSessionKey {
            provider: "codex".into(),
            session_id: "session-1".into(),
        })
    }

    #[test]
    fn alias_state_is_partitioned_by_environment_identity() {
        let mut store = SessionUserStateStore::default();
        let local = EnvironmentId::local();
        let remote: EnvironmentId = "ssh:build".parse().unwrap();
        store
            .set(local.clone(), identity(), Some("Local work".into()))
            .unwrap();
        store
            .set(remote.clone(), identity(), Some("Remote work".into()))
            .unwrap();
        assert_eq!(store.alias(&local, &identity()), Some("Local work"));
        assert_eq!(store.alias(&remote, &identity()), Some("Remote work"));
    }

    #[test]
    fn alias_mutation_failure_rolls_back_visible_state() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session-user-state.json");
        let host = LocalHost::new();
        let mut store = SessionUserStateStore::default();
        let env = EnvironmentId::local();
        store
            .set(env.clone(), identity(), Some("Before".into()))
            .unwrap();
        store.save(&*host, &path).unwrap();
        let mut candidate = store.clone();
        candidate
            .set(env.clone(), identity(), Some("After".into()))
            .unwrap();
        let blocked_parent = temp.path().join("not-a-directory");
        std::fs::write(&blocked_parent, b"blocks create_dir_all").unwrap();
        let impossible = blocked_parent.join("session-user-state.json");
        assert!(candidate.save(&*host, &impossible).is_err());
        assert_eq!(store.alias(&env, &identity()), Some("Before"));
    }

    #[test]
    fn deleted_session_clears_alias_state() {
        let mut store = SessionUserStateStore::default();
        let env = EnvironmentId::local();
        store
            .set(env.clone(), identity(), Some("Work".into()))
            .unwrap();
        assert!(store.delete(&env, &identity()));
        assert_eq!(store.alias(&env, &identity()), None);
    }

    #[test]
    fn session_alias_store_round_trips_through_selected_host() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session-user-state.json");
        let host = LocalHost::new();
        let env = EnvironmentId::local();
        let mut store = SessionUserStateStore::default();
        store
            .set(env.clone(), identity(), Some("  Focused work  ".into()))
            .unwrap();
        store.save(&*host, &path).unwrap();
        let loaded = SessionUserStateStore::load(&*host, &path).unwrap();
        assert_eq!(loaded.alias(&env, &identity()), Some("Focused work"));
    }

    #[test]
    fn session_alias_store_path_uses_selected_host_home() {
        let host = LocalHost::new();
        assert_eq!(
            SessionUserStateStore::path(&*host, Path::new("/remote/home")),
            Path::new("/remote/home/.config/agentty/session-aliases.json")
        );
    }

    #[test]
    fn session_alias_store_path_uses_published_agentty_config_root() {
        let host = LocalHost::new();
        let mut roots = AgentStoreRoots::for_home(PathBuf::from("/remote/home"));
        roots.agentty_config_dir = PathBuf::from("/srv/agentty-config");
        assert_eq!(
            SessionUserStateStore::path_from_roots(&*host, &roots),
            Path::new("/srv/agentty-config/session-aliases.json")
        );
    }

    #[test]
    fn pin_survives_live_history_replacement_and_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session-user-state.json");
        let host = LocalHost::new();
        let env = EnvironmentId::local();
        let mut store = SessionUserStateStore::default();
        store.set_pin(env.clone(), identity(), true);
        store.save(&*host, &path).unwrap();
        let loaded = SessionUserStateStore::load(&*host, &path).unwrap();
        assert!(loaded.is_pinned(&env, &identity()));
    }

    #[test]
    fn failed_remote_pin_rolls_back_without_local_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let host = LocalHost::new();
        let env: EnvironmentId = "ssh:build".parse().unwrap();
        let store = SessionUserStateStore::default();
        let mut candidate = store.clone();
        candidate.set_pin(env.clone(), identity(), true);
        let blocked_parent = temp.path().join("not-a-directory");
        std::fs::write(&blocked_parent, b"blocks create_dir_all").unwrap();
        assert!(
            candidate
                .save(&*host, &blocked_parent.join("state.json"))
                .is_err()
        );
        assert!(!store.is_pinned(&env, &identity()));
    }

    #[test]
    fn deleted_session_clears_pin_and_alias_state() {
        let env = EnvironmentId::local();
        let mut store = SessionUserStateStore::default();
        store
            .set(env.clone(), identity(), Some("Work".into()))
            .unwrap();
        store.set_pin(env.clone(), identity(), true);
        assert!(store.delete(&env, &identity()));
        assert_eq!(store.alias(&env, &identity()), None);
        assert!(!store.is_pinned(&env, &identity()));
    }

    #[test]
    fn pin_and_alias_rebind_from_live_container_to_provider_identity() {
        let env = EnvironmentId::local();
        let live = SessionIdentity::Durable("container-a".into());
        let provider = identity();
        let mut store = SessionUserStateStore::default();
        store
            .set(env.clone(), live.clone(), Some("Live work".into()))
            .unwrap();
        store.set_pin(env.clone(), live.clone(), true);
        assert!(store.rebind_identity(&env, &live, &provider));
        assert_eq!(store.alias(&env, &provider), Some("Live work"));
        assert!(store.is_pinned(&env, &provider));
        assert_eq!(store.alias(&env, &live), None);
        assert!(!store.is_pinned(&env, &live));
    }

    #[test]
    fn failed_reorder_persistence_preserves_visible_order() {
        let temp = tempfile::tempdir().unwrap();
        let host = LocalHost::new();
        let env: EnvironmentId = "ssh:build".parse().unwrap();
        let first = identity();
        let second = SessionIdentity::Durable("container-b".into());
        let mut store = SessionUserStateStore::default();
        store.replace_display_order(env.clone(), vec![(first.clone(), 0), (second.clone(), 1)]);
        let mut candidate = store.clone();
        candidate.replace_display_order(env.clone(), vec![(second.clone(), 0), (first.clone(), 1)]);
        let blocked_parent = temp.path().join("not-a-directory");
        std::fs::write(&blocked_parent, b"file").unwrap();

        assert!(
            candidate
                .save(&*host, &blocked_parent.join("state.json"))
                .is_err()
        );
        assert_eq!(store.display_order(&env, &first), Some(0));
        assert_eq!(store.display_order(&env, &second), Some(1));
    }
}
