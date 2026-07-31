use std::collections::BTreeMap;
use std::fmt;

use super::discovery::{AgentSessionRecord, DiscoveryOutcome};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn new(value: impl Into<String>) -> Result<Self, RegistryError> {
        let value = value.into();
        if value.is_empty()
            || !value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(RegistryError::InvalidId(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub trait AgentSessionProvider: Send + Sync {
    fn id(&self) -> &ProviderId;
    fn discover(&self) -> DiscoveryOutcome;
}

#[derive(Debug, PartialEq, Eq)]
pub enum RegistryError {
    InvalidId(String),
    Duplicate(String),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(f, "invalid provider id: {id}"),
            Self::Duplicate(id) => write!(f, "duplicate provider id: {id}"),
        }
    }
}

impl std::error::Error for RegistryError {}

#[derive(Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<ProviderId, Box<dyn AgentSessionProvider>>,
}

impl ProviderRegistry {
    pub fn register(
        &mut self,
        provider: Box<dyn AgentSessionProvider>,
    ) -> Result<(), RegistryError> {
        let id = provider.id().clone();
        if self.providers.contains_key(&id) {
            return Err(RegistryError::Duplicate(id.0));
        }
        self.providers.insert(id, provider);
        Ok(())
    }

    pub fn provider_ids(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(ProviderId::as_str)
    }

    pub fn discover_all(&self) -> DiscoveryOutcome {
        let mut rows: Vec<AgentSessionRecord> = Vec::new();
        let mut failed = Vec::new();
        for (id, provider) in &self.providers {
            match provider.discover() {
                DiscoveryOutcome::Complete(mut discovered) => rows.append(&mut discovered),
                _ => failed.push(id.as_str().to_string()),
            }
        }
        if failed.is_empty() {
            DiscoveryOutcome::Complete(rows)
        } else {
            DiscoveryOutcome::Partial {
                failed_providers: failed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Empty(ProviderId);
    impl AgentSessionProvider for Empty {
        fn id(&self) -> &ProviderId {
            &self.0
        }
        fn discover(&self) -> DiscoveryOutcome {
            DiscoveryOutcome::Complete(vec![])
        }
    }

    #[test]
    fn registry_rejects_duplicate_provider_ids() {
        let mut registry = ProviderRegistry::default();
        registry
            .register(Box::new(Empty(ProviderId::new("codex").unwrap())))
            .unwrap();
        assert_eq!(
            registry.register(Box::new(Empty(ProviderId::new("codex").unwrap()))),
            Err(RegistryError::Duplicate("codex".into()))
        );
    }
}
