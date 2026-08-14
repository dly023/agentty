use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::cli_agent::CLIAgent;

use super::stores::AgentStoreRoots;

/// Stable persisted-session provider identity carried over the local/remote RPC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Claude,
    Codex,
    Droid,
    OpenCode,
    Copilot,
    Pi,
    Cursor,
    Antigravity,
    Omp,
    Grok,
    Gemini,
}

impl ProviderId {
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Droid => "droid",
            Self::OpenCode => "opencode",
            Self::Copilot => "copilot",
            Self::Pi => "pi",
            Self::Cursor => "cursor",
            Self::Antigravity => "antigravity",
            Self::Omp => "omp",
            Self::Grok => "grok",
            Self::Gemini => "gemini",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderScanner {
    ClaudeJsonl,
    CodexJsonlAndIndex,
    DroidJsonl,
    OpenCodeLegacyJson,
    CopilotJsonl,
    PiJsonl,
    CursorJsonl,
    AntigravityJsonl,
    OmpJsonl,
    GrokSummaryJson,
    GeminiTmpJsonl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub id: ProviderId,
    pub agent: CLIAgent,
    pub scanner: ProviderScanner,
    pub can_resume: bool,
}

impl ProviderDescriptor {
    pub fn source_roots(self, roots: &AgentStoreRoots) -> Vec<PathBuf> {
        match self.id {
            ProviderId::Claude => vec![roots.claude_projects()],
            ProviderId::Codex => vec![roots.codex_sessions(), roots.codex_index()],
            ProviderId::Droid => vec![roots.droid_sessions(), roots.droid_projects()],
            ProviderId::OpenCode => vec![roots.opencode_legacy_sessions()],
            ProviderId::Copilot => vec![roots.copilot_sessions()],
            ProviderId::Pi => vec![roots.pi_sessions()],
            ProviderId::Cursor => vec![roots.cursor_projects()],
            ProviderId::Antigravity => vec![roots.antigravity_brain()],
            ProviderId::Omp => vec![roots.omp_sessions()],
            ProviderId::Grok => vec![roots.grok_sessions()],
            ProviderId::Gemini => vec![roots.gemini_tmp()],
        }
    }

    /// Validate a source against the same target-owned roots snapshot used by
    /// discovery. Lexical component checks are intentional: Host performs the
    /// actual I/O, including remote I/O, so the GUI must not canonicalize a
    /// remote path through its local filesystem.
    pub fn accepts_source(self, roots: &AgentStoreRoots, source: &Path) -> bool {
        match self.id {
            ProviderId::Codex if source == roots.codex_index() => true,
            ProviderId::Omp => is_omp_session_source(&roots.omp_sessions(), source),
            ProviderId::Grok => is_grok_session_source(&roots.grok_sessions(), source),
            ProviderId::Gemini => is_gemini_session_source(&roots.gemini_tmp(), source),
            ProviderId::Cursor => {
                is_descendant(&roots.cursor_projects(), source)
                    && source
                        .components()
                        .any(|component| component.as_os_str() == "agent-transcripts")
                    && has_extension(source, "jsonl")
            }
            ProviderId::Antigravity => {
                is_descendant(&roots.antigravity_brain(), source)
                    && source.file_name().and_then(|name| name.to_str()) == Some("transcript.jsonl")
            }
            ProviderId::OpenCode => {
                is_descendant(&roots.opencode_legacy_sessions(), source)
                    && has_extension(source, "json")
            }
            _ => {
                self.source_roots(roots)
                    .iter()
                    .any(|root| is_descendant(root, source))
                    && has_extension(source, "jsonl")
            }
        }
    }
}

pub const PERSISTED_PROVIDER_DESCRIPTORS: &[ProviderDescriptor] = &[
    ProviderDescriptor {
        id: ProviderId::Claude,
        agent: CLIAgent::Claude,
        scanner: ProviderScanner::ClaudeJsonl,
        can_resume: true,
    },
    ProviderDescriptor {
        id: ProviderId::Codex,
        agent: CLIAgent::Codex,
        scanner: ProviderScanner::CodexJsonlAndIndex,
        can_resume: true,
    },
    ProviderDescriptor {
        id: ProviderId::Droid,
        agent: CLIAgent::Droid,
        scanner: ProviderScanner::DroidJsonl,
        can_resume: true,
    },
    ProviderDescriptor {
        id: ProviderId::OpenCode,
        agent: CLIAgent::OpenCode,
        scanner: ProviderScanner::OpenCodeLegacyJson,
        can_resume: true,
    },
    ProviderDescriptor {
        id: ProviderId::Copilot,
        agent: CLIAgent::Copilot,
        scanner: ProviderScanner::CopilotJsonl,
        can_resume: true,
    },
    ProviderDescriptor {
        id: ProviderId::Pi,
        agent: CLIAgent::Pi,
        scanner: ProviderScanner::PiJsonl,
        can_resume: true,
    },
    ProviderDescriptor {
        id: ProviderId::Cursor,
        agent: CLIAgent::Cursor,
        scanner: ProviderScanner::CursorJsonl,
        can_resume: false,
    },
    ProviderDescriptor {
        id: ProviderId::Antigravity,
        agent: CLIAgent::Antigravity,
        scanner: ProviderScanner::AntigravityJsonl,
        can_resume: true,
    },
    ProviderDescriptor {
        id: ProviderId::Omp,
        agent: CLIAgent::Omp,
        scanner: ProviderScanner::OmpJsonl,
        can_resume: true,
    },
    ProviderDescriptor {
        id: ProviderId::Grok,
        agent: CLIAgent::Grok,
        scanner: ProviderScanner::GrokSummaryJson,
        can_resume: true,
    },
    ProviderDescriptor {
        id: ProviderId::Gemini,
        agent: CLIAgent::Gemini,
        scanner: ProviderScanner::GeminiTmpJsonl,
        can_resume: true,
    },
];

pub fn descriptor_for_id(id: ProviderId) -> &'static ProviderDescriptor {
    PERSISTED_PROVIDER_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.id == id)
        .expect("every ProviderId has exactly one persisted descriptor")
}

