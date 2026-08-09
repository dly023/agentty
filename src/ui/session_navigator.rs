use std::path::PathBuf;

use agentty_core::agent_runtime::{
    DiscoveryOutcome, DiscoveryRequest, LiveCarrier, LiveSession, NavigatorRow, NavigatorRowId,
    OperationId, RestoreOutcome, ScanGeneration, SessionIdentity, SessionNavigator,
    SessionReorderUnit,
};
use gpui::{AppContext as _, Context, PromptLevel, Window};
use gpui_component::WindowExt as _;
use gpui_component::input::InputState;

use crate::ui::app::{AgenttyApp, Tab, new_terminal};
use crate::ui::environment_session::EnvironmentSessionContext;
use crate::ui::pane::{Pane, PaneSlot};

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionRefreshState {
    requested: u64,
    inflight: Option<u64>,
    pending_passive: bool,
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
            SessionRefreshRequest::Start(generation)
        } else if self.inflight.is_some() {
            self.pending_passive = true;
            SessionRefreshRequest::Coalesced
        } else {
            self.inflight = Some(generation);
            SessionRefreshRequest::Start(generation)
        }
    }

    pub(crate) fn finish(&mut self, generation: u64) -> Option<u64> {
        if self.inflight != Some(generation) {
            return None;
        }
        self.inflight = None;
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

    pub(crate) fn abandon(&mut self, generation: u64) {
        if self.inflight == Some(generation) {
            self.inflight = None;
            self.pending_passive = false;
        }
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
) -> agentty_core::core::session::LiveContainerBinding {
    agentty_core::core::session::LiveContainerBinding::new(Some(agent), session_id, launch_argv)
}

impl AgenttyApp {
    /// First paint and recover from a transient Host/roots resolve failure.
    /// When `session_scan_error` is set but EnvironmentSessionContext can now
    /// resolve, request RemoteLinkUp so remote discovery is not stuck empty.
    pub(crate) fn ensure_session_navigator_scan(&mut self, cx: &mut Context<Self>) {
        if !self.session_scan_started {
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
        let alias_path =
            agentty_core::agent_runtime::SessionUserStateStore::path(&*context.host, &context.home);
        let host = context.host.clone();
        let store_roots = context.store_roots.clone();
        let request = DiscoveryRequest::standard(store_roots.clone());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let aliases = agentty_core::agent_runtime::SessionUserStateStore::load(
                        &*host,
                        &alias_path,
                    );
                    let discovery = context.discover_sessions(
                        OperationId(generation),
                        ScanGeneration(generation),
                        request,
                    );
                    Ok::<_, std::io::Error>((
                        discovery,
                        aliases,
                        alias_path,
                        environment,
                        store_roots,
                    ))
                })
                .await;
            let _ = this.update(cx, |app, cx| {
                if !app.session_refresh.accepts(generation) {
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
                        app.session_history = rows;
                        app.session_scan_error = None;
                        if let Ok(aliases) = aliases {
                            app.session_user_state = aliases;
                            app.session_user_state_path = Some(path);
                            app.session_store_roots = Some(store_roots);
                            app.session_alias_environment = Some(environment);
                        }
                    }
                    Ok((Ok(other), _, _, _, _)) => {
                        app.session_scan_error = Some(discovery_message(&other, cx));
                    }
                    Ok((Err(error), _, _, _, _)) | Err(error) => {
                        app.session_scan_error = Some(scan_error_message(&error, cx));
                    }
                }
                app.rebuild_session_navigator(cx);
                if let Some(next) = app.session_refresh.finish(generation) {
                    app.start_session_navigator_refresh(next, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn rebuild_session_navigator(&mut self, cx: &mut gpui::App) {
        let live = self.live_session_rows(cx);
        self.session_navigator.refresh(&self.session_history, &live);
        let environment = crate::core::session::WorkspaceStore::environment_id(cx, self.workspace);
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
        self.session_navigator.project_aliases(&aliases);
        self.session_navigator.project_pins(&pins);
        self.session_navigator
            .project_display_order(&display_orders);

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
                let (binding, session, observed_agent, cwd, focused, unread, pane_id, live_title) =
                    match &slot {
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
                                view.live_first_user_title().map(str::to_owned),
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
                                None,
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
                rows.push(LiveSession {
                    identity,
                    agent,
                    session_id,
                    // Only the once-stamped first AgentPrompt excerpt may publish
                    // here. Tab labels and terminal OSC chrome must never appear.
                    title: live_title,
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
        let binding =
            live_binding_for_resume(row.agent, row.session_id.clone(), row.launch_argv.clone());
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
        let mut candidate_store = self.session_user_state.clone();
        candidate_store.replace_display_order(environment, display_orders);
        crate::ui::host_ops::HostOps::run_in(
            host,
            window,
            cx,
            move |host| candidate_store.save(host, &path).map(|_| candidate_store),
            move |this, result, window, cx| {
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
        window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
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
        let mut candidate = self.session_user_state.clone();
        candidate.set_pin(environment, row.identity, !row.pinned);
        crate::ui::host_ops::HostOps::run_in(
            host,
            window,
            cx,
            move |host| candidate.save(host, &path).map(|_| candidate),
            move |this, result, window, cx| {
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

    pub(crate) fn close_live_session_row(
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
        if row.lifecycle != agentty_core::agent_runtime::RowLifecycle::Live
            && row.lifecycle != agentty_core::agent_runtime::RowLifecycle::Restoring
        {
            return;
        }
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
        let tab_index = row.carrier.as_ref().and_then(|carrier| {
            self.tabs
                .iter()
                .position(|tab| Some(tab.tree_id.get().to_string()) == carrier.tab_id)
        });
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = answer.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if confirmed != Ok(1) {
                    return;
                }
                this.rebuild_session_navigator(cx);
                let Some(index) =
                    tab_index
                        .filter(|&index| index < this.tabs.len())
                        .or_else(|| {
                            this.session_navigator
                                .rows()
                                .iter()
                                .find(|row| row.row_id == row_id)
                                .and_then(|row| row.carrier.as_ref())
                                .and_then(|carrier| {
                                    this.tabs.iter().position(|tab| {
                                        Some(tab.tree_id.get().to_string()) == carrier.tab_id
                                    })
                                })
                        })
                else {
                    window.push_notification(
                        crate::core::i18n::current(cx, "session.live_carrier_missing"),
                        cx,
                    );
                    return;
                };
                this.close_tab(index, window, cx);
                this.refresh_session_navigator_for(SessionRefreshIntent::AgentCarrierClosed, cx);
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
        let row = self
            .session_navigator
            .rows()
            .iter()
            .find(|row| row.row_id == row_id)
            .cloned();
        let Some(row) = row else {
            return;
        };
        let Some(plan) = self.session_navigator.plan_close_and_delete(&row_id) else {
            return;
        };
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
        let tab_index = row.carrier.as_ref().and_then(|carrier| {
            self.tabs
                .iter()
                .position(|tab| Some(tab.tree_id.get().to_string()) == carrier.tab_id)
        });
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = answer.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if confirmed != Ok(1) {
                    return;
                }
                this.rebuild_session_navigator(cx);
                // Carrier may already be gone; still proceed to provider delete.
                if let Some(index) =
                    tab_index
                        .filter(|&index| index < this.tabs.len())
                        .or_else(|| {
                            this.session_navigator
                                .rows()
                                .iter()
                                .find(|row| row.row_id == row_id)
                                .and_then(|row| row.carrier.as_ref())
                                .and_then(|carrier| {
                                    this.tabs.iter().position(|tab| {
                                        Some(tab.tree_id.get().to_string()) == carrier.tab_id
                                    })
                                })
                        })
                {
                    this.close_tab(index, window, cx);
                    this.refresh_session_navigator_for(
                        SessionRefreshIntent::AgentCarrierClosed,
                        cx,
                    );
                }
                let delete_identity = plan.identity.clone();
                let planned_source = match this.session_store_roots.as_ref() {
                    Some(roots) => {
                        match agentty_core::agent_runtime::plan_close_and_delete_source(
                            &plan, roots,
                        ) {
                            Ok(source) => source,
                            Err(error) => {
                                let context =
                                    crate::core::i18n::current(cx, "notify.delete_failed");
                                crate::ui::host_ops::HostOps::notify_err(
                                    window, cx, context, &error,
                                );
                                return;
                            }
                        }
                    }
                    None if plan.source_path.is_some() => {
                        let context = crate::core::i18n::current(cx, "notify.delete_failed");
                        crate::ui::host_ops::HostOps::notify_err(
                            window,
                            cx,
                            context,
                            &std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                "session store roots unavailable",
                            ),
                        );
                        return;
                    }
                    None => None,
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
                match planned_source {
                    Some(source) => {
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
                    }
                    None => {
                        crate::ui::host_ops::HostOps::run_in(
                            host,
                            window,
                            cx,
                            move |host| {
                                agentty_core::agent_runtime::apply_session_user_state_delete(
                                    host,
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
}

#[cfg(test)]
mod session_keyboard_cursor_tests {
    use super::SessionKeyboardCursor;
    use agentty_core::agent_runtime::NavigatorRowId;

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
        );
        assert!(binding.agent.is_some());
        assert_eq!(binding.session_id.as_deref(), Some("session-1"));
        assert!(!binding.container_id.is_empty());
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
            block.contains("live_first_user_title") && block.contains("title: live_title"),
            "live projection may publish only the once-stamped first-user title"
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
            .split("fn close_and_delete_live_session_row")
            .nth(1)
            .expect("close_and_delete_live_session_row present");
        assert!(
            block.contains("plan_close_and_delete_source")
                && block.contains("apply_session_user_state_delete"),
            "Close and Delete must tombstone without a false failure when no provider source exists"
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
