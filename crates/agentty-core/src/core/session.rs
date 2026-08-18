use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agent_runtime::SessionTitleCandidates;
use crate::daemon::protocol::NativeSshSpec;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SessionAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveContainerBinding {
    #[serde(default = "new_live_container_id")]
    pub container_id: String,
    #[serde(default)]
    pub agent: Option<crate::core::cli_agent::CLIAgent>,
    #[serde(default, rename = "agent_session_id")]
    pub session_id: Option<String>,
    #[serde(default, rename = "agent_launch_argv")]
    pub launch_argv: Vec<String>,
    /// Provider-authored or legacy historical title carried into a resumed
    /// live carrier.  Keep this separate from the first real AgentPrompt so a
    /// post-resume prompt cannot be mistaken for historical first-user
    /// evidence.
    #[serde(
        default,
        deserialize_with = "deserialize_provider_title",
        rename = "agent_provider_title",
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_title: Option<String>,
    /// First real AgentPrompt observed by the live carrier. This is a
    /// persisted title candidate, not the final display title; provider
    /// history may later supply a more meaningful title.
    #[serde(
        default,
        deserialize_with = "deserialize_first_user_title",
        rename = "agent_first_user_title",
        skip_serializing_if = "Option::is_none"
    )]
    pub first_user_title: Option<String>,
}

fn deserialize_first_user_title<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value.and_then(|value| crate::agent_runtime::normalize_title_candidate(&value)))
}

fn deserialize_provider_title<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value.and_then(|value| crate::agent_runtime::normalize_title_candidate(&value)))
}

fn new_live_container_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl LiveContainerBinding {
    pub fn new(
        agent: Option<crate::core::cli_agent::CLIAgent>,
        session_id: Option<String>,
        launch_argv: Vec<String>,
    ) -> Self {
        Self {
            container_id: new_live_container_id(),
            agent,
            session_id,
            launch_argv,
            provider_title: None,
            first_user_title: None,
        }
    }

    /// Materialize a live carrier with all title evidence already known by
    /// the historical Navigator row.  Provider and first-user slots remain
    /// typed and are never collapsed into one display string.
    pub fn new_with_title_candidates(
        agent: Option<crate::core::cli_agent::CLIAgent>,
        session_id: Option<String>,
        launch_argv: Vec<String>,
        candidates: SessionTitleCandidates,
    ) -> Self {
        let mut binding = Self::new(agent, session_id, launch_argv);
        binding.set_title_candidates(candidates);
        binding
    }

    /// Materialize a new live carrier without dropping title evidence already
    /// learned from its historical row.
    pub fn new_with_first_user_title(
        agent: Option<crate::core::cli_agent::CLIAgent>,
        session_id: Option<String>,
        launch_argv: Vec<String>,
        first_user_title: Option<String>,
    ) -> Self {
        Self::new_with_title_candidates(
            agent,
            session_id,
            launch_argv,
            SessionTitleCandidates::from_raw(None, first_user_title.as_deref()),
        )
    }

    pub fn restored(
        container_id: String,
        agent: Option<crate::core::cli_agent::CLIAgent>,
        session_id: Option<String>,
        launch_argv: Vec<String>,
    ) -> Self {
        Self {
            container_id,
            agent,
            session_id,
            launch_argv,
            provider_title: None,
            first_user_title: None,
        }
    }

    /// Restore a binding while retaining typed title evidence from the
    /// machine/session snapshot.  The legacy first-user-only constructor
    /// below remains for old callers and old JSON shapes.
    pub fn restored_with_title_candidates(
        container_id: String,
        agent: Option<crate::core::cli_agent::CLIAgent>,
        session_id: Option<String>,
        launch_argv: Vec<String>,
        candidates: SessionTitleCandidates,
    ) -> Self {
        let mut binding = Self::restored(container_id, agent, session_id, launch_argv);
        binding.set_title_candidates(candidates);
        binding
    }

    /// Restore a binding reconstructed from the machine tree while retaining
    /// the first-user candidate carried in AgentFacts. Keep [`Self::restored`]
    /// as the legacy no-title constructor for older callers and JSON shapes.
    pub fn restored_with_first_user_title(
        container_id: String,
        agent: Option<crate::core::cli_agent::CLIAgent>,
        session_id: Option<String>,
        launch_argv: Vec<String>,
        first_user_title: Option<String>,
    ) -> Self {
        Self::restored_with_title_candidates(
            container_id,
            agent,
            session_id,
            launch_argv,
            SessionTitleCandidates::from_raw(None, first_user_title.as_deref()),
        )
    }

    /// Return normalized typed title evidence carried by this binding.
    pub fn title_candidates(&self) -> SessionTitleCandidates {
        SessionTitleCandidates::from_raw(
            self.provider_title.as_deref(),
            self.first_user_title.as_deref(),
        )
    }

    /// Replace both title slots through the canonical normalizer.  This is
    /// the only resume/materialization boundary that should seed a binding.
    pub fn set_title_candidates(&mut self, candidates: SessionTitleCandidates) {
        let normalized = SessionTitleCandidates::from_raw(
            candidates.provider_title.as_deref(),
            candidates.first_user_title.as_deref(),
        );
        self.provider_title = normalized.provider_title;
        self.first_user_title = normalized.first_user_title;
    }

    pub fn provider_title(&self) -> Option<&str> {
        self.provider_title.as_deref()
    }

    /// Observe the first delivered AgentPrompt as a write-once title
    /// candidate. Placeholder and blank prompts are ignored by the shared
    /// title normalizer. Older persisted bindings and direct public struct
    /// literals may still contain a blank/product placeholder; normalize that
    /// slot before deciding whether the real prompt has already been seen.
    pub fn observe_first_user_title(&mut self, prompt: &str) -> bool {
        let previous_provider = self.provider_title.clone();
        let previous_first_user = self.first_user_title.clone();
        let normalized = self.title_candidates();
        self.set_title_candidates(normalized);
        let mut changed = self.provider_title != previous_provider
            || self.first_user_title != previous_first_user;
        // A provider/legacy title is already a stable historical name.  Do
        // not reinterpret a prompt typed after resume as its first-user
        // evidence; this keeps provenance and the visible name stable even if
        // the history source is temporarily unavailable.
        if self.provider_title.is_some() || self.first_user_title.is_some() {
            return changed;
        }
        let Some(title) = crate::agent_runtime::first_user_title_candidate(prompt) else {
            return changed;
        };
        self.first_user_title = Some(title);
        changed = true;
        changed
    }

    pub fn first_user_title(&self) -> Option<&str> {
        self.first_user_title.as_deref()
    }
}

