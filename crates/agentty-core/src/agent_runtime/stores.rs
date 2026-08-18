use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Immutable target-owned roots used by discovery and target-side integrations.
///
/// The struct is part of the additive `HelloOk.store_roots` snapshot.  Every
/// field has a serde default so a helper built before a field was introduced
/// can still be decoded; callers crossing the remote boundary must call
/// [`AgentStoreRoots::with_derived_defaults`] before using the snapshot.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentStoreRoots {
    #[serde(default)]
    pub home: PathBuf,
    #[serde(default)]
    pub codex_home: PathBuf,
    #[serde(default)]
    pub claude_config_dir: PathBuf,
    #[serde(default)]
    pub omp_agent_dir: PathBuf,
    #[serde(default)]
    pub opencode_data_dir: PathBuf,
    #[serde(default)]
    pub copilot_home: PathBuf,
    #[serde(default)]
    pub pi_agent_dir: PathBuf,
    /// Target XDG configuration root (`XDG_CONFIG_HOME`, or `$HOME/.config`).
    #[serde(default)]
    pub xdg_config_home: PathBuf,
    /// Agentty's own target configuration root (`AGENTTY_CONFIG_DIR`, or
    /// `$XDG_CONFIG_HOME/agentty`).
    #[serde(default)]
    pub agentty_config_dir: PathBuf,
    /// OpenCode's configuration root, independent from its data store.
    #[serde(default)]
    pub opencode_config_dir: PathBuf,
    /// Target Grok configuration/session root (`GROK_HOME`, or `$HOME/.grok`).
    #[serde(default)]
    pub grok_home: PathBuf,
}

impl AgentStoreRoots {
    pub fn for_home(home: PathBuf) -> Self {
        let xdg_config_home = join(&home, ".config");
        Self {
            codex_home: join(&home, ".codex"),
            claude_config_dir: join(&home, ".claude"),
            omp_agent_dir: join(&home, ".omp/agent"),
            opencode_data_dir: join(&home, ".local/share/opencode"),
            copilot_home: join(&home, ".copilot"),
            pi_agent_dir: join(&home, ".pi/agent"),
            agentty_config_dir: join(&xdg_config_home, "agentty"),
            opencode_config_dir: join(&xdg_config_home, "opencode"),
            grok_home: join(&home, ".grok"),
            xdg_config_home,
            home,
        }
    }

    /// Fill fields introduced after an older helper's `HelloOk` snapshot.
    ///
    /// This is intentionally explicit instead of making consumers guess from
    /// their own environment.  The published target `home` remains the sole
    /// authority for legacy defaults, and an already-populated field is never
    /// rewritten.
    pub fn with_derived_defaults(mut self) -> Self {
        let defaults = Self::for_home(self.home.clone());
        if self.codex_home.as_os_str().is_empty() {
            self.codex_home = defaults.codex_home;
        }
        if self.claude_config_dir.as_os_str().is_empty() {
            self.claude_config_dir = defaults.claude_config_dir;
        }
        if self.omp_agent_dir.as_os_str().is_empty() {
            self.omp_agent_dir = defaults.omp_agent_dir;
        }
        if self.opencode_data_dir.as_os_str().is_empty() {
            self.opencode_data_dir = defaults.opencode_data_dir;
        }
        if self.copilot_home.as_os_str().is_empty() {
            self.copilot_home = defaults.copilot_home;
        }
        if self.pi_agent_dir.as_os_str().is_empty() {
            self.pi_agent_dir = defaults.pi_agent_dir;
        }
        if self.xdg_config_home.as_os_str().is_empty() {
            self.xdg_config_home = defaults.xdg_config_home;
        }
        if self.agentty_config_dir.as_os_str().is_empty() {
            self.agentty_config_dir = join(&self.xdg_config_home, "agentty");
        }
        if self.opencode_config_dir.as_os_str().is_empty() {
            self.opencode_config_dir = join(&self.xdg_config_home, "opencode");
        }
        if self.grok_home.as_os_str().is_empty() {
            self.grok_home = defaults.grok_home;
        }
        self
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
        roots.xdg_config_home = configured_root(home, get("XDG_CONFIG_HOME"), ".config");
        roots.agentty_config_dir = match get("AGENTTY_CONFIG_DIR") {
            Some(value) if !value.is_empty() => {
                configured_root(home, Some(value), ".config/agentty")
            }
            _ => join(&roots.xdg_config_home, "agentty"),
        };
        roots.opencode_config_dir = match get("OPENCODE_CONFIG_DIR") {
            Some(value) if !value.is_empty() => {
                configured_root(home, Some(value), ".config/opencode")
            }
            _ => join(&roots.xdg_config_home, "opencode"),
        };
        roots.opencode_data_dir =
            configured_root(home, get("OPENCODE_DATA_DIR"), ".local/share/opencode");
        roots.copilot_home = configured_root(home, get("COPILOT_HOME"), ".copilot");
        roots.grok_home = configured_root(home, get("GROK_HOME"), ".grok");
        roots.pi_agent_dir = normalize_agent_root(
            configured_root(home, get("PI_CODING_AGENT_DIR"), ".pi/agent"),
            ".pi",
        );
        roots.omp_agent_dir = normalize_agent_root(
            configured_root(home, get("OMP_CODING_AGENT_DIR"), ".omp/agent"),
            ".omp",
        );
        roots.with_derived_defaults()
    }