pub fn descriptor_for_agent(agent: CLIAgent) -> Option<&'static ProviderDescriptor> {
    PERSISTED_PROVIDER_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.agent == agent)
}

fn has_extension(path: &Path, expected: &str) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some(expected)
}

fn is_descendant(root: &Path, path: &Path) -> bool {
    path != root && path.strip_prefix(root).is_ok()
}

fn is_omp_session_source(root: &Path, path: &Path) -> bool {
    path.parent()
        .and_then(Path::parent)
        .is_some_and(|parent| parent == root)
        && has_extension(path, "jsonl")
}

fn is_grok_session_source(root: &Path, path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("summary.json")
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .is_some_and(|sessions| sessions == root)
        && path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(looks_like_session_id)
}

fn is_gemini_session_source(root: &Path, path: &Path) -> bool {
    has_extension(path, "jsonl")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("session-"))
        && path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("chats")
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .is_some_and(|tmp| tmp == root)
}

fn looks_like_session_id(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_descriptor_rejects_noncanonical_sources() {
        let roots = AgentStoreRoots::for_home("/home/alice".into());
        let descriptor = descriptor_for_id(ProviderId::Codex);
        assert!(descriptor.accepts_source(&roots, &roots.codex_sessions().join("rollout.jsonl")));
        assert!(descriptor.accepts_source(&roots, &roots.codex_index()));
        assert!(!descriptor.accepts_source(&roots, Path::new("/tmp/rollout.jsonl")));
        assert!(!descriptor_for_id(ProviderId::Omp).accepts_source(
            &roots,
            &roots.omp_sessions().join("project/tool-logs/event.jsonl"),
        ));
        let grok = descriptor_for_id(ProviderId::Grok);
        assert!(
            grok.accepts_source(
                &roots,
                &roots
                    .grok_sessions()
                    .join("%2Fwork/019fd593-3979-7551-825d-bf5f8681a697/summary.json"),
            )
        );
        assert!(
            !grok.accepts_source(&roots, &roots.grok_sessions().join("session_search.sqlite"),)
        );
        assert!(!grok.accepts_source(
            &roots,
            &roots.grok_sessions().join("%2Fwork/prompt_history.jsonl"),
        ));
        let gemini = descriptor_for_id(ProviderId::Gemini);
        assert!(
            gemini.accepts_source(
                &roots,
                &roots
                    .gemini_tmp()
                    .join("repo/chats/session-2026-08-07T12-00-abcdef12.jsonl"),
            )
        );
        assert!(
            !gemini.accepts_source(
                &roots,
                &roots
                    .antigravity_brain()
                    .join("id/conversations/main/transcript.jsonl"),
            )
        );
    }

    #[test]
    fn jcode_is_not_advertised_without_stable_machine_list_contract() {
        assert_eq!(
            CLIAgent::from_slug("jcode"),
            Some(CLIAgent::Jcode),
            "runtime CLI identity is needed for temporary live carrier detection"
        );
        assert!(
            PERSISTED_PROVIDER_DESCRIPTORS
                .iter()
                .all(|descriptor| descriptor.id.slug() != "jcode"),
            "session discovery must not advertise a private-store Jcode provider"
        );
    }

    #[test]
    fn registry_has_no_descriptor_for_unpersisted_agents() {
        for agent in [
            CLIAgent::Amp,
            CLIAgent::Auggie,
            CLIAgent::Goose,
            CLIAgent::Qwen,
        ] {
            assert!(descriptor_for_agent(agent).is_none(), "{agent:?}");
        }
        assert!(descriptor_for_agent(CLIAgent::Gemini).is_some());
        assert!(PERSISTED_PROVIDER_DESCRIPTORS.len() > 3);
    }

    #[test]
    fn every_provider_id_has_one_descriptor() {
        let mut ids = std::collections::BTreeSet::new();
        for descriptor in PERSISTED_PROVIDER_DESCRIPTORS {
            assert!(ids.insert(descriptor.id));
            assert_eq!(descriptor_for_id(descriptor.id), descriptor);
        }
    }
}