impl Default for LiveContainerBinding {
    fn default() -> Self {
        Self::new(None, None, Vec::new())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionPane {
    Leaf {
        #[serde(default)]
        cwd: Option<PathBuf>,
        #[serde(default)]
        pane_id: Option<u64>,
        #[serde(default)]
        ssh_spec: Option<Box<NativeSshSpec>>,
        #[serde(flatten)]
        live_binding: LiveContainerBinding,
    },
    Split {
        axis: SessionAxis,
        #[serde(default = "default_ratio")]
        ratio: f32,
        a: Box<SessionPane>,
        b: Box<SessionPane>,
    },
}

fn default_ratio() -> f32 {
    0.5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTab {
    #[serde(default)]
    pub name: Option<String>,
    pub pane: SessionPane,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_group: Option<std::path::PathBuf>,
    #[serde(skip)]
    pub tree_id: Option<crate::core::machine::TabId>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub active: usize,
    pub tabs: Vec<SessionTab>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(uuid::Uuid);

impl WorkspaceId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    pub fn element_key(&self) -> u64 {
        self.0.as_u64_pair().0
    }
}

impl Default for WorkspaceId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for WorkspaceId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(WorkspaceId)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteTarget {
    Profile {
        id: uuid::Uuid,
    },
    Alias {
        alias: String,
    },
    Direct {
        #[serde(default)]
        user: String,
        host: String,
        #[serde(default = "default_ssh_port")]
        port: u16,
    },
    Wsl {
        distro: String,
    },
    LocalStdio {
        program: String,
        args: Vec<String>,
    },
}

fn default_ssh_port() -> u16 {
    22
}

impl RemoteTarget {
    pub fn direct(user: impl Into<String>, host: impl Into<String>, port: u16) -> RemoteTarget {
        RemoteTarget::Direct {
            user: user.into(),
            host: host.into().to_ascii_lowercase(),
            port,
        }
    }

    pub fn parse_direct(input: &str) -> Option<RemoteTarget> {
        let q = crate::core::ssh_profile::parse_quick_connect(input)?;
        let port = q.port_or_default();
        Some(RemoteTarget::direct(
            q.user.unwrap_or_default(),
            q.host,
            port,
        ))
    }

    pub fn connection_key(&self) -> String {
        match self {
            RemoteTarget::Profile { id } => format!("ssh-profile:{id}"),
            RemoteTarget::Alias { alias } => format!("ssh-alias:{alias}"),
            RemoteTarget::Direct { user, host, port } => {
                format!("ssh-direct:{user}@{}:{port}", host.to_ascii_lowercase())
            }
            RemoteTarget::Wsl { distro } => format!("wsl:{distro}"),
            RemoteTarget::LocalStdio { program, args } => {
                format!("local-stdio:{program} {}", args.join(" "))
            }
        }
    }

    pub fn is_ssh(&self) -> bool {
        match self {
            RemoteTarget::Profile { .. }
            | RemoteTarget::Alias { .. }
            | RemoteTarget::Direct { .. } => true,
            RemoteTarget::Wsl { .. } | RemoteTarget::LocalStdio { .. } => false,
        }
    }

    pub fn can_restart_server(&self) -> bool {
        matches!(
            self,
            RemoteTarget::Profile { .. }
                | RemoteTarget::Alias { .. }
                | RemoteTarget::Direct { .. }
                | RemoteTarget::Wsl { .. }
        )
    }

    pub fn host_id(&self) -> crate::host::HostId {
        crate::host::HostId::from_connection_key(&self.connection_key())
    }
}

impl std::fmt::Display for RemoteTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoteTarget::Profile { id } => write!(f, "{id}"),
            RemoteTarget::Alias { alias } => write!(f, "{alias}"),
            RemoteTarget::Direct { user, host, port } => {
                if !user.is_empty() {
                    write!(f, "{user}@")?;
                }
                write!(f, "{host}")?;
                if *port != 22 {
                    write!(f, ":{port}")?;
                }
                Ok(())
            }
            RemoteTarget::Wsl { distro } => write!(f, "wsl:{distro}"),
            RemoteTarget::LocalStdio { program, .. } => {
                let name = std::path::Path::new(program)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| program.clone());
                write!(f, "local:{name}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteRef {
    pub target: RemoteTarget,
    pub workspace: WorkspaceId,
}

impl RemoteRef {
    pub fn new(target: RemoteTarget, workspace: WorkspaceId) -> RemoteRef {
        RemoteRef { target, workspace }
    }

    pub fn host_id(&self) -> crate::host::HostId {
        self.target.host_id()
    }

    pub fn store_key(&self) -> String {
        self.workspace.to_string()
    }
}

/// Persisted application window keyed by execution Environment rather than by
/// the daemon workspace collection hosted inside that window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentWindow {
    pub environment: crate::core::environment::EnvironmentDescriptor,
    /// Temporary routing key for the daemon tree collection. This is not the
    /// product-level identity of the window.
    #[serde(default)]
    pub workspace: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_workspace: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<crate::core::window_state::WindowState>,
    #[serde(default)]
    pub open: bool,
    #[serde(default)]
    pub last_active: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

impl Default for EnvironmentWindow {
    fn default() -> Self {
        Self {
            environment: crate::core::environment::EnvironmentDescriptor::local(),
            workspace: WorkspaceId::new(),
            remote_workspace: None,
            window: None,
            open: true,
            last_active: now_secs(),
            label: None,
            subject: None,
        }
    }
}

impl EnvironmentWindow {
    pub fn remote(
        target: RemoteTarget,
        workspace: WorkspaceId,
        remote_workspace: WorkspaceId,
    ) -> Self {
        Self {
            environment: crate::core::environment::EnvironmentDescriptor::remote(target),
            workspace,
            remote_workspace: Some(remote_workspace),
            ..Self::default()
        }
    }

    pub fn touch(&mut self) {
        self.last_active = now_secs();
    }

    pub fn is_remote(&self) -> bool {
        !self.environment.id.is_local()
    }

    pub fn remote_ref(&self) -> Option<RemoteRef> {
        Some(RemoteRef::new(
            self.environment.target.clone()?,
            self.remote_workspace?,
        ))
    }

    pub fn host_id(&self) -> crate::host::HostId {
        self.environment
            .target
            .as_ref()
            .map(RemoteTarget::host_id)
            .unwrap_or(crate::host::HostId::LOCAL)
    }

    pub fn peer(&self) -> Self {
        Self {
            environment: self.environment.clone(),
            workspace: WorkspaceId::new(),
            remote_workspace: self.is_remote().then(WorkspaceId::new),
            window: None,
            open: true,
            last_active: now_secs(),
            label: self.label.clone(),
            subject: self.subject.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvironmentWindows {
    pub version: u32,
    pub windows: Vec<EnvironmentWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<crate::core::environment::EnvironmentId>,
}

impl EnvironmentWindows {
    pub const VERSION: u32 = 1;

    pub fn from_legacy(legacy: WindowViews) -> Self {
        let active = legacy.active.and_then(|id| {
            legacy.get(id).map(|view| match &view.host {
                Some(remote) => crate::core::environment::EnvironmentId::for_remote(&remote.target),
                None => crate::core::environment::EnvironmentId::local(),
            })
        });
        let mut windows = Vec::new();
        for view in legacy.views {
            let environment = view
                .host
                .as_ref()
                .map(|remote| {
                    crate::core::environment::EnvironmentDescriptor::remote(remote.target.clone())
                })
                .unwrap_or_else(crate::core::environment::EnvironmentDescriptor::local);
            let candidate = EnvironmentWindow {
                environment,
                workspace: view.id,
                remote_workspace: view.host.as_ref().map(|remote| remote.workspace),
                window: view.window,
                open: view.open,
                last_active: view.last_active,
                label: view.label,
                subject: view.subject,
            };
            if let Some(existing) = windows.iter_mut().find(|window: &&mut EnvironmentWindow| {
                window.environment.id == candidate.environment.id
            }) {
                if candidate.last_active > existing.last_active {
                    *existing = candidate;
                } else {
                    existing.open |= candidate.open;
                }
            } else {
                windows.push(candidate);
            }
        }
        Self {
            version: Self::VERSION,
            windows,
            active,
        }
    }

    pub fn get(&self, workspace: WorkspaceId) -> Option<&EnvironmentWindow> {
        self.get_workspace(workspace)
    }

    pub fn get_environment(
        &self,
        id: &crate::core::environment::EnvironmentId,
    ) -> Option<&EnvironmentWindow> {
        self.windows
            .iter()
            .find(|window| &window.environment.id == id)
    }

    pub fn latest_environment(
        &self,
        id: &crate::core::environment::EnvironmentId,
    ) -> Option<&EnvironmentWindow> {
        self.windows
            .iter()
            .filter(|window| &window.environment.id == id)
            .max_by_key(|window| (window.open, window.last_active))
    }

    pub fn get_environment_mut(
        &mut self,
        id: &crate::core::environment::EnvironmentId,
    ) -> Option<&mut EnvironmentWindow> {
        self.windows
            .iter_mut()
            .find(|window| &window.environment.id == id)
    }

    pub fn get_workspace(&self, workspace: WorkspaceId) -> Option<&EnvironmentWindow> {
        self.windows
            .iter()
            .find(|window| window.workspace == workspace)
    }

    pub fn get_workspace_mut(&mut self, workspace: WorkspaceId) -> Option<&mut EnvironmentWindow> {
        self.windows
            .iter_mut()
            .find(|window| window.workspace == workspace)
    }

    pub fn open_windows(&self) -> impl Iterator<Item = &EnvironmentWindow> {
        self.windows.iter().filter(|window| window.open)
    }

    pub fn workspace_to_restore(&self) -> Option<WorkspaceId> {
        let focused = self
            .active
            .as_ref()
            .and_then(|id| self.latest_environment(id))
            .filter(|w| w.open);
        focused
            .or_else(|| self.open_windows().max_by_key(|w| w.last_active))
            .or_else(|| self.windows.iter().max_by_key(|w| w.last_active))
            .map(|w| w.workspace)
    }

    pub fn workspaces_to_restore(&self) -> Vec<WorkspaceId> {
        let mut open: Vec<_> = self.open_windows().collect();
        open.sort_by_key(|window| window.last_active);
        if !open.is_empty() {
            return open.into_iter().map(|window| window.workspace).collect();
        }
        self.workspace_to_restore().into_iter().collect()
    }

    pub fn load() -> Option<Self> {
        let path = Self::path()?;
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_json::from_str::<Self>(crate::core::config::strip_bom(&text)) {
                Ok(loaded) if loaded.version == Self::VERSION => return Some(loaded),
                Ok(loaded) => log::warn!(
                    "unsupported environment window version {} at {}; ignoring",
                    loaded.version,
                    path.display()
                ),
                Err(e) => log::warn!(
                    "failed to parse environment windows at {}: {e}; ignoring",
                    path.display()
                ),
            }
        }
        let legacy = WindowViews::load()?;
        let migrated = Self::from_legacy(legacy);
        migrated.save();
        Some(migrated)
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            log::warn!(
                "failed to create environment windows dir {}: {e}",
                parent.display()
            );
            return;
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = crate::core::config::write_atomic(&path, json.as_bytes()) {
                    log::warn!(
                        "failed to write environment windows to {}: {e}",
                        path.display()
                    );
                }
            }
            Err(e) => log::warn!("failed to serialize environment windows: {e}"),
        }
    }

    fn path() -> Option<PathBuf> {
        crate::core::config::config_path("environment-windows.json")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowView {
    #[serde(default)]
    pub id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<crate::core::window_state::WindowState>,
    #[serde(default)]
    pub open: bool,
    #[serde(default)]
    pub last_active: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<RemoteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

impl Default for WindowView {
    fn default() -> Self {
        Self {
            id: WorkspaceId::new(),
            window: None,
            open: true,
            last_active: now_secs(),
            host: None,
            label: None,
            subject: None,
        }
    }
}

impl WindowView {
    pub fn touch(&mut self) {
        self.last_active = now_secs();
    }

    pub fn on_remote(host: RemoteRef) -> WindowView {
        WindowView {
            host: Some(host),
            ..WindowView::default()
        }
    }

    pub fn is_remote(&self) -> bool {
        self.host.is_some()
    }

    pub fn host_id(&self) -> crate::host::HostId {
        match &self.host {
            Some(r) => r.host_id(),
            None => crate::host::HostId::LOCAL,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowViews {
    pub views: Vec<WindowView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<WorkspaceId>,
}

impl WindowViews {
    pub fn load() -> Option<Self> {
        let path = Self::path()?;
        let text = std::fs::read_to_string(&path).ok()?;
        match serde_json::from_str(crate::core::config::strip_bom(&text)) {
            Ok(loaded) => Some(loaded),
            Err(e) => {
                log::warn!("failed to parse views at {}: {e}; ignoring", path.display());
                None
            }
        }
    }

    pub fn get(&self, id: WorkspaceId) -> Option<&WindowView> {
        self.views.iter().find(|w| w.id == id)
    }

    pub fn get_mut(&mut self, id: WorkspaceId) -> Option<&mut WindowView> {
        self.views.iter_mut().find(|w| w.id == id)
    }

    pub fn open_views(&self) -> impl Iterator<Item = &WindowView> {
        self.views.iter().filter(|w| w.open)
    }

    pub fn workspace_to_restore(&self) -> Option<WorkspaceId> {
        let focused = self
            .active
            .filter(|id| self.get(*id).is_some_and(|w| w.open));
        focused
            .or_else(|| {
                self.open_views()
                    .max_by_key(|w| w.last_active)
                    .map(|w| w.id)
            })
            .or_else(|| {
                self.views
                    .iter()
                    .max_by_key(|w| w.last_active)
                    .map(|w| w.id)
            })
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log::warn!("failed to create views dir {}: {e}", parent.display());
                return;
            }
        }
        let json = match serde_json::to_string_pretty(self) {
            Ok(j) => j,
            Err(e) => {
                log::warn!("failed to serialize views: {e}");
                return;
            }
        };
        if let Err(e) = crate::core::config::write_atomic(&path, json.as_bytes()) {
            log::warn!("failed to write views to {}: {e}", path.display());
        }
    }

    fn path() -> Option<PathBuf> {
        crate::core::config::config_path("views.json")
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard};

    static SESSION_FILE: Mutex<()> = Mutex::new(());

    pub(crate) fn lock_session_file() -> MutexGuard<'static, ()> {
        SESSION_FILE.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn pin_config_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agentty-covtest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        crate::core::config::set_config_dir(dir.clone());
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{lock_session_file, pin_config_dir};
    use super::*;

    fn view() -> WindowView {
        WindowView::default()
    }

    fn remote_view(alias: &str) -> WindowView {
        WindowView::on_remote(RemoteRef::new(
            RemoteTarget::Alias {
                alias: alias.into(),
            },
            WorkspaceId::new(),
        ))
    }

    #[test]
    fn legacy_window_views_migrate_once() {
        let _file = lock_session_file();
        let dir = pin_config_dir();
        let _ = std::fs::remove_file(dir.join("environment-windows.json"));
        let mut local = view();
        local.open = false;
        local.last_active = 10;
        let mut older_duplicate = view();
        older_duplicate.last_active = 5;
        let remote = remote_view("build-box");
        let remote_environment = crate::core::environment::EnvironmentId::for_remote(
            &remote.host.as_ref().unwrap().target,
        );
        let legacy = WindowViews {
            active: Some(remote.id),
            views: vec![local, older_duplicate, remote.clone()],
        };
        legacy.save();

        let migrated = EnvironmentWindows::load().expect("legacy views should migrate");
        assert_eq!(migrated.version, EnvironmentWindows::VERSION);
        assert_eq!(migrated.windows.len(), 2, "one window per Environment");
        assert_eq!(migrated.active, Some(remote_environment.clone()));
        assert_eq!(
            migrated
                .get_environment(&remote_environment)
                .unwrap()
                .remote_workspace,
            Some(remote.host.unwrap().workspace),
        );
        assert!(dir.join("environment-windows.json").exists());

        // Once the new file exists, edits to the legacy file cannot retarget it.
        WindowViews::default().save();
        let loaded = EnvironmentWindows::load().unwrap();
        assert_eq!(loaded.windows.len(), 2);
        assert_eq!(loaded.active, Some(remote_environment));
    }

    #[test]
    fn environment_window_state_round_trip() {
        let _file = lock_session_file();
        let dir = pin_config_dir();
        let _ = std::fs::remove_file(dir.join("environment-windows.json"));
        let target = RemoteTarget::direct("me", "build.local", 2222);
        let workspace = WorkspaceId::new();
        let remote_workspace = WorkspaceId::new();
        let mut window = EnvironmentWindow::remote(target.clone(), workspace, remote_workspace);
        window.window = Some(crate::core::window_state::WindowState {
            x: 12.0,
            y: 24.0,
            width: 1400.0,
            height: 900.0,
        });
        window.open = false;
        window.last_active = 42;
        let environment = window.environment.id.clone();
        EnvironmentWindows {
            version: EnvironmentWindows::VERSION,
            windows: vec![window],
            active: Some(environment.clone()),
        }
        .save();

        let loaded = EnvironmentWindows::load().unwrap();
        let window = loaded.get_environment(&environment).unwrap();
        assert_eq!(window.workspace, workspace);
        assert_eq!(window.remote_workspace, Some(remote_workspace));
        assert_eq!(window.environment.target.as_ref(), Some(&target));
        assert_eq!(window.window.unwrap().width, 1400.0);
        assert!(!window.open);
        assert_eq!(loaded.active, Some(environment));
    }

    #[test]
    fn tabs_restore_without_authority_drift() {
        let local = EnvironmentWindow::default();
        let target = RemoteTarget::Alias {
            alias: "gpu".into(),
        };
        let remote =
            EnvironmentWindow::remote(target.clone(), WorkspaceId::new(), WorkspaceId::new());
        let remote_workspace = remote.workspace;
        let remote_environment = remote.environment.id.clone();
        let windows = EnvironmentWindows {
            version: EnvironmentWindows::VERSION,
            windows: vec![local, remote],
            active: Some(remote_environment.clone()),
        };

        assert_eq!(windows.workspace_to_restore(), Some(remote_workspace));
        let restored = windows.get(remote_workspace).unwrap();
        assert_eq!(restored.environment.id, remote_environment);
        assert_eq!(restored.environment.target.as_ref(), Some(&target));
        assert!(!restored.environment.id.is_local());
    }

    #[test]
    fn views_round_trip_through_their_file() {
        let _file = lock_session_file();
        pin_config_dir();
        let mut entry = remote_view("build-box");
        entry.open = false;
        entry.last_active = 1_700_000_000;
        let id = entry.id;
        let host = entry.host.clone();
        WindowViews {
            active: Some(id),
            views: vec![entry],
        }
        .save();
        let loaded = WindowViews::load().expect("a saved views file should load back");
        let only = &loaded.views[0];
        assert_eq!(only.id, id, "identity must survive a restart");
        assert_eq!(
            only.host, host,
            "the remote pointer is the load-bearing half"
        );
        assert!(!only.open);
        assert_eq!(only.last_active, 1_700_000_000);
        assert_eq!(loaded.active, Some(id));
    }

    #[test]
    fn an_empty_or_partial_file_decodes_to_defaults() {
        let empty: WindowViews = serde_json::from_str("{}").unwrap();
        assert!(empty.views.is_empty());
        assert!(empty.active.is_none());
        let partial: WindowViews = serde_json::from_str(r#"{"views":[{}]}"#).unwrap();
        assert_eq!(partial.views.len(), 1);
        assert!(!partial.views[0].is_remote());
    }

    #[test]
    fn legacy_active_local_window_migrates_without_panic() {
        // Regression: from_legacy used to expect() a remote host while mapping
        // the active view, so a legacy views.json whose active window was local
        // panicked during migration on launch.
        let local = WindowView::default();
        let local_id = local.id;
        let legacy = WindowViews {
            views: vec![local],
            active: Some(local_id),
        };
        let migrated = EnvironmentWindows::from_legacy(legacy);
        assert_eq!(
            migrated.active,
            Some(crate::core::environment::EnvironmentId::local())
        );

        let remote = WindowView::on_remote(RemoteRef::new(
            RemoteTarget::Alias {
                alias: "build".into(),
            },
            WorkspaceId::new(),
        ));
        let remote_id = remote.id;
        let legacy = WindowViews {
            views: vec![remote],
            active: Some(remote_id),
        };
        let migrated = EnvironmentWindows::from_legacy(legacy);
        assert_eq!(
            migrated.active,
            Some(crate::core::environment::EnvironmentId::for_remote(
                &RemoteTarget::Alias {
                    alias: "build".into()
                }
            ))
        );
    }

    #[test]
    fn connection_keys_match_the_contract_table() {
        let uuid = uuid::Uuid::parse_str("6a8f2a1e-1c1b-4f7a-9d3e-2b5c8e4a7f01").unwrap();
        assert_eq!(
            RemoteTarget::Profile { id: uuid }.connection_key(),
            "ssh-profile:6a8f2a1e-1c1b-4f7a-9d3e-2b5c8e4a7f01"
        );
        assert_eq!(
            RemoteTarget::Alias {
                alias: "devbox".into()
            }
            .connection_key(),
            "ssh-alias:devbox"
        );
        assert_eq!(
            RemoteTarget::direct("me", "box.local", 22).connection_key(),
            "ssh-direct:me@box.local:22"
        );
        assert_eq!(
            RemoteTarget::direct("me", "box.local", 2222).connection_key(),
            "ssh-direct:me@box.local:2222"
        );
        assert_eq!(
            RemoteTarget::Wsl {
                distro: "Ubuntu".into()
            }
            .connection_key(),
            "wsl:Ubuntu"
        );
    }

    #[test]
    fn wsl_is_restartable_without_becoming_an_ssh_target() {
        assert!(
            RemoteTarget::Profile {
                id: uuid::Uuid::nil()
            }
            .is_ssh()
        );
        assert!(
            RemoteTarget::Alias {
                alias: "devbox".into()
            }
            .is_ssh()
        );
        assert!(RemoteTarget::direct("me", "box.local", 22).is_ssh());
        assert!(RemoteTarget::direct("me", "box.local", 22).can_restart_server());
        assert!(
            !RemoteTarget::Wsl {
                distro: "Ubuntu".into()
            }
            .is_ssh(),
            "a distribution's server is started by this client"
        );
        assert!(
            RemoteTarget::Wsl {
                distro: "Ubuntu".into()
            }
            .can_restart_server(),
            "a WSL distribution has a managed server lifecycle without being SSH"
        );
        assert!(
            !RemoteTarget::LocalStdio {
                program: "agentty-server".into(),
                args: vec!["--stdio".into()],
            }
            .is_ssh(),
            "a stdio machine is a child process per connection"
        );
        assert!(
            !RemoteTarget::LocalStdio {
                program: "agentty-server".into(),
                args: vec!["--stdio".into()],
            }
            .can_restart_server()
        );
    }

    #[test]
    fn direct_targets_normalize_and_reuse_the_quick_connect_parser() {
        assert_eq!(
            RemoteTarget::parse_direct("ssh://me@Box.Local"),
            Some(RemoteTarget::direct("me", "box.local", 22))
        );
        assert_eq!(
            RemoteTarget::parse_direct("me@box.local:2222"),
            Some(RemoteTarget::direct("me", "box.local", 2222))
        );
        let shouty = RemoteTarget::Direct {
            user: "me".into(),
            host: "BOX.LOCAL".into(),
            port: 22,
        };
        assert_eq!(
            shouty.host_id(),
            RemoteTarget::direct("me", "box.local", 22).host_id()
        );
        assert_eq!(RemoteTarget::parse_direct(""), None);
        assert_eq!(RemoteTarget::parse_direct("me@box:0"), None);
        assert_ne!(
            RemoteTarget::Alias {
                alias: "Devbox".into()
            }
            .connection_key(),
            RemoteTarget::Alias {
                alias: "devbox".into()
            }
            .connection_key()
        );
    }

    #[test]
    fn a_local_stdio_target_is_its_own_machine() {
        let a = RemoteTarget::LocalStdio {
            program: "/opt/agentty-server".into(),
            args: vec!["--stdio".into()],
        };
        let b = RemoteTarget::LocalStdio {
            program: "/tmp/other-server".into(),
            args: vec!["--stdio".into()],
        };
        assert_eq!(
            a.connection_key(),
            "local-stdio:/opt/agentty-server --stdio"
        );
        assert_ne!(a.host_id(), b.host_id());
        assert!(
            !a.host_id().is_local(),
            "a routed target is never the local host"
        );
        assert_eq!(a.to_string(), "local:agentty-server");
    }

    #[test]
    fn views_on_one_box_share_a_host_id() {
        let target = RemoteTarget::Alias {
            alias: "devbox".into(),
        };
        let a = WindowView::on_remote(RemoteRef::new(target.clone(), WorkspaceId::new()));
        let b = WindowView::on_remote(RemoteRef::new(target.clone(), WorkspaceId::new()));
        assert_ne!(
            a.host.as_ref().unwrap().workspace,
            b.host.as_ref().unwrap().workspace
        );
        assert_eq!(a.host_id(), b.host_id(), "same machine, one HostId");
        assert!(!a.host_id().is_local());

        let other = remote_view("other");
        assert_ne!(a.host_id(), other.host_id());

        assert_eq!(view().host_id(), crate::host::HostId::LOCAL);
        assert_eq!(
            a.host.as_ref().unwrap().store_key(),
            a.host.as_ref().unwrap().workspace.to_string()
        );
    }

    #[test]
    fn open_views_partition_by_flag() {
        let mut open_one = view();
        open_one.open = true;
        let mut closed = view();
        closed.open = false;
        let open_id = open_one.id;
        let all = WindowViews {
            active: None,
            views: vec![open_one, closed],
        };
        assert_eq!(
            all.open_views().map(|w| w.id).collect::<Vec<_>>(),
            vec![open_id]
        );
    }

    #[test]
    fn launch_restores_the_focused_workspace_not_the_most_recently_touched() {
        let mut focused = view();
        focused.open = true;
        focused.last_active = 100;
        let mut busier = view();
        busier.open = true;
        busier.last_active = 900;
        let (focused_id, busier_id) = (focused.id, busier.id);

        let all = WindowViews {
            active: Some(focused_id),
            views: vec![focused, busier],
        };
        assert_eq!(all.workspace_to_restore(), Some(focused_id));
        assert_eq!(
            all.open_views().count(),
            2,
            "the others stay open in the store — launch detaches them, this does not"
        );

        let all = WindowViews {
            active: None,
            ..all
        };
        assert_eq!(all.workspace_to_restore(), Some(busier_id));

        let mut closed = view();
        closed.open = false;
        let closed_id = closed.id;
        let mut open_one = view();
        open_one.open = true;
        let open_id = open_one.id;
        let all = WindowViews {
            active: Some(closed_id),
            views: vec![closed, open_one],
        };
        assert_eq!(all.workspace_to_restore(), Some(open_id));

        let mut first_closed = view();
        first_closed.open = false;
        first_closed.last_active = 100;
        let mut closed_last = view();
        closed_last.open = false;
        closed_last.last_active = 900;
        let closed_last_id = closed_last.id;
        let all = WindowViews {
            active: None,
            views: vec![first_closed, closed_last],
        };
        assert_eq!(all.workspace_to_restore(), Some(closed_last_id));

        let all = WindowViews {
            active: Some(WorkspaceId::new()),
            ..all
        };
        assert_eq!(all.workspace_to_restore(), Some(closed_last_id));

        assert_eq!(WindowViews::default().workspace_to_restore(), None);
    }

    #[test]
    fn an_open_workspace_outranks_a_more_recently_touched_detached_one() {
        let mut open_one = view();
        open_one.open = true;
        open_one.last_active = 100;
        let open_id = open_one.id;
        let mut detached = view();
        detached.open = false;
        detached.last_active = 900;

        let all = WindowViews {
            active: None,
            views: vec![open_one, detached],
        };
        assert_eq!(all.workspace_to_restore(), Some(open_id));
    }
}

#[cfg(test)]
mod live_container_binding_tests {
    use super::*;

    #[test]
    fn restored_live_binding_survives_placeholder_and_restart() {
        let mut binding = LiveContainerBinding::new(
            Some(crate::core::cli_agent::CLIAgent::Codex),
            Some("session-1".into()),
            vec!["codex".into(), "resume".into(), "session-1".into()],
        );
        binding.first_user_title = Some("Draw a fox".into());
        let pane = SessionPane::Leaf {
            cwd: None,
            pane_id: Some(7),
            ssh_spec: None,
            live_binding: binding.clone(),
        };
        let encoded = serde_json::to_string(&pane).unwrap();
        let decoded: SessionPane = serde_json::from_str(&encoded).unwrap();
        let SessionPane::Leaf { live_binding, .. } = decoded else {
            panic!("leaf")
        };
        assert_eq!(live_binding, binding);
    }

    #[test]
    fn live_binding_first_user_title_round_trips_and_old_json_defaults() {
        let mut binding = LiveContainerBinding::default();
        assert!(binding.observe_first_user_title("  Draw a fox  "));
        assert_eq!(binding.first_user_title.as_deref(), Some("Draw a fox"));

        let encoded = serde_json::to_string(&binding).unwrap();
        assert!(encoded.contains("agent_first_user_title"));
        let decoded: LiveContainerBinding = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.first_user_title.as_deref(), Some("Draw a fox"));

        // Existing session files predate the title field and must continue to
        // deserialize with an empty candidate.
        let legacy: LiveContainerBinding =
            serde_json::from_value(serde_json::json!({"container_id": "legacy"})).unwrap();
        assert_eq!(legacy.first_user_title, None);
        assert_eq!(legacy.provider_title, None);
    }

    #[test]
    fn live_binding_typed_title_candidates_round_trip_with_legacy_fields() {
        let binding = LiveContainerBinding::new_with_title_candidates(
            Some(crate::core::cli_agent::CLIAgent::Codex),
            Some("session-typed".into()),
            vec!["codex".into(), "--resume".into()],
            SessionTitleCandidates::from_raw(Some("Provider title"), Some("First request")),
        );
        let encoded = serde_json::to_value(&binding).unwrap();
        assert_eq!(
            encoded
                .get("agent_provider_title")
                .and_then(serde_json::Value::as_str),
            Some("Provider title")
        );
        assert_eq!(
            encoded
                .get("agent_first_user_title")
                .and_then(serde_json::Value::as_str),
            Some("First request")
        );
        let restored: LiveContainerBinding = serde_json::from_value(encoded).unwrap();
        assert_eq!(
            restored.title_candidates(),
            SessionTitleCandidates::from_raw(Some("Provider title"), Some("First request"))
        );
    }

    #[test]
    fn provider_title_seed_blocks_post_resume_prompt_without_fake_first_user() {
        let mut binding = LiveContainerBinding::new_with_title_candidates(
            Some(crate::core::cli_agent::CLIAgent::Codex),
            Some("session-provider".into()),
            vec!["codex".into(), "--resume".into()],
            SessionTitleCandidates::from_raw(Some("Provider title"), None),
        );
        assert!(!binding.observe_first_user_title("Second request"));
        assert_eq!(binding.provider_title(), Some("Provider title"));
        assert_eq!(binding.first_user_title(), None);
    }

    #[test]
    fn persisted_blank_or_placeholder_title_does_not_block_first_prompt() {
        for persisted in ["", "  ", "Agent 会话", "Agent会话", "agentty"] {
            let mut binding: LiveContainerBinding = serde_json::from_value(serde_json::json!({
                "container_id": "legacy",
                "agent_first_user_title": persisted,
            }))
            .unwrap();
            assert_eq!(binding.first_user_title, None, "persisted={persisted:?}");
            assert!(
                binding.observe_first_user_title("Draw a fox"),
                "persisted={persisted:?}"
            );
            assert_eq!(binding.first_user_title.as_deref(), Some("Draw a fox"));
        }

        // The public field remains safe even when a caller constructs a
        // legacy-shaped value directly instead of going through serde.
        let mut direct = LiveContainerBinding::default();
        direct.first_user_title = Some("Agent会话".into());
        assert!(direct.observe_first_user_title("Draw a fox"));
        assert_eq!(direct.first_user_title.as_deref(), Some("Draw a fox"));
    }
}
