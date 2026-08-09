use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentStoreRoots {
    pub home: PathBuf,
    pub codex_home: PathBuf,
    pub claude_config_dir: PathBuf,
    pub omp_agent_dir: PathBuf,
    pub opencode_data_dir: PathBuf,
    pub copilot_home: PathBuf,
    pub pi_agent_dir: PathBuf,
}

impl AgentStoreRoots {
    pub fn for_home(home: PathBuf) -> Self {
        Self {
            codex_home: join(&home, ".codex"),
            claude_config_dir: join(&home, ".claude"),
            omp_agent_dir: join(&home, ".omp/agent"),
            opencode_data_dir: join(&home, ".local/share/opencode"),
            copilot_home: join(&home, ".copilot"),
            pi_agent_dir: join(&home, ".pi/agent"),
            home,
        }
    }

    /// Resolve an immutable snapshot from the target process environment.
    /// Relative overrides are anchored to target HOME, never helper cwd.
    pub fn from_target_environment<F>(home: &Path, mut get: F) -> Self
    where
        F: FnMut(&str) -> Option<OsString>,
    {
        let mut roots = Self::for_home(home.to_path_buf());
        roots.codex_home = configured_root(home, get("CODEX_HOME"), ".codex");
        roots.claude_config_dir = configured_root(home, get("CLAUDE_CONFIG_DIR"), ".claude");
        roots.opencode_data_dir = configured_root(
            home,
            get("OPENCODE_CONFIG_DIR").or_else(|| get("OPENCODE_DATA_DIR")),
            ".local/share/opencode",
        );
        roots.copilot_home = configured_root(home, get("COPILOT_HOME"), ".copilot");
        roots.pi_agent_dir = normalize_agent_root(
            configured_root(home, get("PI_CODING_AGENT_DIR"), ".pi/agent"),
            ".pi",
        );
        roots.omp_agent_dir = normalize_agent_root(
            configured_root(home, get("OMP_CODING_AGENT_DIR"), ".omp/agent"),
            ".omp",
        );
        roots
    }

    pub fn for_current_process(home: &Path) -> Self {
        Self::from_target_environment(home, |name| std::env::var_os(name))
    }

    pub fn codex_sessions(&self) -> PathBuf {
        join(&self.codex_home, "sessions")
    }

    pub fn codex_index(&self) -> PathBuf {
        join(&self.codex_home, "session_index.jsonl")
    }

    pub fn claude_projects(&self) -> PathBuf {
        join(&self.claude_config_dir, "projects")
    }

    pub fn omp_sessions(&self) -> PathBuf {
        join(&self.omp_agent_dir, "sessions")
    }

    pub fn droid_sessions(&self) -> PathBuf {
        join(&self.home, ".factory/sessions")
    }

    pub fn droid_projects(&self) -> PathBuf {
        join(&self.home, ".factory/projects")
    }

    pub fn opencode_legacy_sessions(&self) -> PathBuf {
        join(&self.opencode_data_dir, "storage/session")
    }

    pub fn copilot_sessions(&self) -> PathBuf {
        join(&self.copilot_home, "session-state")
    }

    pub fn pi_sessions(&self) -> PathBuf {
        join(&self.pi_agent_dir, "sessions")
    }

    pub fn cursor_projects(&self) -> PathBuf {
        join(&self.home, ".cursor/projects")
    }

    pub fn antigravity_brain(&self) -> PathBuf {
        join(&self.home, ".gemini/antigravity-cli/brain")
    }

    pub fn grok_sessions(&self) -> PathBuf {
        join(&self.home, ".grok/sessions")
    }

    pub fn gemini_tmp(&self) -> PathBuf {
        join(&self.home, ".gemini/tmp")
    }
}

fn configured_root(home: &Path, value: Option<OsString>, fallback: &str) -> PathBuf {
    let value = value.filter(|value| !value.is_empty());
    let path = value
        .map(PathBuf::from)
        .unwrap_or_else(|| join(home, fallback));
    if path.is_absolute() {
        path
    } else {
        home.join(path)
    }
}

fn normalize_agent_root(path: PathBuf, family: &str) -> PathBuf {
    if path.file_name() == Some(OsStr::new("sessions")) {
        return path.parent().map(Path::to_path_buf).unwrap_or(path);
    }
    if path.ends_with(family) {
        return path.join("agent");
    }
    path
}

fn join(root: &Path, child: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    path.push(child);
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_environment_roots_are_resolved_once_against_target_home() {
        let roots =
            AgentStoreRoots::from_target_environment(
                Path::new("/home/remote"),
                |name| match name {
                    "CODEX_HOME" => Some("relative-codex".into()),
                    "CLAUDE_CONFIG_DIR" => Some("/srv/claude".into()),
                    "OMP_CODING_AGENT_DIR" => Some("/srv/.omp".into()),
                    _ => None,
                },
            );
        assert_eq!(
            roots.codex_home,
            PathBuf::from("/home/remote/relative-codex")
        );
        assert_eq!(roots.claude_config_dir, PathBuf::from("/srv/claude"));
        assert_eq!(roots.omp_agent_dir, PathBuf::from("/srv/.omp/agent"));
    }
}
