use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentStoreRoots {
    pub home: PathBuf,
    pub codex_home: PathBuf,
    pub claude_config_dir: PathBuf,
}

impl AgentStoreRoots {
    pub fn for_home(home: PathBuf) -> Self {
        Self {
            codex_home: join(&home, ".codex"),
            claude_config_dir: join(&home, ".claude"),
            home,
        }
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
}

fn join(root: &Path, child: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    path.push(child);
    path
}
