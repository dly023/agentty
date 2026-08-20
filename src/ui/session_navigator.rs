use std::path::PathBuf;

use agentty_core::agent_runtime::{
    DiscoveryOutcome, DiscoveryRequest, LiveCarrier, LiveSession, NavigatorRow, NavigatorRowId,
    OperationId, RestoreOutcome, RowLifecycle, ScanGeneration, SessionIdentity, SessionNavigator,
    SessionReorderUnit, SessionTitleCandidates,
};
use agentty_core::core::environment::EnvironmentId;
use agentty_core::host::HostId;
use gpui::{AppContext as _, Context, PromptLevel, Window};
use gpui_component::WindowExt as _;
use gpui_component::input::InputState;

use crate::ui::app::{AgenttyApp, Tab, new_terminal};
use crate::ui::environment_session::EnvironmentSessionContext;
use crate::ui::pane::{Pane, PaneSlot};

fn live_title_candidates(
    binding: &agentty_core::core::session::LiveContainerBinding,
) -> SessionTitleCandidates {
    binding.title_candidates()
}

pub(crate) fn navigator_rows_share_session(a: &NavigatorRow, b: &NavigatorRow) -> bool {
    if a.row_id == b.row_id || a.identity == b.identity {
        return true;
    }
    match (&a.session_id, &b.session_id) {
        (Some(a_id), Some(b_id)) => a_id == b_id && a.agent == b.agent,
        _ => false,
    }
}

pub(crate) fn live_carrier_row_for_activation<'a>(
    rows: &'a [NavigatorRow],
    target: &NavigatorRow,
) -> Option<&'a NavigatorRow> {
    if target.carrier.is_some() {
        return None;
    }
    rows.iter().find(|row| {
        row.lifecycle == RowLifecycle::Live
            && row.carrier.is_some()
            && navigator_rows_share_session(row, target)
    })
}

/// Content-addressed gate for SESSION-PROJECTION-PAINT-GATE-53. Search query is
/// intentionally omitted — filtering is viewport-only.
pub(crate) fn session_navigator_input_fingerprint(
    environment: &EnvironmentId,
    history: &[agentty_core::agent_runtime::AgentSessionRecord],
    live: &[LiveSession],
    aliases: &[(agentty_core::agent_runtime::SessionIdentity, String)],
    pins: &[agentty_core::agent_runtime::SessionIdentity],
    display_orders: &[(agentty_core::agent_runtime::SessionIdentity, u64)],
) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    environment.as_str().hash(&mut hasher);
    history.len().hash(&mut hasher);
    for record in history {
        record.key.provider.hash(&mut hasher);
        record.key.session_id.hash(&mut hasher);
        record.updated_at_unix_ms.hash(&mut hasher);
        record.title.hash(&mut hasher);
        record.title_candidates.provider_title.hash(&mut hasher);
        record.title_candidates.first_user_title.hash(&mut hasher);
        record.cwd.hash(&mut hasher);
    }
    live.len().hash(&mut hasher);
    for session in live {
        hash_live_session(session, &mut hasher);
    }
    aliases.len().hash(&mut hasher);
    for (identity, alias) in aliases {
        hash_session_identity(identity, &mut hasher);
        alias.hash(&mut hasher);
    }
    pins.len().hash(&mut hasher);
    for identity in pins {
        hash_session_identity(identity, &mut hasher);
    }
    display_orders.len().hash(&mut hasher);
    for (identity, order) in display_orders {
        hash_session_identity(identity, &mut hasher);
        order.hash(&mut hasher);
    }
    hasher.finish()
}

fn hash_session_identity(identity: &SessionIdentity, hasher: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    match identity {
        SessionIdentity::Provider(key) => {
            0u8.hash(hasher);
            key.provider.hash(hasher);
            key.session_id.hash(hasher);
        }
        SessionIdentity::Durable(id) => {
            1u8.hash(hasher);
            id.hash(hasher);
        }
    }
}

