use std::path::PathBuf;

use agentty_core::agent_runtime::{
    AgentRuntimeAdapter, AgentStoreRoots, DiscoveryOutcome, DiscoveryRequest, LiveCarrier,
    LiveSession, LocalAgentRuntime, NavigatorRowId, OperationId, RestoreOutcome, ScanGeneration,
    SessionIdentity,
};
use gpui::{Context, Window};

use crate::ui::app::{AgenttyApp, Tab, new_terminal};
use crate::ui::pane::{Pane, PaneSlot};

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

impl AgenttyApp {
    pub(crate) fn refresh_session_navigator(&mut self, cx: &mut Context<Self>) {
        self.request_session_navigator_refresh(true, cx);
    }

    pub(crate) fn refresh_session_navigator_passive(&mut self, cx: &mut Context<Self>) {
        self.request_session_navigator_refresh(false, cx);
    }

    fn request_session_navigator_refresh(&mut self, explicit: bool, cx: &mut Context<Self>) {
        let SessionRefreshRequest::Start(generation) = self.session_refresh.request(explicit)
        else {
            return;
        };
        self.start_session_navigator_refresh(generation, cx);
    }

    fn start_session_navigator_refresh(&mut self, generation: u64, cx: &mut Context<Self>) {
        let host_id = self.spawn_host(cx);
        let Some(host) = self.active_host(cx) else {
            return;
        };
        let home = if host_id.is_local() {
            std::env::var_os("HOME").map(PathBuf::from)
        } else {
            crate::ui::remote_connect::HostLinks::home(cx, host_id)
        };
        let Some(home) = home else {
            return;
        };
        let remote = (!host_id.is_local())
            .then(|| crate::ui::remote_connect::HostLinks::get(cx, host_id))
            .flatten();
        self.session_scan_started = true;
        self.session_scan_error = None;
        let request = DiscoveryRequest::codex_and_claude(AgentStoreRoots::for_home(home));
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    if host_id.is_local() {
                        LocalAgentRuntime::new(&*host).discover_sessions(
                            OperationId(generation),
                            ScanGeneration(generation),
                            request,
                        )
                    } else {
                        let remote = remote.ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::NotConnected,
                                "remote Environment has no connected Agent helper",
                            )
                        })?;
                        agentty_core::agent_runtime::RemoteAgentRuntime::new(&*remote)
                            .discover_sessions(
                                OperationId(generation),
                                ScanGeneration(generation),
                                request,
                            )
                    }
                })
                .await;
            let _ = this.update(cx, |app, cx| {
                if !app.session_refresh.accepts(generation) {
                    return;
                }
                match outcome {
                    Ok(DiscoveryOutcome::Complete(rows)) => {
                        app.session_history = rows;
                        app.session_scan_error = None;
                    }
                    Ok(other) => {
                        app.session_scan_error = Some(discovery_message(&other, cx));
                    }
                    Err(error) => {
                        app.session_scan_error = Some(error.to_string());
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

    pub(crate) fn rebuild_session_navigator(&mut self, cx: &gpui::App) {
        let live = self.live_session_rows(cx);
        self.session_navigator.refresh(&self.session_history, &live);
    }

    pub(crate) fn live_session_rows(&self, cx: &gpui::App) -> Vec<LiveSession> {
        let mut rows = Vec::new();
        for tab in &self.tabs {
            let tab_id = tab.tree_id.get().to_string();
            for terminal in tab.pane.terminals() {
                let view = terminal.read(cx);
                let Some(agent) = view.agent() else {
                    continue;
                };
                let session = view.agent_session();
                let session_id = session.as_ref().and_then(|s| s.session_id.clone());
                let identity = match session_id.as_deref() {
                    Some(id) => {
                        SessionIdentity::Provider(agentty_core::agent_runtime::AgentSessionKey {
                            provider: agent.slug().into(),
                            session_id: id.into(),
                        })
                    }
                    None => SessionIdentity::Durable(format!("pane:{}", view.pane_id())),
                };
                rows.push(LiveSession {
                    identity,
                    agent,
                    session_id,
                    title: Some(tab.name.clone().unwrap_or_else(|| view.title.clone())),
                    cwd: view.cwd().map(|cwd| cwd.to_string_lossy().into_owned()),
                    launch_argv: session.and_then(|s| s.launch_argv).unwrap_or_default(),
                    carrier: LiveCarrier {
                        tab_id: tab_id.clone(),
                        pane_id: view.pane_id(),
                    },
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
        if let Some(carrier) = row.carrier {
            if let Some(index) = self
                .tabs
                .iter()
                .position(|tab| tab.tree_id.get().to_string() == carrier.tab_id)
            {
                self.activate(index, window, cx);
            }
            return;
        }
        let Some(invocation) = self.session_navigator.begin_restore(&row_id) else {
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
        match &pane {
            PaneSlot::Ready(terminal) => {
                terminal.read(cx).run_invocation(&invocation, cx);
            }
            PaneSlot::Connecting(pending) => {
                pending.update(cx, |pending, _| {
                    pending.spawn.resume_invocation = Some(invocation.clone());
                    pending.spawn.navigator_row_id = Some(row_id.clone());
                    pending.spawn.agent = Some(row.agent);
                    pending.spawn.agent_session_id = row.session_id.clone();
                    pending.spawn.agent_launch_argv = Some(row.launch_argv.clone());
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
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

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
}