    pub fn for_current_process(home: &Path) -> Self {
        Self::from_target_environment(home, |name| std::env::var_os(name))
    }

    pub fn jcode_sessions(&self) -> PathBuf {
        join(&self.home, ".jcode/sessions")
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
        join(&self.grok_home, "sessions")
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
        assert_eq!(roots.xdg_config_home, PathBuf::from("/home/remote/.config"));
        assert_eq!(
            roots.opencode_config_dir,
            PathBuf::from("/home/remote/.config/opencode")
        );
        assert_eq!(
            roots.opencode_data_dir,
            PathBuf::from("/home/remote/.local/share/opencode")
        );
        assert_eq!(roots.grok_home, PathBuf::from("/home/remote/.grok"));
    }

    #[test]
    fn target_environment_keeps_opencode_config_and_data_roots_distinct() {
        let roots =
            AgentStoreRoots::from_target_environment(
                Path::new("/home/remote"),
                |name| match name {
                    "XDG_CONFIG_HOME" => Some("/srv/config".into()),
                    "AGENTTY_CONFIG_DIR" => Some("relative-agentty-config".into()),
                    "OPENCODE_CONFIG_DIR" => Some("relative-opencode-config".into()),
                    "OPENCODE_DATA_DIR" => Some("/srv/opencode-data".into()),
                    "GROK_HOME" => Some("relative-grok".into()),
                    _ => None,
                },
            );
        assert_eq!(roots.xdg_config_home, PathBuf::from("/srv/config"));
        assert_eq!(
            roots.agentty_config_dir,
            PathBuf::from("/home/remote/relative-agentty-config")
        );
        assert_eq!(
            roots.opencode_config_dir,
            PathBuf::from("/home/remote/relative-opencode-config")
        );
        assert_eq!(roots.opencode_data_dir, PathBuf::from("/srv/opencode-data"));
        assert_eq!(roots.grok_home, PathBuf::from("/home/remote/relative-grok"));
        assert_eq!(
            roots.opencode_legacy_sessions(),
            PathBuf::from("/srv/opencode-data/storage/session")
        );
        assert_eq!(
            roots.grok_sessions(),
            PathBuf::from("/home/remote/relative-grok/sessions")
        );
    }

    #[test]
    fn old_serialized_roots_fill_new_hook_defaults_from_target_home() {
        let old = serde_json::json!({
            "home": "/home/legacy",
            "codex_home": "/home/legacy/.codex",
            "claude_config_dir": "/home/legacy/.claude",
            "omp_agent_dir": "/home/legacy/.omp/agent",
            "opencode_data_dir": "/home/legacy/.local/share/opencode",
            "copilot_home": "/home/legacy/.copilot",
            "pi_agent_dir": "/home/legacy/.pi/agent"
        });
        let roots: AgentStoreRoots =
            serde_json::from_value(old).expect("old HelloOk roots remain decodable");
        let roots = roots.with_derived_defaults();
        assert_eq!(roots.xdg_config_home, PathBuf::from("/home/legacy/.config"));
        assert_eq!(
            roots.agentty_config_dir,
            PathBuf::from("/home/legacy/.config/agentty")
        );
        assert_eq!(
            roots.opencode_config_dir,
            PathBuf::from("/home/legacy/.config/opencode")
        );
        assert_eq!(roots.grok_home, PathBuf::from("/home/legacy/.grok"));
    }
}