fn hash_live_session(session: &LiveSession, hasher: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    hash_session_identity(&session.identity, hasher);
    format!("{:?}", session.agent).hash(hasher);
    session.session_id.hash(hasher);
    session.title.hash(hasher);
    session.title_candidates.provider_title.hash(hasher);
    session.title_candidates.first_user_title.hash(hasher);
    session.cwd.hash(hasher);
    session.launch_argv.hash(hasher);
    session.carrier.container_id.hash(hasher);
    session.carrier.tab_id.hash(hasher);
    session.carrier.pane_id.hash(hasher);
    match &session.execution {
        None => 0u8.hash(hasher),
        Some(execution) => {
            1u8.hash(hasher);
            format!("{:?}", execution.state.status).hash(hasher);
            execution.state.message.hash(hasher);
            execution.state.session_id.hash(hasher);
            execution.state.activity_seq.hash(hasher);
            execution.focused.hash(hasher);
            execution.unread.hash(hasher);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SessionSearchDocumentId {
    environment: agentty_core::core::environment::EnvironmentId,
    row_id: NavigatorRowId,
}

impl SessionSearchDocumentId {
    pub(crate) fn new(
        environment: agentty_core::core::environment::EnvironmentId,
        row_id: NavigatorRowId,
    ) -> Self {
        Self {
            environment,
            row_id,
        }
    }

    pub(crate) fn environment(&self) -> &agentty_core::core::environment::EnvironmentId {
        &self.environment
    }

    pub(crate) fn row_id(&self) -> &NavigatorRowId {
        &self.row_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionSearchDocument {
    pub(crate) id: SessionSearchDocumentId,
    pub(crate) workspace: crate::core::session::WorkspaceId,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) search_text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionViewportProjection {
    units: Vec<SessionReorderUnit>,
}

impl SessionViewportProjection {
    pub(crate) fn new(
        navigator: &SessionNavigator,
        mut matches: impl FnMut(&NavigatorRow) -> bool,
    ) -> Self {
        let units = navigator
            .reorder_units()
            .into_iter()
            .filter(|unit| {
                unit.row_ids
                    .iter()
                    .any(|row_id| navigator.detail_row(row_id).is_some_and(&mut matches))
            })
            .collect();
        Self { units }
    }

    pub(crate) fn len(&self) -> usize {
        self.units.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    pub(crate) fn unit(&self, index: usize) -> Option<&SessionReorderUnit> {
        self.units.get(index)
    }

    pub(crate) fn truncated(&self, limit: usize) -> Self {
        Self {
            units: self.units.iter().take(limit).cloned().collect(),
        }
    }

    pub(crate) fn unit_index_for_row(&self, row_id: &NavigatorRowId) -> Option<usize> {
        self.units
            .iter()
            .position(|unit| unit.row_ids.contains(row_id))
    }

    pub(crate) fn row_ids(&self) -> Vec<NavigatorRowId> {
        self.units
            .iter()
            .flat_map(|unit| unit.row_ids.iter().cloned())
            .collect()
    }

    pub(crate) fn rows_for_unit(
        &self,
        index: usize,
        navigator: &SessionNavigator,
    ) -> Option<Vec<NavigatorRow>> {
        self.unit(index)?
            .row_ids
            .iter()
            .map(|row_id| navigator.detail_row(row_id).cloned())
            .collect()
    }

    pub(crate) fn splice_delta(&self, previous: &Self) -> (std::ops::Range<usize>, usize) {
        let prefix = previous
            .units
            .iter()
            .zip(&self.units)
            .take_while(|(left, right)| left == right)
            .count();
        let suffix = previous.units[prefix..]
            .iter()
            .rev()
            .zip(self.units[prefix..].iter().rev())
            .take_while(|(left, right)| left == right)
            .count();
        (
            prefix..previous.units.len().saturating_sub(suffix),
            self.units.len().saturating_sub(prefix + suffix),
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionKeyboardCursor {
    row_id: Option<NavigatorRowId>,
}

impl SessionKeyboardCursor {
    pub(crate) fn current(&self) -> Option<&NavigatorRowId> {
        self.row_id.as_ref()
    }

    pub(crate) fn normalize(&mut self, visible: &[NavigatorRowId]) {
        if self
            .row_id
            .as_ref()
            .is_some_and(|current| !visible.contains(current))
        {
            self.row_id = None;
        }
    }

    pub(crate) fn move_by(
        &mut self,
        visible: &[NavigatorRowId],
        delta: isize,
    ) -> Option<NavigatorRowId> {
        if visible.is_empty() {
            self.row_id = None;
            return None;
        }
        let next = match self
            .row_id
            .as_ref()
            .and_then(|current| visible.iter().position(|row_id| row_id == current))
        {
            Some(index) => index
                .saturating_add_signed(delta)
                .min(visible.len().saturating_sub(1)),
            None if delta < 0 => visible.len().saturating_sub(1),
            None => 0,
        };
        self.row_id = Some(visible[next].clone());
        self.row_id.clone()
    }

    pub(crate) fn activation_target(
        &mut self,
        visible: &[NavigatorRowId],
    ) -> Option<NavigatorRowId> {
        self.normalize(visible);
        self.row_id.clone()
    }

    pub(crate) fn clear(&mut self) {
        self.row_id = None;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionRefreshState {
    requested: u64,
    inflight: Option<u64>,
    pending_passive: bool,
    scope: Option<SessionNavigatorScope>,
}

/// Immutable authority tuple captured when a Navigator scan starts. A
/// generation alone is insufficient because a workspace can be rebound while
/// an older Host operation is still completing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionNavigatorScope {
    pub(crate) workspace: crate::core::session::WorkspaceId,
    pub(crate) host: HostId,
    pub(crate) environment: EnvironmentId,
    pub(crate) alias_path: PathBuf,
}

impl SessionNavigatorScope {
    pub(crate) fn new(
        workspace: crate::core::session::WorkspaceId,
        host: HostId,
        environment: EnvironmentId,
        alias_path: PathBuf,
    ) -> Self {
        Self {
            workspace,
            host,
            environment,
            alias_path,
        }
    }

    pub(crate) fn matches(
        &self,
        workspace: crate::core::session::WorkspaceId,
        host: HostId,
        environment: &EnvironmentId,
        alias_path: &std::path::Path,
    ) -> bool {
        self.workspace == workspace
            && self.host == host
            && &self.environment == environment
            && self.alias_path == alias_path
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionRefreshRequest {
    Start(u64),
    Coalesced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionRefreshIntent {
    Explicit,
    InitialTargetReady,
    RemoteLinkUp,
    ProviderSourceMutation,
    AgentCarrierClosed,
    GenericTerminalEvent,
    LayoutOnly,
}

impl SessionRefreshIntent {
    fn explicit(self) -> Option<bool> {
        match self {
            Self::Explicit => Some(true),
            Self::InitialTargetReady
            | Self::RemoteLinkUp
            | Self::ProviderSourceMutation
            | Self::AgentCarrierClosed => Some(false),
            Self::GenericTerminalEvent | Self::LayoutOnly => None,
        }
    }
}

impl SessionRefreshState {
    pub(crate) fn request(&mut self, explicit: bool) -> SessionRefreshRequest {
        self.requested = self.requested.saturating_add(1);
        let generation = self.requested;
        if explicit {
            self.inflight = Some(generation);
            self.pending_passive = false;
            self.scope = None;
            SessionRefreshRequest::Start(generation)
        } else if self.inflight.is_some() {
            self.pending_passive = true;
            SessionRefreshRequest::Coalesced
        } else {
            self.inflight = Some(generation);
            self.scope = None;
            SessionRefreshRequest::Start(generation)
        }
    }

    pub(crate) fn bind_scope(&mut self, generation: u64, scope: SessionNavigatorScope) -> bool {
        if self.inflight != Some(generation) {
            return false;
        }
        self.scope = Some(scope);
        true
    }

    pub(crate) fn finish(&mut self, generation: u64) -> Option<u64> {
        if self.inflight != Some(generation) {
            return None;
        }
        self.inflight = None;
        self.scope = None;
        if self.pending_passive {
            self.pending_passive = false;
            self.requested = self.requested.saturating_add(1);
            self.inflight = Some(self.requested);
            Some(self.requested)
        } else {
            None
        }
    }

    pub(crate) fn accepts(&self, generation: u64) -> bool {
        self.inflight == Some(generation)
    }

    pub(crate) fn accepts_scope(&self, generation: u64, scope: &SessionNavigatorScope) -> bool {
        self.inflight == Some(generation) && self.scope.as_ref() == Some(scope)
    }

    pub(crate) fn abandon(&mut self, generation: u64) {
        if self.inflight == Some(generation) {
            self.inflight = None;
            self.pending_passive = false;
            self.scope = None;
        }
    }

    /// Invalidate every in-flight completion before a workspace/environment
    /// rebind. Bumping `requested` ensures a late callback cannot reuse the
    /// previous generation even if a caller accidentally restarts immediately.
    pub(crate) fn invalidate(&mut self) {
        self.requested = self.requested.saturating_add(1);
        self.inflight = None;
        self.pending_passive = false;
        self.scope = None;
    }

    pub(crate) fn is_inflight(&self) -> bool {
        self.inflight.is_some()
    }
}

fn discovery_message(outcome: &DiscoveryOutcome, cx: &gpui::App) -> String {
    use crate::core::i18n::{current, current_format};
    match outcome {
        DiscoveryOutcome::Complete(_) => current(cx, "navigator.scan_completed").into(),
        DiscoveryOutcome::Failed { message } => message.clone(),
        DiscoveryOutcome::SourceMissing { source } => {
            current_format(cx, "navigator.source_missing", &[("source", source)])
        }
        DiscoveryOutcome::Partial {
            failed_providers: failed,
        } => current_format(
            cx,
            "navigator.scan_incomplete",
            &[("failed", &failed.join(", "))],
        ),
        DiscoveryOutcome::Cancelled => current(cx, "navigator.scan_cancelled").into(),
        DiscoveryOutcome::SourceLimitExceeded { source, limit } => current_format(
            cx,
            "navigator.source_limit",
            &[("source", source), ("limit", &limit.to_string())],
        ),
    }
}

/// Map transport/operation timeouts to a stable i18n key so Discovering cannot
/// linger as a spinner with a raw TimedOut string (SESSION-DISCOVERY-TIMEOUT-VISIBLE-33).
pub(crate) fn is_discovery_timeout(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::TimedOut
        || error.to_string().to_ascii_lowercase().contains("timed out")
}

fn scan_error_message(error: &std::io::Error, cx: &gpui::App) -> String {
    if is_discovery_timeout(error) {
        crate::core::i18n::current(cx, "navigator.scan_timed_out").into()
    } else {
        error.to_string()
    }
}

fn live_binding_for_resume(
    agent: crate::core::cli_agent::CLIAgent,
    session_id: Option<String>,
    launch_argv: Vec<String>,
    title_candidates: SessionTitleCandidates,
) -> agentty_core::core::session::LiveContainerBinding {
    agentty_core::core::session::LiveContainerBinding::new_with_title_candidates(
        Some(agent),
        session_id,
        launch_argv,
        title_candidates,
    )
}

impl AgenttyApp {
    /// First paint and recover from a transient Host/roots resolve failure.
    /// When `session_scan_error` is set but EnvironmentSessionContext can now
    /// resolve, request RemoteLinkUp so remote discovery is not stuck empty.
    pub(crate) fn ensure_session_navigator_scan(&mut self, cx: &mut Context<Self>) {
        if !self.session_scan_started {
            if !self.remote_session_discovery_ready(cx) {
                return;
            }
            self.refresh_session_navigator_for(SessionRefreshIntent::InitialTargetReady, cx);
            return;
        }
        if self.session_scan_error.is_none() || self.session_refresh.is_inflight() {
            return;
        }
        let host_id = self.spawn_host(cx);
        if EnvironmentSessionContext::resolve(cx, host_id).is_err() {
            return;
        }
        self.session_scan_error = None;
        self.refresh_session_navigator_for(SessionRefreshIntent::RemoteLinkUp, cx);
    }

    pub(crate) fn refresh_session_navigator(&mut self, cx: &mut Context<Self>) {
        self.request_session_navigator_refresh(SessionRefreshIntent::Explicit, cx);
    }

    pub(crate) fn refresh_session_navigator_for(
        &mut self,
        intent: SessionRefreshIntent,
        cx: &mut Context<Self>,
    ) {
        self.request_session_navigator_refresh(intent, cx);
    }

    fn current_session_navigator_scope(&self, cx: &gpui::App) -> Option<SessionNavigatorScope> {
        let host = self.spawn_host(cx);
        let context = EnvironmentSessionContext::resolve(cx, host).ok()?;
        let environment = crate::core::session::WorkspaceStore::environment_id(cx, self.workspace);
        let alias_path = context.session_user_state_path();
        Some(SessionNavigatorScope::new(
            self.workspace,
            host,
            environment,
            alias_path,
        ))
    }

    fn cached_session_navigator_scope(&self, cx: &gpui::App) -> Option<SessionNavigatorScope> {
        Some(SessionNavigatorScope::new(
            self.workspace,
            self.spawn_host(cx),
            self.session_alias_environment.clone()?,
            self.session_user_state_path.clone()?,
        ))
    }

    pub(crate) fn session_navigator_scope_is_current(
        &self,
        scope: &SessionNavigatorScope,
        cx: &gpui::App,
    ) -> bool {
        self.current_session_navigator_scope(cx)
            .is_some_and(|current| current == *scope)
    }

    /// Clear every projection and cached Host path before a workspace/window
    /// rebind. The next workspace starts with an empty Navigator and requests
    /// a fresh Environment-scoped scan. Per-environment stash in
    /// `environment_navigator_cache` is preserved so switching back can restore
    /// a prior committed projection.
    pub(crate) fn invalidate_session_navigator_scope(&mut self, cx: &mut Context<Self>) {
        self.session_refresh.invalidate();
        self.session_history.clear();
        self.session_history_environment = None;
        self.session_user_state = Default::default();
        self.session_user_state_path = None;
        self.session_store_roots = None;
        self.session_alias_environment = None;
        self.session_alias_edit = None;
        self.pending_carrier_action = None;
        self.ssh_close_confirm = None;
        self.session_scan_started = false;
        self.session_scan_error = None;
        self.session_navigator = Default::default();
        self.session_navigator_input_fingerprint = None;
        self.session_keyboard_cursor.clear();
        cx.notify();
    }

    pub(crate) fn stash_environment_navigator(
        &mut self,
        environment: agentty_core::core::environment::EnvironmentId,
    ) {
        if !self.session_scan_started && self.session_history.is_empty() {
            return;
        }
        self.environment_navigator_cache.insert(
            environment,
            crate::ui::environment_navigator_cache::CachedEnvironmentNavigator {
                navigator: self.session_navigator.clone(),
                history: self.session_history.clone(),
                history_environment: self.session_history_environment.clone(),
                user_state: self.session_user_state.clone(),
                user_state_path: self.session_user_state_path.clone(),
                store_roots: self.session_store_roots.clone(),
                alias_environment: self.session_alias_environment.clone(),
                scan_error: self.session_scan_error.clone(),
                scan_started: self.session_scan_started,
            },
        );
    }

    pub(crate) fn restore_environment_navigator(
        &mut self,
        environment: &agentty_core::core::environment::EnvironmentId,
    ) -> bool {
        let Some(entry) = self.environment_navigator_cache.take(environment) else {
            return false;
        };
        self.session_navigator = entry.navigator;
        self.session_history = entry.history;
        self.session_history_environment = entry.history_environment;
        self.session_user_state = entry.user_state;
        self.session_user_state_path = entry.user_state_path;
        self.session_store_roots = entry.store_roots;
        self.session_alias_environment = entry.alias_environment;
        self.session_scan_error = entry.scan_error;
        self.session_scan_started = entry.scan_started;
        self.session_navigator_input_fingerprint = None;
        true
    }

    pub(crate) fn environment_session_counts(
        &self,
        environment: &agentty_core::core::environment::EnvironmentId,
        is_current: bool,
    ) -> crate::ui::environment_navigator_cache::EnvironmentSessionCounts {
        if is_current {
            return crate::ui::environment_navigator_cache::counts_from_navigator(
                &self.session_navigator,
            );
        }
        self.environment_navigator_cache
            .counts(environment)
            .unwrap_or(
                crate::ui::environment_navigator_cache::EnvironmentSessionCounts {
                    live: 0,
                    total: 0,
                },
            )
    }

    fn request_session_navigator_refresh(
        &mut self,
        intent: SessionRefreshIntent,
        cx: &mut Context<Self>,
    ) {
        let Some(explicit) = intent.explicit() else {
            return;
        };
        let SessionRefreshRequest::Start(generation) = self.session_refresh.request(explicit)
        else {
            return;
        };
        self.start_session_navigator_refresh(generation, cx);
    }

    fn start_session_navigator_refresh(&mut self, generation: u64, cx: &mut Context<Self>) {
        let host_id = self.spawn_host(cx);
        let context = match EnvironmentSessionContext::resolve(cx, host_id) {
            Ok(context) => context,
            Err(error) => {
                self.session_refresh.abandon(generation);
                self.session_scan_error = Some(error.message().to_string());
                cx.notify();
                return;
            }
        };
        self.session_scan_started = true;
        self.session_scan_error = None;
        let environment = crate::core::session::WorkspaceStore::environment_id(cx, self.workspace);
        let alias_path = context.session_user_state_path();
        let scope = SessionNavigatorScope::new(
            self.workspace,
            host_id,
            environment.clone(),
            alias_path.clone(),
        );
        if !self.session_refresh.bind_scope(generation, scope.clone()) {
            return;
        }
        let context = context.clone();
        let host = context.host.clone();
        let store_roots = context.store_roots.clone();
        let request = DiscoveryRequest::standard(store_roots.clone());
        let aliases_path = alias_path;
        let result_environment = environment.clone();
        crate::ui::host_ops::HostOps::run(
            host,
            cx,
            move |_host| {
                let aliases =
                    agentty_core::agent_runtime::SessionUserStateStore::load(_host, &aliases_path);
                let discovery = context.discover_sessions(
                    OperationId(generation),
                    ScanGeneration(generation),
                    request,
                );
                Ok::<_, std::io::Error>((
                    discovery,
                    aliases,
                    aliases_path,
                    result_environment,
                    store_roots,
                ))
            },
            move |this, result, cx| {
                if !this.session_refresh.accepts_scope(generation, &scope)
                    || !this.session_navigator_scope_is_current(&scope, cx)
                {
                    this.session_refresh.abandon(generation);
                    return;
                }
                match result {
                    Ok((
                        Ok(DiscoveryOutcome::Complete(rows)),
                        aliases,
                        path,
                        environment,
                        store_roots,
                    )) => {
                        this.session_history = rows;
                        this.session_history_environment = Some(environment.clone());
                        this.session_scan_error = None;
                        // Discovery is the content of the left navigation
                        // surface. A remote attach must not leave it hidden
                        // behind the user's previous collapsed state.
                        if crate::core::session::WorkspaceStore::remote_ref(cx, this.workspace)
                            .is_some()
                        {
                            this.sidebar_collapsed = false;
                            this.update_config(cx, |cfg| {
                                cfg.tab_bar_position = crate::core::config::TabBarPosition::Left;
                                cfg.sidebar_collapsed = false;
                            });
                        }
                        if let Ok(aliases) = aliases {
                            this.session_user_state = aliases;
                            this.session_user_state_path = Some(path);
                            this.session_store_roots = Some(store_roots);
                            this.session_alias_environment = Some(environment);
                        }
                    }
                    Ok((Ok(other), _, _, _, _)) => {
                        this.session_scan_error = Some(discovery_message(&other, cx));
                    }
                    Ok((Err(error), _, _, _, _)) | Err(error) => {
                        this.session_scan_error = Some(scan_error_message(&error, cx));
                    }
                }
                this.rebuild_session_navigator(cx);
                if let Some(next) = this.session_refresh.finish(generation) {
                    this.start_session_navigator_refresh(next, cx);
                }
                cx.notify();
            },
        );
    }

    pub(crate) fn rebuild_session_navigator(&mut self, cx: &mut gpui::App) {
        let live = self.live_session_rows(cx);
        let environment = crate::core::session::WorkspaceStore::environment_id(cx, self.workspace);
        let history: &[agentty_core::agent_runtime::AgentSessionRecord] =
            match self.session_history_environment.as_ref() {
                // Test fixtures and a freshly-created local window may seed the
                // history projection before the first scan has committed a scope.
                None => &self.session_history,
                Some(scoped) if scoped == &environment => &self.session_history,
                Some(_) => &[],
            };
        let aliases = if self.session_alias_environment.as_ref() == Some(&environment) {
            self.session_user_state
                .aliases_for_environment(&environment)
        } else {
            Vec::new()
        };
        let pins = if self.session_alias_environment.as_ref() == Some(&environment) {
            self.session_user_state.pins_for_environment(&environment)
        } else {
            Vec::new()
        };
        let display_orders = if self.session_alias_environment.as_ref() == Some(&environment) {
            self.session_user_state
                .display_orders_for_environment(&environment)
        } else {
            Vec::new()
        };
        let fingerprint = session_navigator_input_fingerprint(
            &environment,
            history,
            &live,
            &aliases,
            &pins,
            &display_orders,
        );
        if self.session_navigator_input_fingerprint == Some(fingerprint) {
            return;
        }
        self.session_navigator.refresh(history, &live);
        self.session_navigator.project_aliases(&aliases);
        self.session_navigator.project_pins(&pins);
        self.session_navigator
            .project_display_order(&display_orders);
        self.session_navigator_input_fingerprint = Some(fingerprint);

        let environment_label =
            crate::core::session::WorkspaceStore::remote_ref(cx, self.workspace)
                .map(|remote| crate::ui::remote_connect::label_for(&remote.target, cx))
                .unwrap_or_else(|| {
                    crate::core::i18n::current(cx, "environment.local.label").to_string()
                });
        let documents =
            self.committed_session_search_documents(environment.clone(), &environment_label, cx);
        crate::ui::windows::WindowRegistry::publish_session_documents(
            cx,
            self.workspace,
            environment,
            documents,
        );
    }

    fn committed_session_search_documents(
        &self,
        environment: agentty_core::core::environment::EnvironmentId,
        environment_label: &str,
        cx: &gpui::App,
    ) -> Vec<SessionSearchDocument> {
        self.session_navigator
            .rows()
            .iter()
            .map(|row| {
                let title =
                    row.display_title(&crate::core::i18n::current(cx, "session.default_name"));
                let mut subtitle_parts = vec![
                    row.agent.display_name().to_string(),
                    environment_label.to_string(),
                ];
                if let Some(cwd) = row.cwd.as_ref() {
                    subtitle_parts.push(cwd.clone());
                }
                let subtitle = subtitle_parts.join(" · ");
                let lifecycle = format!("{:?}", row.lifecycle);
                let search_text = [
                    title.as_str(),
                    row.alias.as_deref().unwrap_or_default(),
                    row.agent.display_name(),
                    row.session_id.as_deref().unwrap_or_default(),
                    row.cwd.as_deref().unwrap_or_default(),
                    environment_label,
                    lifecycle.as_str(),
                ]
                .join(" ");
                SessionSearchDocument {
                    id: SessionSearchDocumentId::new(environment.clone(), row.row_id.clone()),
                    workspace: self.workspace,
                    title,
                    subtitle,
                    search_text,
                }
            })
            .collect()
    }

    pub(crate) fn live_session_rows(&self, cx: &gpui::App) -> Vec<LiveSession> {
        let mut rows = Vec::new();
        for tab in &self.tabs {
            let tab_id = tab.tree_id.get().to_string();
            for slot in tab.pane.leaves() {
                let (binding, session, observed_agent, cwd, focused, unread, pane_id) = match &slot
                {
                    PaneSlot::Ready(terminal) => {
                        let view = terminal.read(cx);
                        (
                            view.live_binding().clone(),
                            view.agent_session(),
                            view.agent(),
                            view.cwd().map(|cwd| cwd.to_string_lossy().into_owned()),
                            view.is_focused(),
                            view.agent_result_unread(),
                            Some(view.pane_id()),
                        )
                    }
                    PaneSlot::Connecting(pending) => {
                        let pending = pending.read(cx);
                        (
                            pending.spawn.live_binding.clone(),
                            None,
                            pending.spawn.live_binding.agent,
                            pending
                                .spawn
                                .working_directory
                                .as_ref()
                                .map(|cwd| cwd.to_string_lossy().into_owned()),
                            false,
                            false,
                            pending.spawn.restore_pane,
                        )
                    }
                };
                let Some(agent) = binding.agent.or(observed_agent) else {
                    continue;
                };
                let session_id = session
                    .as_ref()
                    .and_then(|state| state.session_id.clone())
                    .or_else(|| binding.session_id.clone());
                let identity = session_id.as_deref().map_or_else(
                    || SessionIdentity::Durable(binding.container_id.clone()),
                    |id| {
                        SessionIdentity::Provider(agentty_core::agent_runtime::AgentSessionKey {
                            provider: agent.slug().into(),
                            session_id: id.into(),
                        })
                    },
                );
                let execution =
                    session
                        .as_ref()
                        .map(|state| agentty_core::agent_runtime::LiveExecutionState {
                            state: state.clone(),
                            focused,
                            unread,
                        });
                let title_candidates = live_title_candidates(&binding);
                rows.push(LiveSession {
                    identity,
                    agent,
                    session_id,
                    // Publish only typed provider/first-user evidence carried
                    // by the binding. Tab labels and terminal OSC chrome must
                    // never enter the session-title state.
                    title: title_candidates.resolved().map(str::to_owned),
                    title_candidates,
                    cwd,
                    launch_argv: session
                        .and_then(|state| state.launch_argv)
                        .filter(|argv| !argv.is_empty())
                        .unwrap_or_else(|| binding.launch_argv.clone()),
                    carrier: LiveCarrier {
                        container_id: binding.container_id,
                        tab_id: Some(tab_id.clone()),
                        pane_id,
                    },
                    execution,
                });
            }
        }
        rows
    }

    pub(crate) fn activate_navigator_row(
        &mut self,
        row_id: NavigatorRowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rebuild_session_navigator(cx);
        let row = self
            .session_navigator
            .rows()
            .iter()
            .find(|row| row.row_id == row_id)
            .cloned();
        let Some(row) = row else {
            return;
        };
        // Selection chrome is keyed by NavigatorRowId; always select before focus/resume.
        let _ = self.session_navigator.select(&row_id);
        if row.carrier.is_none() {
            let live_focus = live_carrier_row_for_activation(self.session_navigator.rows(), &row)
                .and_then(|live_row| {
                    live_row.carrier.as_ref().and_then(|carrier| {
                        carrier
                            .tab_id
                            .clone()
                            .map(|tab_id| (live_row.row_id.clone(), tab_id))
                    })
                });
            if let Some((live_row_id, tab_id)) = live_focus {
                let _ = self.session_navigator.select(&live_row_id);
                if let Some(index) = self
                    .tabs
                    .iter()
                    .position(|tab| Some(tab.tree_id.get().to_string()) == Some(tab_id.clone()))
                {
                    self.activate(index, window, cx);
                    cx.notify();
                    return;
                }
            }
        }
        if let Some(carrier) = row.carrier {
            if let Some(index) = self
                .tabs
                .iter()
                .position(|tab| Some(tab.tree_id.get().to_string()) == carrier.tab_id)
            {
                self.activate(index, window, cx);
            } else {
                window.push_notification(
                    crate::core::i18n::current(cx, "session.live_carrier_missing"),
                    cx,
                );
            }
            cx.notify();
            return;
        }
        let Some(invocation) = self.session_navigator.begin_restore(&row_id) else {
            cx.notify();
            return;
        };
        if !self.guard_local_spawn(window, cx) {
            let _ = self.session_navigator.finish_restore(
                &row_id,
                RestoreOutcome::Retryable("Environment unavailable".into()),
            );
            return;
        }
        let cwd = invocation.cwd.as_ref().map(PathBuf::from);
        let binding = live_binding_for_resume(
            row.agent,
            row.session_id.clone(),
            row.launch_argv.clone(),
            row.resume_title_candidates(),
        );
        let pane = match new_terminal(
            self.window_workspace(cx),
            Some(self.workspace),
            self.font_size,
            cwd,
            None,
            None,
            window,
            cx,
        ) {
            Ok(pane) => pane,
            Err(error) => {
                let _ = self
                    .session_navigator
                    .finish_restore(&row_id, RestoreOutcome::Retryable(error.to_string()));
                return;
            }
        };
        if let PaneSlot::Ready(terminal) = &pane {
            terminal.update(cx, |view, cx| view.set_live_binding(binding.clone(), cx));
        }
        match &pane {
            PaneSlot::Ready(terminal) => {
                terminal.read(cx).run_invocation(&invocation, cx);
            }
            PaneSlot::Connecting(pending) => {
                pending.update(cx, |pending, _| {
                    pending.spawn.resume_invocation = Some(invocation.clone());
                    pending.spawn.navigator_row_id = Some(row_id.clone());
                    pending.spawn.live_binding = binding.clone();
                });
            }
        }
        self.remember_active_pane(window, cx);
        let insert_at = self.new_tab_insert_at(cx);
        self.tabs.insert(insert_at, Tab::new(Pane::leaf(pane)));
        self.active = insert_at;
        if let Some(carrier) = self
            .live_session_rows(cx)
            .into_iter()
            .find(|live| live.identity == row.identity)
            .map(|live| live.carrier)
        {
            let _ = self
                .session_navigator
                .finish_restore(&row_id, RestoreOutcome::Success(carrier));
        }
        self.focus_active(window, cx);
        self.save_session(cx);
        self.rebuild_session_navigator(cx);
        cx.notify();
    }

    pub(crate) fn session_viewport_projection(&self, cx: &gpui::App) -> SessionViewportProjection {
        let query = self.sidebar_search.read(cx).value().trim().to_lowercase();
        SessionViewportProjection::new(&self.session_navigator, |row| {
            query.is_empty() || crate::ui::tab_sidebar::navigator_search_text(row).contains(&query)
        })
    }

    pub(crate) fn visible_session_row_ids(&self, cx: &gpui::App) -> Vec<NavigatorRowId> {
        self.session_viewport_projection(cx).row_ids()
    }

    pub(crate) fn apply_session_reorder(
        &mut self,
        order: &[usize],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.sidebar_search.read(cx).value().trim().is_empty() {
            return;
        }
        self.rebuild_session_navigator(cx);
        let units = self.session_navigator.reorder_units();
        if order.len() != units.len()
            || order.iter().any(|&index| index >= units.len())
            || order
                .iter()
                .enumerate()
                .all(|(index, &ordered)| index == ordered)
        {
            return;
        }
        let mut seen = std::collections::HashSet::new();
        if order.iter().any(|index| !seen.insert(*index)) {
            return;
        }
        let ordered_rows: Vec<_> = order
            .iter()
            .flat_map(|&index| units[index].row_ids.iter().cloned())
            .collect();
        let mut candidate_navigator = self.session_navigator.clone();
        let Ok(display_orders) = candidate_navigator.reorder(&ordered_rows) else {
            return;
        };
        let Some(environment) = self.session_alias_environment.clone() else {
            return;
        };
        let Some(path) = self.session_user_state_path.clone() else {
            return;
        };
        let Some(host) = self.active_host(cx) else {
            return;
        };
        let Some(scope) = self.cached_session_navigator_scope(cx) else {
            return;
        };
        let mut candidate_store = self.session_user_state.clone();
        candidate_store.replace_display_order(environment, display_orders);
        crate::ui::host_ops::HostOps::run_in(
            host,
            window,
            cx,
            move |host| candidate_store.save(host, &path).map(|_| candidate_store),
            move |this, result, window, cx| {
                if !this.session_navigator_scope_is_current(&scope, cx) {
                    return;
                }
                match result {
                    Ok(store) => {
                        this.session_user_state = store;
                        this.rebuild_session_navigator(cx);
                    }
                    Err(error) => {
                        let context = crate::core::i18n::current(cx, "notify.save_failed");
                        crate::ui::host_ops::HostOps::notify_err(window, cx, context, &error);
                    }
                }
                cx.notify();
            },
        );
    }

    pub(crate) fn normalize_session_keyboard_cursor(&mut self, cx: &gpui::App) {
        let visible = self.visible_session_row_ids(cx);
        self.session_keyboard_cursor.normalize(&visible);
    }

    pub(crate) fn move_session_keyboard_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let visible = self.visible_session_row_ids(cx);
        if let Some(row_id) = self.session_keyboard_cursor.move_by(&visible, delta)
            && let Some(index) = self
                .session_viewport_projection(cx)
                .unit_index_for_row(&row_id)
        {
            self.session_list_state.scroll_to_reveal_item(index);
        }
        cx.notify();
    }

    pub(crate) fn activate_session_keyboard_cursor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let visible = self.visible_session_row_ids(cx);
        if let Some(row_id) = self.session_keyboard_cursor.activation_target(&visible) {
            self.activate_navigator_row(row_id, window, cx);
        }
    }

    pub(crate) fn escape_session_keyboard_cursor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query_empty = self.sidebar_search.read(cx).value().is_empty();
        self.session_keyboard_cursor.clear();
        if query_empty {
            self.focus_active(window, cx);
        } else {
            self.sidebar_search
                .update(cx, |search, cx| search.set_value("", window, cx));
        }
        cx.notify();
    }

    pub(crate) fn begin_session_alias_edit(
        &mut self,
        row_id: NavigatorRowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rebuild_session_navigator(cx);
        let Some(row) = self
            .session_navigator
            .rows()
            .iter()
            .find(|row| row.row_id == row_id)
            .cloned()
        else {
            return;
        };
        // Seed the editor with the current display title; empty means clear alias.
        // Select the whole seed so one delete/backspace clears it for replacement.
        let current = row.display_title("");
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        input.update(cx, |state, cx| state.focus(window, cx));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _input, event: &gpui_component::input::InputEvent, window, cx| match event {
                gpui_component::input::InputEvent::PressEnter { .. } => {
                    this.commit_session_alias_edit(window, cx)
                }
                gpui_component::input::InputEvent::Blur => this.cancel_session_alias_edit(cx),
                _ => {}
            },
        );
        self.session_alias_edit = Some(crate::ui::app::SessionAliasEdit {
            row_id,
            identity: row.identity,
            input,
            _subscription: subscription,
        });
        // Dispatch only after the edit entity is installed. GPUI resolves the
        // action through the focused input, and doing this earlier races the
        // first render/focus transaction on fast machines.
        window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        cx.notify();
    }

    pub(crate) fn cancel_session_alias_edit(&mut self, cx: &mut Context<Self>) {
        self.session_alias_edit = None;
        cx.notify();
    }

    pub(crate) fn commit_session_alias_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = self.session_alias_edit.take() else {
            return;
        };
        let alias = edit.input.read(cx).value().to_string();
        let Some(environment) = self.session_alias_environment.clone() else {
            return;
        };
        let Some(path) = self.session_user_state_path.clone() else {
            return;
        };
        let Some(host) = self.active_host(cx) else {
            return;
        };
        let Some(scope) = self.cached_session_navigator_scope(cx) else {
            return;
        };
        let mut candidate = self.session_user_state.clone();
        if let Err(error) = candidate.set(environment, edit.identity, Some(alias)) {
            window.push_notification(error.to_string(), cx);
            return;
        }
        crate::ui::host_ops::HostOps::run_in(
            host,
            window,
            cx,
            move |host| candidate.save(host, &path).map(|_| candidate),
            move |this, result, window, cx| {
                if !this.session_navigator_scope_is_current(&scope, cx) {
                    return;
                }
                match result {
                    Ok(candidate) => {
                        this.session_user_state = candidate;
                        this.rebuild_session_navigator(cx);
                    }
                    Err(error) => {
                        let context = crate::core::i18n::current(cx, "notify.save_failed");
                        crate::ui::host_ops::HostOps::notify_err(window, cx, context, &error);
                    }
                }
                cx.notify();
            },
        );
    }
    pub(crate) fn toggle_session_pin(
        &mut self,
        row_id: NavigatorRowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rebuild_session_navigator(cx);
        let Some(row) = self.session_navigator.detail_row(&row_id).cloned() else {
            return;
        };
        let Some(environment) = self.session_alias_environment.clone() else {
            return;
        };
        let Some(path) = self.session_user_state_path.clone() else {
            return;
        };
        let Some(host) = self.active_host(cx) else {
            return;
        };
        let Some(scope) = self.cached_session_navigator_scope(cx) else {
            return;
        };
        let mut candidate = self.session_user_state.clone();
        candidate.set_pin(environment, row.identity, !row.pinned);
        crate::ui::host_ops::HostOps::run_in(
            host,
            window,
            cx,
            move |host| candidate.save(host, &path).map(|_| candidate),
            move |this, result, window, cx| {
                if !this.session_navigator_scope_is_current(&scope, cx) {
                    return;
                }
                match result {
                    Ok(candidate) => {
                        this.session_user_state = candidate;
                        this.rebuild_session_navigator(cx);
                    }
                    Err(error) => {
                        let context = crate::core::i18n::current(cx, "notify.save_failed");
                        crate::ui::host_ops::HostOps::notify_err(window, cx, context, &error);
                    }
                }
                cx.notify();
            },
        );
    }

    pub(crate) fn persist_live_binding_rebind(
        &mut self,
        binding: &agentty_core::core::session::LiveContainerBinding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(agent), Some(session_id)) = (binding.agent, binding.session_id.as_deref()) else {
            self.save_session(cx);
            return;
        };
        let Some(environment) = self.session_alias_environment.clone() else {
            self.save_session(cx);
            return;
        };
        let Some(path) = self.session_user_state_path.clone() else {
            self.save_session(cx);
            return;
        };
        let Some(host) = self.active_host(cx) else {
            self.save_session(cx);
            return;
        };
        let Some(scope) = self.cached_session_navigator_scope(cx) else {
            self.save_session(cx);
            return;
        };
        let from = SessionIdentity::Durable(binding.container_id.clone());
        let to = SessionIdentity::Provider(agentty_core::agent_runtime::AgentSessionKey {
            provider: agent.slug().into(),
            session_id: session_id.into(),
        });
        let mut candidate = self.session_user_state.clone();
        if !candidate.rebind_identity(&environment, &from, &to) {
            self.save_session(cx);
            return;
        }
        crate::ui::host_ops::HostOps::run_in(
            host,
            window,
            cx,
            move |host| candidate.save(host, &path).map(|_| candidate),
            move |this, result, window, cx| {
                if !this.session_navigator_scope_is_current(&scope, cx) {
                    return;
                }
                match result {
                    Ok(candidate) => {
                        this.session_user_state = candidate;
                        this.rebuild_session_navigator(cx);
                        this.save_session(cx);
                    }
                    Err(error) => {
                        let context = crate::core::i18n::current(cx, "notify.save_failed");
                        crate::ui::host_ops::HostOps::notify_err(window, cx, context, &error);
                    }
                }
                cx.notify();
            },
        );
    }

    fn carrier_token_for_row(
        &self,
        row: &NavigatorRow,
        scope: SessionNavigatorScope,
    ) -> Option<crate::ui::app::CarrierCloseToken> {
        Some(crate::ui::app::CarrierCloseToken {
            scope,
            row_id: row.row_id.clone(),
            identity: row.identity.clone(),
            carrier: row.carrier.clone()?,
        })
    }

    fn carrier_token_is_current(
        &self,
        token: &crate::ui::app::CarrierCloseToken,
        cx: &gpui::App,
    ) -> bool {
        self.session_navigator_scope_is_current(&token.scope, cx)
            && self
                .session_navigator
                .rows()
                .iter()
                .find(|row| row.row_id == token.row_id)
                .is_some_and(|row| {
                    row.identity == token.identity && row.carrier.as_ref() == Some(&token.carrier)
                })
    }

    fn notify_carrier_close_stale(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.push_notification(
            crate::core::i18n::current(cx, "session.live_carrier_missing"),
            cx,
        );
    }

    fn execute_pending_carrier_action(
        &mut self,
        action: crate::ui::app::PendingCarrierAction,
        ssh_confirmed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let token = match &action {
            crate::ui::app::PendingCarrierAction::SoftClose { token }
            | crate::ui::app::PendingCarrierAction::CloseAndDelete { token, .. } => token.clone(),
        };
        self.rebuild_session_navigator(cx);
        if !self.carrier_token_is_current(&token, cx) {
            self.notify_carrier_close_stale(window, cx);
            return;
        }
        match self.close_carrier_pane(&token, window, cx, ssh_confirmed) {
            crate::ui::app::CarrierCloseOutcome::NeedsSshConfirmation => {
                self.pending_carrier_action = Some(action);
                self.ssh_close_confirm = Some(crate::ui::app::SshCloseKind::Carrier);
                cx.notify();
            }
            crate::ui::app::CarrierCloseOutcome::Stale
            | crate::ui::app::CarrierCloseOutcome::Missing => {
                self.notify_carrier_close_stale(window, cx);
            }
            crate::ui::app::CarrierCloseOutcome::Closed => {
                let _ = self
                    .session_navigator
                    .handoff_closed_carrier(&token.row_id, &token.carrier);
                self.rebuild_session_navigator(cx);
                match action {
                    crate::ui::app::PendingCarrierAction::SoftClose { .. } => {
                        self.refresh_session_navigator_for(
                            SessionRefreshIntent::AgentCarrierClosed,
                            cx,
                        );
                    }
                    crate::ui::app::PendingCarrierAction::CloseAndDelete {
                        token,
                        plan,
                        host,
                        roots,
                        alias_path,
                        environment,
                        aliases,
                        source,
                        ..
                    } => self.start_captured_delete(
                        plan,
                        token.scope,
                        host,
                        roots,
                        alias_path,
                        environment,
                        aliases,
                        source,
                        window,
                        cx,
                    ),
                }
            }
        }
    }

    pub(crate) fn finish_pending_carrier_action(
        &mut self,
        action: crate::ui::app::PendingCarrierAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.execute_pending_carrier_action(action, true, window, cx);
    }

    fn start_captured_delete(
        &mut self,
        plan: agentty_core::agent_runtime::DeletePlan,
        scope: SessionNavigatorScope,
        host: crate::ui::host_ops::SharedHost,
        roots: agentty_core::agent_runtime::AgentStoreRoots,
        alias_path: PathBuf,
        environment: EnvironmentId,
        aliases: agentty_core::agent_runtime::SessionUserStateStore,
        source: Option<agentty_core::agent_runtime::SessionDeleteSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let planned_source = match source {
            Some(source) => Some(source),
            None => {
                match agentty_core::agent_runtime::plan_close_and_delete_source(&plan, &roots) {
                    Ok(source) => source,
                    Err(error) => {
                        let context = crate::core::i18n::current(cx, "notify.delete_failed");
                        crate::ui::host_ops::HostOps::notify_err(window, cx, context, &error);
                        return;
                    }
                }
            }
        };
        let delete_identity = plan.identity.clone();
        match planned_source {
            Some(source) => {
                let alias_path_for_op = alias_path.clone();
                let environment_for_op = environment.clone();
                let aliases_for_op = aliases.clone();
                let plan_for_land = plan.clone();
                let scope_for_land = scope.clone();
                crate::ui::host_ops::HostOps::run_in(
                    host,
                    window,
                    cx,
                    move |host| {
                        agentty_core::agent_runtime::apply_session_delete_transaction(
                            host,
                            &source,
                            &alias_path_for_op,
                            &aliases_for_op,
                            &environment_for_op,
                            &delete_identity,
                        )
                    },
                    move |this, result, window, cx| {
                        if !this.session_navigator_scope_is_current(&scope_for_land, cx) {
                            return;
                        }
                        match result {
                            Ok(updated_aliases) => {
                                this.session_user_state = updated_aliases;
                                this.session_navigator.commit_delete(&plan_for_land);
                                this.session_history.retain(|record| {
                                    record.key.provider != plan_for_land.agent.slug()
                                        || plan_for_land.session_id.as_deref()
                                            != Some(record.key.session_id.as_str())
                                });
                                this.save_session(cx);
                                this.rebuild_session_navigator(cx);
                                this.refresh_session_navigator_for(
                                    SessionRefreshIntent::ProviderSourceMutation,
                                    cx,
                                );
                            }
                            Err(error) => {
                                let context =
                                    crate::core::i18n::current(cx, "notify.delete_failed");
                                crate::ui::host_ops::HostOps::notify_err(
                                    window, cx, context, &error,
                                );
                            }
                        }
                        cx.notify();
                    },
                );
            }
            None => {
                let alias_path_for_op = alias_path.clone();
                let environment_for_op = environment.clone();
                let aliases_for_op = aliases.clone();
                let plan_for_land = plan.clone();
                let scope_for_land = scope;
                crate::ui::host_ops::HostOps::run_in(
                    host,
                    window,
                    cx,
                    move |host| {
                        agentty_core::agent_runtime::apply_session_user_state_delete(
                            host,
                            &alias_path_for_op,
                            &aliases_for_op,
                            &environment_for_op,
                            &delete_identity,
                        )
                    },
                    move |this, result, window, cx| {
                        if !this.session_navigator_scope_is_current(&scope_for_land, cx) {
                            return;
                        }
                        match result {
                            Ok(updated_aliases) => {
                                this.session_user_state = updated_aliases;
                                this.session_navigator.commit_delete(&plan_for_land);
                                this.session_history.retain(|record| {
                                    record.key.provider != plan_for_land.agent.slug()
                                        || plan_for_land.session_id.as_deref()
                                            != Some(record.key.session_id.as_str())
                                });
                                this.save_session(cx);
                                this.rebuild_session_navigator(cx);
                                this.refresh_session_navigator_for(
                                    SessionRefreshIntent::ProviderSourceMutation,
                                    cx,
                                );
                            }
                            Err(error) => {
                                let context =
                                    crate::core::i18n::current(cx, "notify.delete_failed");
                                crate::ui::host_ops::HostOps::notify_err(
                                    window, cx, context, &error,
                                );
                            }
                        }
                        cx.notify();
                    },
                );
            }
        }
    }

    pub(crate) fn close_live_session_row(
        &mut self,
        row_id: NavigatorRowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rebuild_session_navigator(cx);
        let Some(row) = self
            .session_navigator
            .rows()
            .iter()
            .find(|row| row.row_id == row_id)
            .cloned()
        else {
            return;
        };
        if row.lifecycle != agentty_core::agent_runtime::RowLifecycle::Live
            && row.lifecycle != agentty_core::agent_runtime::RowLifecycle::Restoring
        {
            return;
        }
        let Some(scope) = self.current_session_navigator_scope(cx) else {
            return;
        };
        let Some(token) = self.carrier_token_for_row(&row, scope) else {
            return;
        };
        let _ = self.session_navigator.select(&row_id);
        let title = row.display_title(crate::core::i18n::current(cx, "session.default_name"));
        let agent = row.agent.display_name().to_string();
        let body = crate::core::i18n::current_format(
            cx,
            "session.close_confirm_body",
            &[("agent", &agent), ("title", &title)],
        );
        let answer = window.prompt(
            PromptLevel::Warning,
            crate::core::i18n::current(cx, "session.close_confirm_title"),
            Some(&body),
            &[
                crate::core::i18n::current(cx, "common.cancel"),
                crate::core::i18n::current(cx, "common.close"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = answer.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if confirmed == Ok(1) {
                    this.execute_pending_carrier_action(
                        crate::ui::app::PendingCarrierAction::SoftClose { token },
                        false,
                        window,
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    pub(crate) fn close_and_delete_live_session_row(
        &mut self,
        row_id: NavigatorRowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rebuild_session_navigator(cx);
        let Some(row) = self
            .session_navigator
            .rows()
            .iter()
            .find(|row| row.row_id == row_id)
            .cloned()
        else {
            return;
        };
        let Some(plan) = self.session_navigator.plan_close_and_delete(&row_id) else {
            return;
        };
        let Some(scope) = self.current_session_navigator_scope(cx) else {
            return;
        };
        let Some(token) = self.carrier_token_for_row(&row, scope.clone()) else {
            return;
        };
        let Ok(context) = EnvironmentSessionContext::resolve(cx, scope.host) else {
            return;
        };
        let alias_path = context.session_user_state_path();
        let source = match agentty_core::agent_runtime::plan_close_and_delete_source(
            &plan,
            &context.store_roots,
        ) {
            Ok(source) => source,
            Err(error) => {
                let context = crate::core::i18n::current(cx, "notify.delete_failed");
                crate::ui::host_ops::HostOps::notify_err(window, cx, context, &error);
                return;
            }
        };
        let aliases = self.session_user_state.clone();
        let _ = self.session_navigator.select(&row_id);
        let title = row.display_title(crate::core::i18n::current(cx, "session.default_name"));
        let agent = plan.agent.display_name().to_string();
        let body = crate::core::i18n::current_format(
            cx,
            "session.close_and_delete_confirm_body",
            &[("agent", &agent), ("title", &title)],
        );
        let answer = window.prompt(
            PromptLevel::Warning,
            crate::core::i18n::current(cx, "session.close_and_delete_confirm_title"),
            Some(&body),
            &[
                crate::core::i18n::current(cx, "common.cancel"),
                crate::core::i18n::current(cx, "common.close_and_delete"),
            ],
            cx,
        );
        let action = crate::ui::app::PendingCarrierAction::CloseAndDelete {
            token,
            plan,
            host: context.host,
            roots: context.store_roots,
            alias_path,
            environment: scope.environment,
            aliases,
            source,
        };
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = answer.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if confirmed == Ok(1) {
                    this.execute_pending_carrier_action(action, false, window, cx);
                }
            });
        })
        .detach();
    }

    pub(crate) fn delete_session_row(
        &mut self,
        row_id: NavigatorRowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rebuild_session_navigator(cx);
        let Some(plan) = self.session_navigator.plan_delete(&row_id) else {
            return;
        };
        let title = plan.title.clone().unwrap_or_default();
        let agent = plan.agent.display_name().to_string();
        let body = crate::core::i18n::current_format(
            cx,
            "session.delete_confirm_body",
            &[("agent", &agent), ("title", &title)],
        );
        let answer = window.prompt(
            PromptLevel::Warning,
            crate::core::i18n::current(cx, "session.delete_confirm_title"),
            Some(&body),
            &[
                crate::core::i18n::current(cx, "common.cancel"),
                crate::core::i18n::current(cx, "common.delete"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = answer.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if confirmed != Ok(1) {
                    return;
                }
                this.rebuild_session_navigator(cx);
                let Some(plan) = this.session_navigator.plan_delete(&row_id) else {
                    return;
                };
                let Some(store_roots) = this.session_store_roots.clone() else {
                    return;
                };
                let source = match agentty_core::agent_runtime::plan_session_delete_source(
                    &plan,
                    &store_roots,
                ) {
                    Ok(source) => source,
                    Err(error) => {
                        let context = crate::core::i18n::current(cx, "notify.delete_failed");
                        crate::ui::host_ops::HostOps::notify_err(window, cx, context, &error);
                        return;
                    }
                };
                let Some(host) = this.active_host(cx) else {
                    return;
                };
                let Some(environment) = this.session_alias_environment.clone() else {
                    return;
                };
                let Some(alias_path) = this.session_user_state_path.clone() else {
                    return;
                };
                let aliases = this.session_user_state.clone();
                let delete_identity = plan.identity.clone();
                crate::ui::host_ops::HostOps::run_in(
                    host,
                    window,
                    cx,
                    move |host| {
                        agentty_core::agent_runtime::apply_session_delete_transaction(
                            host,
                            &source,
                            &alias_path,
                            &aliases,
                            &environment,
                            &delete_identity,
                        )
                    },
                    move |this, result, window, cx| {
                        match result {
                            Ok(aliases) => {
                                this.session_user_state = aliases;
                                this.session_navigator.commit_delete(&plan);
                                this.session_history.retain(|record| {
                                    record.key.provider != plan.agent.slug()
                                        || plan.session_id.as_deref()
                                            != Some(record.key.session_id.as_str())
                                });
                                this.save_session(cx);
                                this.rebuild_session_navigator(cx);
                                this.refresh_session_navigator_for(
                                    SessionRefreshIntent::ProviderSourceMutation,
                                    cx,
                                );
                            }
                            Err(error) => {
                                let context =
                                    crate::core::i18n::current(cx, "notify.delete_failed");
                                crate::ui::host_ops::HostOps::notify_err(
                                    window, cx, context, &error,
                                );
                            }
                        }
                        cx.notify();
                    },
                );
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

    #[test]
    fn generic_terminal_events_do_not_scan_provider_history() {
        assert_eq!(SessionRefreshIntent::GenericTerminalEvent.explicit(), None);
        assert_eq!(SessionRefreshIntent::LayoutOnly.explicit(), None);
    }

    #[test]
    fn discovery_timeout_maps_to_localized_scan_error() {
        let timed_out = std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "control request 7 timed out after 30s",
        );
        assert!(is_discovery_timeout(&timed_out));
        let other = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
        assert!(!is_discovery_timeout(&other));
        assert!(is_discovery_timeout(&std::io::Error::new(
            std::io::ErrorKind::Other,
            "remote helper timed out while scanning",
        )));
    }

    #[test]
    fn source_mutation_and_agent_close_trigger_passive_refresh() {
        assert_eq!(
            SessionRefreshIntent::ProviderSourceMutation.explicit(),
            Some(false)
        );
        assert_eq!(
            SessionRefreshIntent::AgentCarrierClosed.explicit(),
            Some(false)
        );
        assert_eq!(SessionRefreshIntent::RemoteLinkUp.explicit(), Some(false));
        assert_eq!(SessionRefreshIntent::Explicit.explicit(), Some(true));
    }

    #[test]
    fn passive_refreshes_coalesce_while_a_scan_is_running() {
        let mut state = SessionRefreshState::default();
        assert_eq!(state.request(false), SessionRefreshRequest::Start(1));
        assert_eq!(state.request(false), SessionRefreshRequest::Coalesced);
        assert_eq!(state.request(false), SessionRefreshRequest::Coalesced);
        assert_eq!(state.finish(1), Some(4));
        assert!(state.accepts(4));
        assert_eq!(state.finish(4), None);
    }

    #[test]
    fn explicit_refresh_supersedes_the_inflight_generation() {
        let mut state = SessionRefreshState::default();
        assert_eq!(state.request(false), SessionRefreshRequest::Start(1));
        assert_eq!(state.request(true), SessionRefreshRequest::Start(2));
        assert!(!state.accepts(1));
        assert!(state.accepts(2));
        assert_eq!(state.finish(1), None);
        assert_eq!(state.finish(2), None);
    }

    #[test]
    fn unavailable_remote_refresh_does_not_poison_link_up_refresh() {
        let mut state = SessionRefreshState::default();
        assert_eq!(state.request(false), SessionRefreshRequest::Start(1));
        state.abandon(1);
        assert!(!state.is_inflight());
        assert_eq!(state.request(false), SessionRefreshRequest::Start(2));
    }

    #[test]
    fn session_scan_retries_after_host_roots_become_ready() {
        let source = include_str!("session_navigator.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            prod.contains("fn ensure_session_navigator_scan")
                && prod.contains("SessionRefreshIntent::RemoteLinkUp")
                && prod.contains("session_scan_error"),
            "sidebar must RemoteLinkUp-retry after Host+roots become ready following scan_error"
        );
        let sidebar = include_str!("tab_sidebar.rs");
        let sidebar_prod = sidebar.split("#[cfg(test)]").next().unwrap_or(sidebar);
        assert!(
            sidebar_prod.contains("ensure_session_navigator_scan"),
            "tab_sidebar must call ensure_session_navigator_scan on paint"
        );
    }

    #[test]
    fn remote_session_discovery_waits_for_attach() {
        let app = include_str!("app.rs");
        let nav = include_str!("session_navigator.rs");
        assert!(app.contains("remote_session_discovery_ready"));
        assert!(nav.contains("remote_session_discovery_ready"));
    }

    #[test]
    fn connect_and_link_up_share_passive_refresh_intent() {
        assert_eq!(
            SessionRefreshIntent::InitialTargetReady.explicit(),
            Some(false)
        );
        assert_eq!(SessionRefreshIntent::RemoteLinkUp.explicit(), Some(false));
        let mut state = SessionRefreshState::default();
        assert_eq!(state.request(false), SessionRefreshRequest::Start(1));
        state.abandon(1);
        assert_eq!(
            state.request(
                SessionRefreshIntent::RemoteLinkUp
                    .explicit()
                    .expect("passive")
            ),
            SessionRefreshRequest::Start(2)
        );
    }

    #[test]
    fn refresh_scope_requires_workspace_host_environment_and_alias_path() {
        let workspace = crate::core::session::WorkspaceId::new();
        let host = HostId(41);
        let environment = EnvironmentId::local();
        let path = std::path::PathBuf::from("/target/.config/agentty/session-aliases.json");
        let scope = SessionNavigatorScope::new(workspace, host, environment.clone(), path.clone());
        assert!(scope.matches(workspace, host, &environment, &path));
        assert!(!scope.matches(
            crate::core::session::WorkspaceId::new(),
            host,
            &environment,
            &path,
        ));
        assert!(!scope.matches(workspace, HostId(42), &environment, &path,));
        assert!(!scope.matches(
            workspace,
            host,
            &"other".parse::<EnvironmentId>().unwrap(),
            &path,
        ));
        assert!(!scope.matches(
            workspace,
            host,
            &environment,
            std::path::Path::new("/other/session-aliases.json"),
        ));
    }

    #[test]
    fn navigator_scope_rejects_stale_completion_after_workspace_switch() {
        let workspace_a = crate::core::session::WorkspaceId::new();
        let workspace_b = crate::core::session::WorkspaceId::new();
        let scope_a = SessionNavigatorScope::new(
            workspace_a,
            HostId(41),
            EnvironmentId::local(),
            "/a/.config/agentty/session-aliases.json".into(),
        );
        let scope_b = SessionNavigatorScope::new(
            workspace_b,
            HostId(42),
            "remote-b".parse::<EnvironmentId>().unwrap(),
            "/b/.config/agentty/session-aliases.json".into(),
        );
        let mut state = SessionRefreshState::default();
        assert_eq!(state.request(false), SessionRefreshRequest::Start(1));
        assert!(state.bind_scope(1, scope_a.clone()));
        assert!(state.accepts_scope(1, &scope_a));

        // This models switch_workspace: the old completion may still return,
        // but its generation and scope are both invalidated before B starts.
        state.invalidate();
        assert!(!state.accepts_scope(1, &scope_a));
        assert_eq!(state.request(false), SessionRefreshRequest::Start(3));
        assert!(state.bind_scope(3, scope_b.clone()));
        assert!(!state.accepts_scope(3, &scope_a));
        assert!(state.accepts_scope(3, &scope_b));
    }

    #[test]
    fn session_history_is_cleared_when_workspace_scope_changes() {
        let source = include_str!("app.rs");
        let switch = source
            .split("pub(crate) fn switch_workspace(")
            .nth(1)
            .and_then(|tail| tail.split("pub(crate) fn adopt_workspace(").next())
            .expect("switch_workspace body");
        assert!(
            switch.contains("invalidate_session_navigator_scope"),
            "workspace switch must invalidate Navigator state before rebind"
        );
        assert!(
            switch.contains("stash_environment_navigator")
                && switch.contains("restore_environment_navigator"),
            "workspace switch must stash and restore per-environment Navigator cache"
        );
        assert!(
            switch.contains("refresh_session_navigator_for")
                && switch.contains("InitialTargetReady"),
            "new workspace must request a fresh initial scan"
        );
    }

    #[test]
    fn identical_projection_inputs_share_fingerprint() {
        let environment = EnvironmentId::local();
        let live = [live_fp("s1")];
        let a = session_navigator_input_fingerprint(&environment, &[], &live, &[], &[], &[]);
        let b = session_navigator_input_fingerprint(&environment, &[], &live, &[], &[], &[]);
        assert_eq!(a, b);
    }

    #[test]
    fn live_execution_change_updates_fingerprint() {
        let environment = EnvironmentId::local();
        let mut live = live_fp("s1");
        let before =
            session_navigator_input_fingerprint(&environment, &[], &[live.clone()], &[], &[], &[]);
        live.execution = Some(agentty_core::agent_runtime::LiveExecutionState {
            state: crate::core::cli_agent::AgentSessionState {
                status: crate::core::cli_agent::AgentStatus::Waiting,
                ..Default::default()
            },
            focused: false,
            unread: false,
        });
        let after = session_navigator_input_fingerprint(&environment, &[], &[live], &[], &[], &[]);
        assert_ne!(before, after);
    }

    #[test]
    fn rebuild_session_navigator_gates_on_input_fingerprint() {
        let source = include_str!("session_navigator.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(prod.contains("session_navigator_input_fingerprint"));
        assert!(prod.contains("session_navigator_input_fingerprint == Some(fingerprint)"));
        assert!(
            prod.contains("intentionally omitted"),
            "search query must not feed the rebuild fingerprint"
        );
    }

    fn live_fp(id: &str) -> LiveSession {
        LiveSession {
            identity: SessionIdentity::Provider(agentty_core::agent_runtime::AgentSessionKey {
                provider: "codex".into(),
                session_id: id.into(),
            }),
            agent: crate::core::cli_agent::CLIAgent::Codex,
            session_id: Some(id.into()),
            title: Some(id.into()),
            title_candidates: Default::default(),
            cwd: Some("/repo".into()),
            launch_argv: vec![],
            carrier: LiveCarrier {
                container_id: format!("container-{id}"),
                tab_id: Some("tab".into()),
                pane_id: Some(1),
            },
            execution: None,
        }
    }
}

#[cfg(test)]
mod session_keyboard_cursor_tests {
    use super::SessionKeyboardCursor;
    use agentty_core::agent_runtime::{NavigatorRowId, SessionTitleCandidates};

    fn row(id: &str) -> NavigatorRowId {
        let mut navigator = agentty_core::agent_runtime::SessionNavigator::default();
        navigator.refresh(
            &[agentty_core::agent_runtime::AgentSessionRecord {
                key: agentty_core::agent_runtime::AgentSessionKey {
                    provider: "codex".into(),
                    session_id: id.into(),
                },
                agent: crate::core::cli_agent::CLIAgent::Codex,
                title: None,
                title_candidates: Default::default(),
                cwd: None,
                updated_at_unix_ms: None,
                launch_argv: Vec::new(),
                source_path: None,
                created_at_unix_ms: None,
            }],
            &[],
        );
        navigator.rows()[0].row_id.clone()
    }

    #[test]
    fn keyboard_cursor_uses_row_identity_not_list_index() {
        let a = row("a");
        let b = row("b");
        let c = row("c");
        let mut cursor = SessionKeyboardCursor::default();
        assert_eq!(
            cursor.move_by(&[a.clone(), b.clone(), c.clone()], 1),
            Some(a.clone())
        );
        assert_eq!(
            cursor.move_by(&[c.clone(), a.clone(), b.clone()], 1),
            Some(b.clone())
        );
        assert_eq!(cursor.current(), Some(&b));
        let mut navigator = agentty_core::agent_runtime::SessionNavigator::default();
        navigator.refresh(
            &[agentty_core::agent_runtime::AgentSessionRecord {
                key: agentty_core::agent_runtime::AgentSessionKey {
                    provider: "codex".into(),
                    session_id: "selection-proof".into(),
                },
                agent: crate::core::cli_agent::CLIAgent::Codex,
                title: None,
                title_candidates: Default::default(),
                cwd: None,
                updated_at_unix_ms: None,
                launch_argv: Vec::new(),
                source_path: None,
                created_at_unix_ms: None,
            }],
            &[],
        );
        assert!(navigator.selected().is_none());
    }

    #[test]
    fn enter_activates_filtered_cursor_and_escape_clears_transient_state() {
        let a = row("a");
        let b = row("b");
        let mut cursor = SessionKeyboardCursor::default();
        cursor.move_by(&[a, b.clone()], 1);
        cursor.move_by(&[b.clone()], 1);
        assert_eq!(cursor.activation_target(&[b.clone()]), Some(b));
        cursor.clear();
        assert!(cursor.current().is_none());
    }

    #[test]
    fn resume_carrier_is_bound_before_first_projection() {
        let binding = super::live_binding_for_resume(
            crate::core::cli_agent::CLIAgent::Codex,
            Some("session-1".into()),
            vec!["codex".into(), "resume".into(), "session-1".into()],
            SessionTitleCandidates::from_raw(None, Some("Draw a fox")),
        );
        assert!(binding.agent.is_some());
        assert_eq!(binding.session_id.as_deref(), Some("session-1"));
        assert!(!binding.container_id.is_empty());
        assert_eq!(binding.first_user_title(), Some("Draw a fox"));
    }
}

#[cfg(test)]
mod resume_title_seed_tests {
    use super::{live_binding_for_resume, live_title_candidates};
    use agentty_core::agent_runtime::{
        AgentSessionKey, AgentSessionRecord, LiveCarrier, LiveSession, NavigatorRow,
        SessionIdentity, SessionNavigator, SessionTitleCandidates,
    };
    use agentty_core::core::session::LiveContainerBinding;

    fn row(id: &str, title: Option<&str>, candidates: SessionTitleCandidates) -> NavigatorRow {
        let mut navigator = SessionNavigator::default();
        navigator.refresh(
            &[AgentSessionRecord {
                key: AgentSessionKey {
                    provider: "codex".into(),
                    session_id: id.into(),
                },
                agent: crate::core::cli_agent::CLIAgent::Codex,
                title: title.map(str::to_owned),
                title_candidates: candidates,
                cwd: Some("/work".into()),
                updated_at_unix_ms: Some(1),
                launch_argv: vec!["codex".into(), "--resume".into()],
                source_path: None,
                created_at_unix_ms: None,
            }],
            &[],
        );
        navigator
            .rows()
            .first()
            .cloned()
            .expect("resume row is discoverable")
    }

    fn resumed_binding(row: &NavigatorRow) -> LiveContainerBinding {
        live_binding_for_resume(
            row.agent,
            row.session_id.clone(),
            row.launch_argv.clone(),
            row.resume_title_candidates(),
        )
    }

    fn live_after_prompt(mut binding: LiveContainerBinding, prompt: &str) -> LiveSession {
        let _ = binding.observe_first_user_title(prompt);
        let candidates = live_title_candidates(&binding);
        LiveSession {
            identity: SessionIdentity::Durable(binding.container_id.clone()),
            agent: binding.agent.expect("resume binding carries agent"),
            session_id: None,
            title: candidates.resolved().map(str::to_owned),
            title_candidates: candidates,
            cwd: None,
            launch_argv: binding.launch_argv.clone(),
            carrier: LiveCarrier {
                container_id: binding.container_id,
                tab_id: Some("resume-tab".into()),
                pane_id: Some(7),
            },
            execution: None,
        }
    }

    fn title_after_history_gap(row: &NavigatorRow, prompt: &str) -> String {
        let live = live_after_prompt(resumed_binding(row), prompt);
        let mut navigator = SessionNavigator::default();
        navigator.refresh(&[], &[live]);
        navigator.rows()[0].display_title("Unnamed")
    }

    #[test]
    fn resume_legacy_title_seed_survives_post_resume_prompt() {
        let row = row(
            "legacy-resume",
            Some("Original request"),
            SessionTitleCandidates::default(),
        );
        let mut binding = resumed_binding(&row);

        // Model a successful post-resume Composer delivery.  It must not be
        // mistaken for the historical session's first user message merely
        // because the old record had only `title` on disk.
        assert!(
            !binding.observe_first_user_title("Second request"),
            "resume must seed the stable legacy title before a new prompt"
        );
        assert_eq!(
            binding.provider_title(),
            Some("Original request"),
            "a post-resume prompt must not rename a legacy-title session"
        );
        assert_eq!(
            binding.first_user_title(),
            None,
            "legacy title must remain provider/unknown evidence, not fake first-user evidence"
        );
    }

    #[test]
    fn resume_provider_only_title_seed_survives_history_gap_and_second_prompt() {
        let row = row(
            "provider-resume",
            Some("Provider title"),
            SessionTitleCandidates::from_raw(Some("Provider title"), None),
        );
        assert_eq!(
            title_after_history_gap(&row, "Second request"),
            "Provider title"
        );
    }

    #[test]
    fn resume_typed_first_user_seed_remains_write_once_after_second_prompt() {
        let row = row(
            "first-user-resume",
            Some("First request"),
            SessionTitleCandidates::from_raw(None, Some("First request")),
        );
        assert_eq!(
            title_after_history_gap(&row, "Second request"),
            "First request"
        );
    }

    #[test]
    fn resume_both_title_candidates_keep_provider_precedence_after_second_prompt() {
        let row = row(
            "both-resume",
            Some("Provider title"),
            SessionTitleCandidates::from_raw(Some("Provider title"), Some("First request")),
        );
        assert_eq!(
            title_after_history_gap(&row, "Second request"),
            "Provider title"
        );
    }
}

#[cfg(test)]
mod session_viewport_projection_tests {
    use super::SessionViewportProjection;
    use agentty_core::agent_runtime::{
        AgentSessionKey, AgentSessionRecord, LiveCarrier, LiveSession, SessionIdentity,
        SessionNavigator,
    };

    fn record(id: &str, title: &str) -> AgentSessionRecord {
        AgentSessionRecord {
            key: AgentSessionKey {
                provider: "codex".into(),
                session_id: id.into(),
            },
            agent: crate::core::cli_agent::CLIAgent::Codex,
            title: Some(title.into()),
            title_candidates: Default::default(),
            cwd: None,
            updated_at_unix_ms: None,
            launch_argv: Vec::new(),
            source_path: None,
            created_at_unix_ms: None,
        }
    }

    fn live(id: &str, tab_id: &str, title: &str) -> LiveSession {
        LiveSession {
            identity: SessionIdentity::Durable(id.into()),
            agent: crate::core::cli_agent::CLIAgent::Codex,
            session_id: None,
            title: Some(title.into()),
            title_candidates: Default::default(),
            cwd: None,
            launch_argv: vec!["codex".into()],
            carrier: LiveCarrier {
                container_id: id.into(),
                tab_id: Some(tab_id.into()),
                pane_id: None,
            },
            execution: None,
        }
    }

    #[test]
    fn viewport_indices_never_become_row_identity() {
        let mut navigator = SessionNavigator::default();
        navigator.refresh(
            &[
                record("a", "alpha"),
                record("b", "beta"),
                record("c", "gamma"),
            ],
            &[],
        );
        let before = SessionViewportProjection::new(&navigator, |_| true);
        let stable = before.unit(1).unwrap().row_ids[0].clone();
        let mut reordered = navigator
            .rows()
            .iter()
            .map(|row| row.row_id.clone())
            .collect::<Vec<_>>();
        let old_index = reordered.iter().position(|id| id == &stable).unwrap();
        reordered.remove(old_index);
        reordered.insert(0, stable.clone());
        navigator.reorder(&reordered).unwrap();

        let after = SessionViewportProjection::new(&navigator, |_| true);
        assert_eq!(after.unit(0).unwrap().row_ids[0], stable);
        assert_ne!(
            before.unit(0).unwrap().row_ids,
            after.unit(0).unwrap().row_ids
        );
    }

    #[test]
    fn offscreen_rows_are_not_built_but_keep_canonical_order() {
        let mut navigator = SessionNavigator::default();
        let records = (0..100)
            .map(|index| record(&format!("session-{index}"), &format!("title-{index}")))
            .collect::<Vec<_>>();
        navigator.refresh(&records, &[]);
        let projection = SessionViewportProjection::new(&navigator, |_| true);
        let canonical = navigator
            .rows()
            .iter()
            .map(|row| row.row_id.clone())
            .collect::<Vec<_>>();

        let mut built = Vec::new();
        for index in 40..43 {
            built.push(
                projection
                    .rows_for_unit(index, &navigator)
                    .unwrap()
                    .into_iter()
                    .map(|row| row.row_id.clone())
                    .collect::<Vec<_>>(),
            );
        }

        assert_eq!(built.len(), 3);
        assert_eq!(projection.row_ids(), canonical);
    }

    #[test]
    fn filtered_projection_keeps_split_siblings_as_one_unit() {
        let mut navigator = SessionNavigator::default();
        navigator.refresh(
            &[],
            &[
                live("left", "tab-a", "needle"),
                live("right", "tab-a", "other"),
                live("outside", "tab-b", "outside"),
            ],
        );
        let projection = SessionViewportProjection::new(&navigator, |row| {
            row.title.as_deref() == Some("needle")
        });

        assert_eq!(projection.len(), 1);
        assert_eq!(projection.unit(0).unwrap().row_ids.len(), 2);
    }
}

#[cfg(test)]
mod session_search_identity_tests {
    use super::SessionSearchDocumentId;
    use agentty_core::agent_runtime::{AgentSessionKey, AgentSessionRecord, SessionNavigator};
    use agentty_core::core::environment::EnvironmentId;

    #[test]
    fn live_session_rows_omit_tab_and_terminal_titles() {
        let source = include_str!("session_navigator.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        let block = prod
            .split("fn live_session_rows")
            .nth(1)
            .expect("live_session_rows present");
        assert!(
            block.contains("live_title_candidates") && block.contains("title_candidates"),
            "live projection may publish only typed first-user title candidates"
        );
        assert!(
            !block.contains("tab.name")
                && !block.contains("view.title")
                && !block.contains("\"agentty\""),
            "live_session_rows must not publish tab labels, OSC titles, or product placeholders"
        );
    }

    #[test]
    fn begin_session_alias_edit_selects_existing_display_title() {
        let source = include_str!("session_navigator.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        let block = prod
            .split("fn begin_session_alias_edit")
            .nth(1)
            .expect("begin_session_alias_edit present");
        assert!(
            block.contains("SelectAll") && block.contains("default_value(current)"),
            "alias edit must seed the display title and select it for one-key clear"
        );
    }

    #[test]
    fn close_and_delete_handles_missing_provider_source() {
        let source = include_str!("session_navigator.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        let block = prod
            .split("fn start_captured_delete(")
            .nth(1)
            .expect("start_captured_delete present");
        assert!(
            block.contains("plan_close_and_delete_source")
                && block.contains("apply_session_user_state_delete"),
            "Close and Delete must tombstone without a false failure when no provider source exists"
        );
    }

    #[test]
    fn split_navigator_soft_close_targets_only_selected_carrier() {
        let source = include_str!("session_navigator.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        let block = prod
            .split("fn close_live_session_row")
            .nth(1)
            .expect("close_live_session_row present");
        assert!(
            block.contains("execute_pending_carrier_action"),
            "soft Close must route through the typed pane-carrier action"
        );
        assert!(
            !block.contains("close_tab(index"),
            "soft Close must not close an entire split tab through a stale index"
        );
        assert!(
            block.contains("rebuild_session_navigator"),
            "soft Close must re-resolve the stable row/carrier after confirmation"
        );
    }

    #[test]
    fn split_navigator_close_and_delete_re_resolves_carrier() {
        let source = include_str!("session_navigator.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        let block = prod
            .split("fn close_and_delete_live_session_row")
            .nth(1)
            .expect("close_and_delete_live_session_row present");
        assert!(
            block.contains("execute_pending_carrier_action"),
            "Close and Delete must route through the typed pane-carrier action"
        );
        assert!(
            !block.contains("let tab_index"),
            "carrier identity must be re-resolved after confirmation, not captured as a numeric tab index"
        );
        assert!(
            block.contains("plan_close_and_delete"),
            "Close and Delete must capture its typed delete plan before closing"
        );
    }

    #[test]
    fn close_and_delete_carries_captured_host_scope_after_rebind() {
        let source = include_str!("session_navigator.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        let block = prod
            .split("fn close_and_delete_live_session_row")
            .nth(1)
            .expect("close_and_delete_live_session_row present");
        assert!(
            block.contains("SessionNavigatorScope") || block.contains("scope"),
            "Close and Delete must capture an immutable Navigator scope before prompting"
        );
        assert!(
            block.contains("PendingCarrierAction") || block.contains("CarrierCloseToken"),
            "Close and Delete must carry a typed pending action through SSH confirmation"
        );
        assert!(
            block.contains("session_store_roots") && block.contains("session_user_state_path"),
            "the original Host-backed roots and alias path must be captured, not resolved after rebind"
        );
        let delete = prod
            .split("fn start_captured_delete(")
            .nth(1)
            .expect("start_captured_delete present");
        assert!(
            delete.contains("session_navigator_scope_is_current"),
            "a delayed delete callback must not mutate a newly rebound Environment"
        );
    }

    #[test]
    fn close_and_delete_aborts_when_carrier_token_is_stale() {
        let source = include_str!("session_navigator.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        let block = prod
            .split("fn close_and_delete_live_session_row")
            .nth(1)
            .expect("close_and_delete_live_session_row present");
        assert!(
            block.contains("carrier") && block.contains("identity"),
            "Close and Delete must inspect the current carrier and identity"
        );
        assert!(
            block.contains("stale")
                || block.contains("expected_carrier")
                || block.contains("carrier_token_for_row")
                || block.contains("CarrierCloseToken"),
            "a replaced carrier must fail closed instead of deleting the captured plan"
        );
        assert!(
            block.contains("execute_pending_carrier_action"),
            "the guarded path must defer mutation until the token is revalidated"
        );
        let action = prod
            .split("fn execute_pending_carrier_action(")
            .nth(1)
            .and_then(|tail| {
                tail.split("\n    pub(crate) fn finish_pending_carrier_action")
                    .next()
            })
            .expect("execute_pending_carrier_action present");
        assert!(
            action.contains("carrier_token_is_current")
                && action.contains("handoff_closed_carrier"),
            "the typed action must fail closed before the canonical mutation and hand off only after close"
        );
    }

    #[test]
    fn session_search_identity_combines_environment_and_row_id() {
        let mut navigator = SessionNavigator::default();
        navigator.refresh(
            &[AgentSessionRecord {
                key: AgentSessionKey {
                    provider: "codex".into(),
                    session_id: "same-native-id".into(),
                },
                agent: crate::core::cli_agent::CLIAgent::Codex,
                title: None,
                title_candidates: Default::default(),
                cwd: None,
                updated_at_unix_ms: None,
                launch_argv: Vec::new(),
                source_path: None,
                created_at_unix_ms: None,
            }],
            &[],
        );
        let row_id = navigator.rows()[0].row_id.clone();
        let local = SessionSearchDocumentId::new(EnvironmentId::local(), row_id.clone());
        let remote =
            SessionSearchDocumentId::new("ssh:build".parse::<EnvironmentId>().unwrap(), row_id);

        assert_ne!(local, remote);
        assert_eq!(local.environment().as_str(), "local");
    }
}

#[cfg(test)]
mod activate_dedup_tests {
    use super::{live_carrier_row_for_activation, navigator_rows_share_session};
    use agentty_core::agent_runtime::RowLifecycle;
    use agentty_core::agent_runtime::SessionNavigator;

    fn history(id: &str) -> agentty_core::agent_runtime::AgentSessionRecord {
        agentty_core::agent_runtime::AgentSessionRecord {
            key: agentty_core::agent_runtime::AgentSessionKey {
                provider: "codex".into(),
                session_id: id.into(),
            },
            agent: agentty_core::core::cli_agent::CLIAgent::Codex,
            title: Some(id.into()),
            title_candidates: agentty_core::agent_runtime::SessionTitleCandidates::default(),
            cwd: Some("/repo".into()),
            updated_at_unix_ms: Some(1),
            launch_argv: vec![],
            source_path: Some(format!("/tmp/{id}")),
            created_at_unix_ms: None,
        }
    }

    fn live(id: &str, tab: &str, pane: u64) -> agentty_core::agent_runtime::LiveSession {
        agentty_core::agent_runtime::LiveSession {
            identity: agentty_core::agent_runtime::SessionIdentity::Provider(history(id).key),
            agent: agentty_core::core::cli_agent::CLIAgent::Codex,
            session_id: Some(id.into()),
            title: Some(id.into()),
            title_candidates: agentty_core::agent_runtime::SessionTitleCandidates::default(),
            cwd: Some("/repo".into()),
            launch_argv: vec![],
            carrier: agentty_core::agent_runtime::LiveCarrier {
                container_id: format!("container-{id}"),
                tab_id: Some(tab.into()),
                pane_id: Some(pane),
            },
            execution: None,
        }
    }

    #[test]
    fn activate_virtual_while_live_exists_focuses_existing_tab() {
        let mut virtual_model = SessionNavigator::default();
        virtual_model.refresh(&[history("s1")], &[]);
        let virtual_row = virtual_model.rows()[0].clone();
        let mut live_model = SessionNavigator::default();
        live_model.refresh(&[], &[live("s1", "tab-live", 1)]);
        let live = live_model.rows()[0].clone();
        assert!(navigator_rows_share_session(&virtual_row, &live));
        let rows = [virtual_row.clone(), live.clone()];
        let resolved = live_carrier_row_for_activation(&rows, &virtual_row).expect("live carrier");
        assert_eq!(resolved.lifecycle, RowLifecycle::Live);
        assert!(resolved.carrier.is_some());
        assert_eq!(
            resolved.carrier.as_ref().and_then(|c| c.tab_id.as_deref()),
            Some("tab-live")
        );
    }
}

#[cfg(test)]
mod session_live_title_tests {
    use super::live_title_candidates;
    use agentty_core::agent_runtime::SessionTitleCandidates;
    use agentty_core::core::session::LiveContainerBinding;

    #[test]
    fn pending_connecting_projects_binding_first_user_title() {
        let mut binding = LiveContainerBinding::default();
        assert!(binding.observe_first_user_title("Draw a fox"));
        let candidates = live_title_candidates(&binding);
        assert_eq!(
            candidates,
            SessionTitleCandidates::from_raw(None, Some("Draw a fox"))
        );
        assert_eq!(candidates.resolved(), Some("Draw a fox"));
    }
}
