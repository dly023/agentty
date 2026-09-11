//! Machine-scoped history management; reserved future actions have no handlers.
mod staging;
mod title_updates;

use super::app::Tty7App;
use super::host_ops::{HostId, HostOps};
use super::host_registry::HostRegistry;
use super::i18n::{L10nKey, t};
use super::manager_shell;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, MouseButton, ScrollHandle, Subscription, Window, div,
    prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use std::sync::Arc;
use tty7_core::agent_sessions::HistorySnapshot;
use uuid::Uuid;

#[derive(PartialEq)]
enum OriginPane {
    Empty,
    View(gpui::EntityId),
    Split(gpui::Axis, u32, Box<Self>, Box<Self>),
}

impl OriginPane {
    fn capture(pane: &super::pane::Pane) -> Self {
        use super::pane::Pane;
        match pane {
            Pane::Empty => Self::Empty,
            Pane::Leaf(slot) => Self::View(slot.entity_id()),
            Pane::Split {
                axis, ratio, a, b, ..
            } => Self::Split(
                *axis,
                ratio.get().to_bits(),
                Box::new(Self::capture(a)),
                Box::new(Self::capture(b)),
            ),
        }
    }
}

#[derive(PartialEq)]
struct RejoinOrigin {
    workspace: crate::core::session::WorkspaceId,
    active: usize,
    tabs: Vec<(tty7_core::core::machine::TabId, Option<String>, OriginPane)>,
}

impl RejoinOrigin {
    fn capture(app: &Tty7App) -> Self {
        Self {
            workspace: app.workspace,
            active: app.active,
            tabs: app
                .tabs
                .iter()
                .map(|tab| {
                    (
                        tab.tree_id.get(),
                        tab.name.clone(),
                        OriginPane::capture(&tab.pane),
                    )
                })
                .collect(),
        }
    }

    fn matches(&self, app: &Tty7App) -> bool {
        *self == Self::capture(app)
    }
}

fn rejoin_route_spec(
    target: &crate::core::session::RemoteTarget,
    cx: &gpui::App,
) -> Result<Option<Box<crate::daemon::protocol::NativeSshSpec>>, String> {
    use crate::core::session::RemoteTarget;
    match target {
        RemoteTarget::Wsl { .. } | RemoteTarget::LocalStdio { .. } => Ok(None),
        RemoteTarget::Profile { .. } | RemoteTarget::Alias { .. } | RemoteTarget::Direct { .. } => {
            super::remote_connect::spec_for(target, cx)
                .map(|spec| Some(Box::new(spec.without_secrets())))
        }
    }
}

pub(crate) struct HistoryManager {
    host: HostId,
    label: String,
    state: ScanState,
    selected: Option<(tty7_core::agent_sessions::Provider, String)>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    resuming: Option<Uuid>,
    action_alive: Arc<std::sync::atomic::AtomicBool>,
    query: Entity<InputState>,
    filter: String,
    _query_subscription: Subscription,
    titles: Option<title_updates::Interest>,
    title_unavailable: bool,
    dismissed: std::cell::Cell<bool>,
}

impl HistoryManager {
    pub(super) fn dismiss(&self) {
        self.dismissed.set(true);
        self.action_alive
            .store(false, std::sync::atomic::Ordering::Release);
        if let Some(interest) = &self.titles {
            interest.stop.close();
        }
    }

    fn cancel_titles(&mut self) {
        self.titles.take();
        self.title_unavailable = false;
    }

    fn cancel_action(&mut self) {
        self.action_alive
            .store(false, std::sync::atomic::Ordering::Release);
        self.resuming = None;
    }

    fn start_action(&mut self, token: Uuid) -> Arc<std::sync::atomic::AtomicBool> {
        self.cancel_action();
        self.action_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        self.resuming = Some(token);
        self.action_alive.clone()
    }
}

impl Drop for HistoryManager {
    fn drop(&mut self) {
        self.dismiss();
    }
}

/// A source-validated intent awaiting its target carrier's authoritative layout.
struct ResumeContinuation {
    token: Uuid,
    host: HostId,
    carrier: tty7_core::core::session::WorkspaceId,
    expected: super::host_ops::SharedHost,
    session: tty7_core::agent_sessions::HistorySession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RejoinTarget {
    workspace: tty7_core::core::session::WorkspaceId,
    tab: tty7_core::core::machine::TabId,
    pane: u64,
}

fn nonresident_rejoin_target(
    machine: &tty7_core::core::machine::Machine,
    session: &tty7_core::agent_sessions::HistorySession,
) -> Option<RejoinTarget> {
    let provider = match session.provider {
        tty7_core::agent_sessions::Provider::Codex => crate::core::cli_agent::CLIAgent::Codex,
        tty7_core::agent_sessions::Provider::Claude => crate::core::cli_agent::CLIAgent::Claude,
    };
    let mut panes = machine.panes.iter().filter(|pane| {
        pane.live
            && pane.agent.as_ref().is_some_and(|facts| {
                facts.agent == provider && facts.session_id.as_deref() == Some(session.id.as_str())
            })
    });
    let pane = panes.next()?;
    if panes.next().is_some() {
        return None;
    }
    let mut owners = machine.workspaces.iter().flat_map(|workspace| {
        workspace.tabs.iter().flat_map(move |tab| {
            tab.root
                .pane_ids()
                .into_iter()
                .filter(move |id| *id == pane.id)
                .map(move |_| (workspace, tab))
        })
    });
    let (workspace, tab) = owners.next()?;
    if owners.next().is_some() || workspace.attachment.is_some() {
        return None;
    }
    Some(RejoinTarget {
        workspace: workspace.id,
        tab: tab.id,
        pane: pane.id,
    })
}

fn future_actions() -> [PopupMenuItem; 3] {
    [
        PopupMenuItem::new(t(L10nKey::HistoryDeletePlanned)).disabled(true),
        PopupMenuItem::new(t(L10nKey::HistoryForkPlanned)).disabled(true),
        PopupMenuItem::new(t(L10nKey::HistoryConvertPlanned)).disabled(true),
    ]
}

fn history_matches(session: &tty7_core::agent_sessions::HistorySession, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || [
            session.title.as_str(),
            session.cwd.as_str(),
            session.id.as_str(),
            match session.provider {
                tty7_core::agent_sessions::Provider::Codex => "codex",
                tty7_core::agent_sessions::Provider::Claude => "claude",
            },
        ]
        .iter()
        .any(|field| field.to_lowercase().contains(&query))
}

impl HistoryManager {
    fn visible(&self) -> Vec<&tty7_core::agent_sessions::HistorySession> {
        self.state
            .snapshot
            .iter()
            .flat_map(|s| &s.sessions)
            .filter(|s| history_matches(s, &self.filter))
            .collect()
    }

    fn reconcile_selection(&mut self) {
        let rows = self.visible();
        if !rows
            .iter()
            .any(|s| self.selected.as_ref() == Some(&(s.provider, s.id.clone())))
        {
            self.selected = rows.first().map(|s| (s.provider, s.id.clone()));
        }
        self.scroll_to_selection();
    }

    fn scroll_to_selection(&self) {
        if let Some(index) = self
            .visible()
            .iter()
            .position(|s| self.selected.as_ref() == Some(&(s.provider, s.id.clone())))
        {
            // History rows are direct scroll children, unlike the switcher's
            // nested groups. Resolve the index from this visible identity now.
            self.scroll.scroll_to_item(index);
        }
    }
}

impl Tty7App {
    /// Resident fast path only: reports from an existing stream, never cached
    /// recovery metadata. Non-resident rejoin needs a guarded attach operation.
    fn resident_history_pane(
        &self,
        host: HostId,
        session: &tty7_core::agent_sessions::HistorySession,
        cx: &gpui::App,
    ) -> Option<(usize, Entity<crate::terminal::view::TerminalView>)> {
        if self.spawn_host(cx) != host
            || super::remote_workspace::workspace_is_preempted(cx, self.workspace)
            || !HostRegistry::lookup(cx, host).is_some_and(|h| h.is_connected())
        {
            return None;
        }
        let agent = match session.provider {
            tty7_core::agent_sessions::Provider::Codex => crate::core::cli_agent::CLIAgent::Codex,
            tty7_core::agent_sessions::Provider::Claude => crate::core::cli_agent::CLIAgent::Claude,
        };
        let mut found = None;
        for (index, tab) in self.tabs.iter().enumerate() {
            for terminal in tab.pane.terminals() {
                let view = terminal.read(cx);
                if view.host_id() == host
                    && !view.terminal.exited
                    && !view.terminal.child_exited()
                    && view.agent() == Some(agent)
                    && view
                        .agent_session()
                        .is_some_and(|s| s.session_id.as_deref() == Some(session.id.as_str()))
                {
                    if found.is_some() {
                        return None;
                    }
                    found = Some((index, terminal));
                }
            }
        }
        found
    }

    fn rejoin_selected_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(manager) = self.history_manager.as_ref() else {
            return;
        };
        if manager.resuming.is_some() || manager.state.generation.is_some() {
            return;
        }
        let Some(session) = manager
            .visible()
            .into_iter()
            .find(|s| manager.selected.as_ref() == Some(&(s.provider, s.id.clone())))
        else {
            return;
        };
        let Some((index, terminal)) = self.resident_history_pane(manager.host, session, cx) else {
            let (host, session) = (manager.host, session.clone());
            self.begin_nonresident_rejoin(host, session, window, cx);
            return;
        };
        self.close_history_manager(window, cx);
        self.activate(index, window, cx);
        let focus = terminal.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
    }

    fn begin_nonresident_rejoin(
        &mut self,
        id: HostId,
        session: tty7_core::agent_sessions::HistorySession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(expected) = HostRegistry::get(cx, id).filter(|host| host.is_connected()) else {
            self.finish_history_resume_error(t(L10nKey::HistoryOffline).into(), cx);
            return;
        };
        let pane_workspace = if id.is_local() {
            None
        } else {
            let Some(target) = self.machine_target(id, cx) else {
                self.finish_history_resume_error(t(L10nKey::HistoryOffline).into(), cx);
                return;
            };
            let Ok(spec) = rejoin_route_spec(&target, cx) else {
                self.finish_history_resume_error(t(L10nKey::HistoryRejoinUnavailable).into(), cx);
                return;
            };
            Some(crate::terminal::PaneWorkspace {
                workspace: self.workspace,
                target,
                spec,
                label: self.history_manager.as_ref().map(|m| m.label.clone()),
                resize_echo: super::remote_connect::HostLinks::peer_supports(
                    cx,
                    id,
                    crate::daemon::protocol::FEATURE_RESIZE_ECHO,
                ),
            })
        };
        let super::tree_sync::TreeLink::Ready(client) = super::tree_sync::tree_control_for(cx, id)
        else {
            self.finish_history_resume_error(t(L10nKey::HistoryRejoinUnavailable).into(), cx);
            return;
        };
        let expected_route = pane_workspace
            .as_ref()
            .map(|w| (w.target.clone(), w.spec.clone()));
        let token = Uuid::new_v4();
        let origin = RejoinOrigin::capture(self);
        let Some(manager) = self.history_manager.as_mut().filter(|m| m.host == id) else {
            return;
        };
        let alive = manager.start_action(token);
        manager.state.error = None;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(20))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .history_manager
                    .as_ref()
                    .is_some_and(|m| m.resuming == Some(token))
                {
                    this.finish_history_resume_error(t(L10nKey::HistoryResumePending).into(), cx);
                }
            });
        })
        .detach();
        let source = session.clone();
        let expected_client = client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let cleanup_client = client.clone();
            let mut result = cx
                .background_executor()
                .spawn(async move { staging::prepare(&client, pane_workspace, &source, &alive) })
                .await;
            let committed = this
                .update_in(cx, |this, window, cx| {
                    if !this
                        .history_manager
                        .as_ref()
                        .is_some_and(|m| m.host == id && m.resuming == Some(token))
                    {
                        return false;
                    }
                    let current_route = if id.is_local() {
                        None
                    } else {
                        this.machine_target(id, cx).map(|target| {
                            let spec = rejoin_route_spec(&target, cx).ok().flatten();
                            (target, spec)
                        })
                    };
                    let valid = origin.matches(this)
                        && current_route == expected_route
                        && HostRegistry::get(cx, id)
                            .is_some_and(|h| Arc::ptr_eq(&h, &expected) && h.is_connected())
                        && super::tree_sync::control_for(cx, id)
                            .is_some_and(|c| Arc::ptr_eq(&c, &expected_client) && c.is_connected());
                    if !valid {
                        this.finish_history_resume_error(t(L10nKey::HistoryOffline).into(), cx);
                        return false;
                    }
                    let ticket = match result.as_mut() {
                        Ok(ticket) => ticket,
                        Err(error) => {
                            this.finish_history_resume_error(error.clone(), cx);
                            return false;
                        }
                    };
                    let Some(carrier) =
                        this.resolve_rejoin_workspace(id, ticket.target.workspace, window, cx)
                    else {
                        this.finish_history_resume_error(
                            t(L10nKey::HistoryRejoinUnavailable).into(),
                            cx,
                        );
                        return false;
                    };
                    if let Err(prepared) = this.switch_workspace_prepared(
                        carrier,
                        ticket.prepared.take().expect("ticket commits once"),
                        window,
                        cx,
                    ) {
                        ticket.prepared = Some(prepared);
                        this.finish_history_resume_error(
                            t(L10nKey::HistoryRejoinUnavailable).into(),
                            cx,
                        );
                        return false;
                    }
                    this.retain_workspace_use(cleanup_client.clone(), ticket.usage.clone(), cx);
                    this.close_history_manager(window, cx);
                    if let Some((index, terminal)) = this.resident_history_pane(id, &session, cx) {
                        this.activate(index, window, cx);
                        let focus = terminal.read(cx).focus_handle.clone();
                        window.focus(&focus, cx);
                    }
                    true
                })
                .unwrap_or(false);
            if !committed {
                if let Ok(mut ticket) = result {
                    drop(ticket.prepared.take());
                    cx.background_executor()
                        .spawn(async move {
                            staging::release(&cleanup_client, ticket.usage);
                        })
                        .await;
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn open_history_manager(
        &mut self,
        host: HostId,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = cx.focus_handle();
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(t(L10nKey::HistorySearch)));
        query.update(cx, |input, cx| input.focus(window, cx));
        let subscription = cx.subscribe_in(
            &query,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let filter = input.read(cx).value().to_string();
                    if let Some(manager) = this.history_manager.as_mut() {
                        manager.filter = filter;
                        manager.cancel_action();
                        manager.reconcile_selection();
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.resume_selected_history(window, cx),
                _ => {}
            },
        );
        let scroll = ScrollHandle::new();
        self.history_manager = Some(HistoryManager {
            host,
            label,
            state: ScanState::default(),
            selected: None,
            scroll,
            focus,
            resuming: None,
            action_alive: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            query,
            filter: String::new(),
            _query_subscription: subscription,
            titles: None,
            title_unavailable: false,
            dismissed: std::cell::Cell::new(false),
        });
        self.refresh_history_manager(cx);
    }

    pub(crate) fn close_history_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mut manager) = self.history_manager.take() {
            manager.state.cancel();
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn refresh_history_manager(&mut self, cx: &mut Context<Self>) {
        let Some(manager) = self.history_manager.as_mut().filter(|m| !m.dismissed.get()) else {
            return;
        };
        let id = manager.host;
        manager.cancel_action();
        manager.cancel_titles();
        let token = manager.state.begin();
        let Some(host) = HostRegistry::get(cx, id).filter(|host| host.is_connected()) else {
            manager
                .state
                .finish(token, Err(t(L10nKey::HistoryOffline).into()));
            cx.notify();
            return;
        };
        let expected = host.clone();
        HostOps::run(
            host,
            cx,
            |h| h.agent_sessions().map_err(|e| e.to_string()),
            move |this, result, cx| {
                let current = HostRegistry::get(cx, id);
                let valid = current
                    .as_ref()
                    .is_some_and(|h| Arc::ptr_eq(h, &expected) && h.is_connected());
                if let Some(manager) = this
                    .history_manager
                    .as_mut()
                    .filter(|m| m.host == id && !m.dismissed.get())
                {
                    let result = if valid {
                        result
                    } else {
                        Err(t(L10nKey::HistoryOffline).into())
                    };
                    if manager.state.finish(token, result) {
                        manager.reconcile_selection();
                        if manager.state.error.is_none() {
                            this.start_history_titles(expected.clone(), cx);
                        }
                        cx.notify();
                    }
                }
            },
        );
        cx.notify();
    }

    fn start_history_titles(&mut self, host: super::host_ops::SharedHost, cx: &mut Context<Self>) {
        let Some(manager) = self
            .history_manager
            .as_mut()
            .filter(|m| m.host == host.id() && !m.dismissed.get())
        else {
            return;
        };
        let Some(snapshot) = manager.state.snapshot.clone() else {
            return;
        };
        manager.cancel_titles();
        let (interest, ready) =
            title_updates::start(host.clone(), snapshot, cx.background_executor());
        let token = interest.id;
        let stop = interest.stop.clone();
        manager.titles = Some(interest);
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(events)) = ready.recv().await {
                loop {
                    let event = events.recv().await.ok();
                    let keep = this
                        .update(cx, |app, cx| {
                            app.deliver_history_titles(token, &host, event, cx)
                        })
                        .unwrap_or(false);
                    if !keep {
                        break;
                    }
                }
            } else {
                let _ = this.update(cx, |app, cx| {
                    app.deliver_history_titles(token, &host, None, cx)
                });
            }
            stop.close();
        })
        .detach();
    }

    fn deliver_history_titles(
        &mut self,
        token: Uuid,
        expected: &super::host_ops::SharedHost,
        event: Option<tty7_core::agent_sessions::live_titles::HistoryTitleEvent>,
        cx: &mut Context<Self>,
    ) -> bool {
        use tty7_core::agent_sessions::live_titles::HistoryTitleEvent;
        let Some(manager) = self.history_manager.as_mut().filter(|m| {
            m.host == expected.id()
                && !m.dismissed.get()
                && m.state.generation.is_none()
                && m.titles
                    .as_ref()
                    .is_some_and(|interest| interest.id == token)
        }) else {
            return false;
        };
        if !HostRegistry::lookup(cx, manager.host).is_some_and(|h| Arc::ptr_eq(&h, expected)) {
            return false;
        }
        if !expected.is_connected() || event.is_none() {
            manager.title_unavailable = true;
            cx.notify();
            return false;
        }
        match event.unwrap() {
            HistoryTitleEvent::Names(names) => {
                match manager
                    .state
                    .snapshot
                    .as_mut()
                    .ok_or(())
                    .and_then(|snapshot| title_updates::project(snapshot, &names))
                {
                    Ok(changed) => {
                        manager.title_unavailable = false;
                        if changed {
                            manager.reconcile_selection();
                        }
                    }
                    Err(()) => manager.title_unavailable = true,
                }
            }
            HistoryTitleEvent::Unavailable => manager.title_unavailable = true,
        }
        cx.notify();
        true
    }

    fn resume_selected_history(&mut self, window: &Window, cx: &mut Context<Self>) {
        let workspace = self.workspace;
        let Some(manager) = self.history_manager.as_mut() else {
            return;
        };
        if manager.resuming.is_some() || manager.state.generation.is_some() {
            return;
        }
        let Some(session) = manager
            .state
            .snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot.sessions.iter().find(|s| {
                    history_matches(s, &manager.filter)
                        && manager.selected.as_ref() == Some(&(s.provider, s.id.clone()))
                })
            })
            .cloned()
        else {
            return;
        };
        let id = manager.host;
        let Some(host) = HostRegistry::get(cx, id).filter(|h| h.is_connected()) else {
            manager.state.error = Some(t(L10nKey::HistoryOffline).into());
            cx.notify();
            return;
        };
        let token = Uuid::new_v4();
        manager.start_action(token);
        manager.state.error = None;
        let expected = host.clone();
        let source = session.clone();
        HostOps::run_in(
            host,
            window,
            cx,
            move |h| h.validate_agent_session(&source).map_err(|e| e.to_string()),
            move |this, result, window, cx| {
                let Some(manager) = this
                    .history_manager
                    .as_mut()
                    .filter(|m| m.host == id && m.resuming == Some(token))
                else {
                    return;
                };
                let connected = HostRegistry::get(cx, id)
                    .is_some_and(|h| Arc::ptr_eq(&h, &expected) && h.is_connected());
                let result = if connected && this.workspace == workspace {
                    result
                } else {
                    Err(t(L10nKey::HistoryOffline).into())
                };
                if let Err(error) = result {
                    this.finish_history_resume_error(error, cx);
                    return;
                }
                let Some(carrier) = this.activate_history_machine(id, window, cx) else {
                    this.finish_history_resume_error(t(L10nKey::HistoryResumePending).into(), cx);
                    return;
                };
                if let Some(manager) = &this.history_manager {
                    manager
                        .query
                        .update(cx, |input, cx| input.focus(window, cx));
                }
                let pending = ResumeContinuation {
                    token,
                    host: id,
                    carrier,
                    expected,
                    session,
                };
                this.wait_history_resume(pending, window, cx);
            },
        );
        cx.notify();
    }

    fn wait_history_resume(
        &mut self,
        pending: ResumeContinuation,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            for attempt in 0..=200 {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.poll_history_resume(&pending, attempt == 200, window, cx)
                    })
                    .unwrap_or(true)
                {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
            }
        })
        .detach();
    }

    fn poll_history_resume(
        &mut self,
        pending: &ResumeContinuation,
        timed_out: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self
            .history_manager
            .as_ref()
            .is_some_and(|m| m.host == pending.host && m.resuming == Some(pending.token))
        {
            return true;
        }
        let connected = HostRegistry::get(cx, pending.host)
            .is_some_and(|h| Arc::ptr_eq(&h, &pending.expected) && h.is_connected());
        if !connected
            || self.workspace != pending.carrier
            || self.spawn_host(cx) != pending.host
            || super::remote_workspace::workspace_is_preempted(cx, pending.carrier)
        {
            self.finish_history_resume_error(t(L10nKey::HistoryOffline).into(), cx);
            return true;
        }
        if timed_out {
            self.finish_history_resume_error(t(L10nKey::HistoryResumePending).into(), cx);
            return true;
        }
        if !super::tree_sync::workspace_is_ready(cx, pending.carrier) {
            return false;
        }
        if self.resume_history_session(pending.host, &pending.session, window, cx) {
            self.close_history_manager(window, cx);
        } else {
            self.finish_history_resume_error(t(L10nKey::HistoryResumePending).into(), cx);
        }
        true
    }

    fn finish_history_resume_error(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(manager) = &mut self.history_manager {
            manager.cancel_action();
            manager.state.error = Some(error);
        }
        cx.notify();
    }

    pub(crate) fn render_history_manager(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let manager = self.history_manager.as_ref()?;
        let shell = manager_shell::layout(window);
        let sessions = manager.visible();
        let selected = sessions
            .iter()
            .find(|s| manager.selected.as_ref() == Some(&(s.provider, s.id.clone())));
        let mut list = v_flex()
            .id("history-session-list")
            .debug_selector(|| "history-session-list".into())
            .w(px(shell.left_width))
            .h_full()
            .flex_shrink_0()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&manager.scroll)
            .gap_1();
        for (index, session) in sessions.iter().enumerate() {
            let key = (session.provider, session.id.clone());
            let active = manager.selected.as_ref() == Some(&key);
            list = list.child(
                v_flex()
                    .id(("history-session", index))
                    .flex_shrink_0()
                    .when(active, |row| {
                        row.debug_selector(|| "history-selected-row".into())
                    })
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(active, |d| d.bg(cx.theme().secondary))
                    .hover(|d| d.bg(cx.theme().secondary))
                    .child(div().truncate().child(session.title.clone()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(session.cwd.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(manager) = this.history_manager.as_mut() {
                            manager.cancel_action();
                            manager.selected = Some(key.clone());
                        }
                        cx.notify();
                    })),
            );
        }
        if sessions.is_empty()
            && manager.state.generation.is_none()
            && manager.state.error.is_none()
        {
            list = list.child(t(if manager.filter.trim().is_empty() {
                L10nKey::HistoryEmpty
            } else {
                L10nKey::SwitcherNoMatch
            }));
        }
        let mut detail = v_flex()
            .id("history-session-detail")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .gap_2()
            .px_3();
        if let Some(session) = selected {
            detail = detail
                .child(div().child(session.title.clone()))
                .child(format!("{:?}", session.provider))
                .child(div().text_sm().child(session.id.clone()))
                .child(div().text_sm().child(session.cwd.clone()))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(session.source_path.clone()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("history-resume")
                                .label(t(L10nKey::HistoryResume))
                                .primary()
                                .disabled(
                                    manager.resuming.is_some()
                                        || manager.state.generation.is_some(),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.resume_selected_history(window, cx)
                                })),
                        )
                        .child(
                            Button::new("history-rejoin")
                                .label(t(L10nKey::HistoryRejoin))
                                .disabled(
                                    manager.resuming.is_some()
                                        || manager.state.generation.is_some()
                                        || !HostRegistry::lookup(cx, manager.host)
                                            .is_some_and(|h| h.is_connected()),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.rejoin_selected_history(window, cx)
                                })),
                        )
                        .child(
                            Button::new("history-more")
                                .label(t(L10nKey::TabTooltipMore))
                                .ghost()
                                .dropdown_menu(|mut menu, _, _| {
                                    for action in future_actions() {
                                        menu = menu.item(action);
                                    }
                                    menu
                                }),
                        ),
                );
        } else {
            detail = detail.child(t(L10nKey::HistorySelect));
        }
        let card = manager_shell::card(shell.width, cx)
            .id("history-manager-card")
            .debug_selector(|| "history-manager-card".into())
            .h(px(shell.height))
            .p_2()
            .gap_3()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(manager_shell::search(&manager.query, cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(format!(
                        "{} · {}",
                        t(L10nKey::HistoryManagerTitle),
                        manager.label
                    )))
                    .child(
                        Button::new("history-refresh")
                            .label(t(L10nKey::HistoryRefresh))
                            .small()
                            .on_click(
                                cx.listener(|this, _, _, cx| this.refresh_history_manager(cx)),
                            ),
                    )
                    .child(
                        Button::new("history-close")
                            .label(t(L10nKey::HistoryClose))
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_history_manager(window, cx)
                            })),
                    ),
            )
            .when(manager.state.generation.is_some(), |d| {
                d.child(t(L10nKey::HistoryLoading))
            })
            .when_some(manager.state.error.clone(), |d, error| {
                d.child(div().text_sm().child(error))
            })
            .when(manager.title_unavailable, |d| {
                d.child(div().text_sm().child(t(L10nKey::HistoryTitleUnavailable)))
            })
            .when(
                manager.state.snapshot.as_ref().is_some_and(|s| s.limited),
                |d| d.child(t(L10nKey::HistoryLimited)),
            )
            .when(
                manager
                    .state
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| !s.missing.is_empty()),
                |d| d.child(t(L10nKey::HistoryMissing)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .items_stretch()
                    .child(list)
                    .child(detail),
            );
        Some(
            manager_shell::overlay(shell, cx)
                .id("history-manager-overlay")
                .track_focus(&manager.focus)
                // Input emits PressEnter and propagates its action. Consume
                // that action here so it cannot fall through as terminal text.
                .on_action(cx.listener(|_, _: &gpui_component::input::Enter, _, cx| {
                    cx.stop_propagation();
                }))
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    let key = event.keystroke.key.as_str();
                    if key == "escape" {
                        this.close_history_manager(window, cx);
                        cx.stop_propagation();
                    } else if matches!(key, "up" | "down") && !event.keystroke.modifiers.modified()
                    {
                        if let Some(manager) = this.history_manager.as_mut() {
                            let rows = manager.visible();
                            let n = rows.len();
                            if n > 0 {
                                let current = rows.iter().position(|s| {
                                    manager.selected.as_ref() == Some(&(s.provider, s.id.clone()))
                                });
                                let next = current
                                    .map(|i| manager_shell::step(i, n, key == "down"))
                                    .unwrap_or(0);
                                manager.selected =
                                    Some((rows[next].provider, rows[next].id.clone()));
                                manager.cancel_action();
                                manager.scroll_to_selection();
                            }
                        }
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_history_manager(window, cx)),
                )
                .child(card)
                .into_any_element(),
        )
    }
}

/// A complete snapshot is published only by its current explicit scan.
#[derive(Default)]
pub(super) struct ScanState {
    pub snapshot: Option<HistorySnapshot>,
    pub error: Option<String>,
    generation: Option<Uuid>,
}

impl ScanState {
    pub fn begin(&mut self) -> Uuid {
        let token = Uuid::new_v4();
        self.generation = Some(token);
        self.error = None;
        token
    }

    pub fn cancel(&mut self) {
        self.generation = None;
    }

    pub fn finish(&mut self, token: Uuid, result: Result<HistorySnapshot, String>) -> bool {
        if self.generation != Some(token) {
            return false;
        }
        self.generation = None;
        match result {
            Ok(snapshot)
                if self.snapshot.as_ref().is_some_and(|old| {
                    snapshot
                        .missing
                        .iter()
                        .any(|provider| !old.missing.contains(provider))
                }) =>
            {
                self.error = Some(t(L10nKey::HistoryMissing).into())
            }
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tty7_core::agent_sessions::{HistorySession, Provider};

    #[gpui::test]
    fn scan_failure_preserves_rows(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        app.update_in(&mut vcx, |app, window, cx| {
            let host = HostId::from_connection_key("offline-scan-test");
            app.open_history_manager(host, "Offline".into(), window, cx);
            let tabs = app.tabs.iter().map(|t| t.tree_id.get()).collect::<Vec<_>>();
            let workspace = app.workspace;
            let active = app.active;
            let manager = app.history_manager.as_mut().unwrap();
            manager.state.snapshot = Some(snapshot());
            manager.selected = Some((Provider::Codex, snapshot().sessions[0].id.clone()));
            app.refresh_history_manager(cx);
            let manager = app.history_manager.as_ref().unwrap();
            assert_eq!(manager.state.snapshot, Some(snapshot()));
            assert!(manager.state.error.is_some());
            assert!(manager.state.generation.is_none());
            assert_eq!(manager.visible().len(), 1);
            assert_eq!(
                manager.selected,
                Some((Provider::Codex, snapshot().sessions[0].id.clone()))
            );
            assert_eq!(
                app.tabs.iter().map(|t| t.tree_id.get()).collect::<Vec<_>>(),
                tabs
            );
            assert_eq!(app.workspace, workspace);
            assert_eq!(app.active, active);
        });
    }

    #[gpui::test]
    fn rejoin_requires_a_live_pane(cx: &mut gpui::TestAppContext) {
        use crate::core::cli_agent::{AgentSessionState, AgentStatus, CLIAgent};
        use crate::daemon::protocol::DaemonMsg;
        let (app, mut vcx, mut streams) = crate::ui::app::test_window::harness_with_tabs(cx, 1);
        let row = snapshot().sessions.remove(0);
        DaemonMsg::Agent(Some(CLIAgent::Codex))
            .encode(&mut streams[0])
            .unwrap();
        DaemonMsg::AgentStatus(Some(AgentSessionState {
            status: AgentStatus::Waiting,
            session_id: Some(row.id.clone()),
            message: None,
            launch_argv: None,
            rich: true,
            cwd: None,
            activity: 0,
        }))
        .encode(&mut streams[0])
        .unwrap();
        let mut ready = false;
        for _ in 0..200 {
            ready = app.update_in(&mut vcx, |app, _, cx| {
                app.resident_history_pane(HostId::LOCAL, &row, cx).is_some()
            });
            if ready {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(ready);
        DaemonMsg::Exited { code: Some(0) }
            .encode(&mut streams[0])
            .unwrap();
        let mut exited = false;
        for _ in 0..200 {
            exited = app.update_in(&mut vcx, |app, _, cx| {
                app.tabs[0].pane.terminals()[0]
                    .read(cx)
                    .terminal
                    .child_exited()
            });
            if exited {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(exited);
        app.update_in(&mut vcx, |app, window, cx| {
            assert!(app.resident_history_pane(HostId::LOCAL, &row, cx).is_none());
            app.open_history_manager(HostId::LOCAL, "Local".into(), window, cx);
            let manager = app.history_manager.as_mut().unwrap();
            manager.state.generation = None;
            manager.state.snapshot = Some(snapshot());
            manager.selected = Some((row.provider, row.id.clone()));
            app.rejoin_selected_history(window, cx);
            assert!(app.history_manager.as_ref().unwrap().state.error.is_some());
            assert_eq!(
                app.tabs.len(),
                1,
                "exit must not fall back to spawning a new pane"
            );
        });
    }
    #[gpui::test]
    fn resident_rejoin_focuses_existing_identity_without_new_tabs(cx: &mut gpui::TestAppContext) {
        use crate::core::cli_agent::{AgentSessionState, AgentStatus, CLIAgent};
        use crate::daemon::protocol::DaemonMsg;
        let (app, mut vcx, mut streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        let row = snapshot().sessions.remove(0);
        DaemonMsg::Agent(Some(CLIAgent::Codex))
            .encode(&mut streams[1])
            .unwrap();
        DaemonMsg::AgentStatus(Some(AgentSessionState {
            status: AgentStatus::Waiting,
            session_id: Some(row.id.clone()),
            message: None,
            launch_argv: None,
            rich: true,
            cwd: None,
            activity: 0,
        }))
        .encode(&mut streams[1])
        .unwrap();
        let mut ready = false;
        for _ in 0..200 {
            ready = app.update_in(&mut vcx, |app, _, cx| {
                app.resident_history_pane(HostId::LOCAL, &row, cx).is_some()
            });
            if ready {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(ready, "daemon identity must reach the existing terminal");
        app.update_in(&mut vcx, |app, window, cx| {
            let workspace = app.workspace;
            let ids: Vec<_> = app.tabs.iter().map(|t| t.tree_id.get()).collect();
            app.open_history_manager(HostId::LOCAL, "Local".into(), window, cx);
            let manager = app.history_manager.as_mut().unwrap();
            manager.state.generation = None;
            manager.state.snapshot = Some(snapshot());
            manager.selected = Some((row.provider, row.id.clone()));
            app.rejoin_selected_history(window, cx);
            assert!(app.history_manager.is_none());
            assert_eq!(app.active, 1);
            assert_eq!(app.workspace, workspace);
            assert_eq!(
                app.tabs.iter().map(|t| t.tree_id.get()).collect::<Vec<_>>(),
                ids
            );
            let terminal = app.tabs[1].pane.terminals().remove(0);
            assert!(terminal.read(cx).focus_handle.is_focused(window));
            assert_eq!(terminal.read(cx).pane_id, 2);
            let foreign = HostId::from_connection_key("not-local");
            assert!(app.resident_history_pane(foreign, &row, cx).is_none());
            let mut wrong = row.clone();
            wrong.provider = Provider::Claude;
            assert!(
                app.resident_history_pane(HostId::LOCAL, &wrong, cx)
                    .is_none()
            );
            wrong = row.clone();
            wrong.id = "replaced".into();
            assert!(
                app.resident_history_pane(HostId::LOCAL, &wrong, cx)
                    .is_none()
            );
            terminal.update(cx, |view, _| view.terminal.exited = true);
            assert!(app.resident_history_pane(HostId::LOCAL, &row, cx).is_none());
        });
    }
    #[gpui::test]
    fn resident_rejoin_rejects_foreign_missing_and_ambiguous_identity(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::core::cli_agent::{AgentSessionState, AgentStatus, CLIAgent};
        use crate::daemon::protocol::DaemonMsg;
        let (app, mut vcx, mut streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        let row = snapshot().sessions.remove(0);
        app.update_in(&mut vcx, |app, _, cx| {
            assert!(app.resident_history_pane(HostId::LOCAL, &row, cx).is_none());
        });
        for stream in &mut streams {
            DaemonMsg::Agent(Some(CLIAgent::Codex))
                .encode(stream)
                .unwrap();
            DaemonMsg::AgentStatus(Some(AgentSessionState {
                status: AgentStatus::Waiting,
                session_id: Some(row.id.clone()),
                message: None,
                launch_argv: None,
                rich: true,
                cwd: None,
                activity: 0,
            }))
            .encode(stream)
            .unwrap();
        }
        let mut ready = false;
        for _ in 0..200 {
            ready = app.update_in(&mut vcx, |app, _, cx| {
                app.tabs
                    .iter()
                    .all(|t| t.pane.terminals()[0].read(cx).agent_session().is_some())
            });
            if ready {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(ready);
        app.update_in(&mut vcx, |app, window, cx| {
            let active = app.active;
            let workspace = app.workspace;
            assert!(app.resident_history_pane(HostId::LOCAL, &row, cx).is_none());
            app.open_history_manager(HostId::LOCAL, "Local".into(), window, cx);
            let manager = app.history_manager.as_mut().unwrap();
            manager.state.generation = None;
            manager.state.snapshot = Some(snapshot());
            manager.selected = Some((row.provider, row.id.clone()));
            app.rejoin_selected_history(window, cx);
            assert!(app.history_manager.as_ref().unwrap().state.error.is_some());
            assert_eq!(app.active, active);
            assert_eq!(app.workspace, workspace);
            assert_eq!(app.tabs.len(), 2);
        });
    }

    #[test]
    fn history_search_matches_metadata_without_changing_snapshot() {
        let mut snapshot = snapshot();
        snapshot.sessions[0].title = "官方命名 Review".into();
        let row = &snapshot.sessions[0];
        for query in ["", "   ", "REVIEW", "官方", "/PROJECT", "codex", &row.id] {
            assert!(history_matches(row, query), "query {query:?}");
        }
        assert!(!history_matches(row, "not-present"));
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].title, "官方命名 Review");
    }
    #[test]
    fn history_manager_future_actions_have_no_handlers() {
        for item in future_actions() {
            match item {
                PopupMenuItem::Item {
                    disabled,
                    handler,
                    action,
                    label,
                    ..
                } => {
                    assert!(disabled);
                    assert!(handler.is_none());
                    assert!(action.is_none());
                    assert!(!label.is_empty());
                }
                _ => panic!("future actions must be disabled plain menu items"),
            }
        }
    }
    #[gpui::test]
    fn history_resume_wait_cancellation_timeout_and_host_replacement_have_no_pane_effects(
        cx: &mut gpui::TestAppContext,
    ) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        app.update_in(&mut vcx, |app, window, cx| {
            app.open_history_manager(
                HostId::from_connection_key("history-wait-fixture"),
                "offline".into(),
                window,
                cx,
            );
            super::super::tree_sync::forget(cx, app.workspace);
            let mut pending = ResumeContinuation {
                token: Uuid::new_v4(),
                host: HostId::LOCAL,
                carrier: app.workspace,
                expected: HostRegistry::local(cx),
                session: snapshot().sessions.remove(0),
            };
            let arm = |app: &mut Tty7App, token| {
                let manager = app.history_manager.as_mut().unwrap();
                manager.host = HostId::LOCAL;
                manager.resuming = Some(token);
                manager.state.error = None;
            };
            arm(app, pending.token);
            assert!(
                !app.poll_history_resume(&pending, false, window, cx),
                "uninformed carrier waits"
            );
            assert_eq!(app.tabs.len(), 2);
            let newer = Uuid::new_v4();
            arm(app, newer);
            assert!(app.poll_history_resume(&pending, false, window, cx));
            assert_eq!(
                app.history_manager.as_ref().unwrap().resuming,
                Some(newer),
                "stale wait cannot cancel newer intent"
            );
            arm(app, pending.token);
            assert!(app.poll_history_resume(&pending, true, window, cx));
            assert!(app.history_manager.as_ref().unwrap().state.error.is_some());
            assert!(app.history_manager.as_ref().unwrap().resuming.is_none());
            arm(app, pending.token);
            pending.carrier = tty7_core::core::session::WorkspaceId::new();
            assert!(app.poll_history_resume(&pending, false, window, cx));
            assert!(app.history_manager.as_ref().unwrap().state.error.is_some());
            pending.carrier = app.workspace;
            arm(app, pending.token);
            pending.expected = tty7_core::host::local::LocalHost::new();
            assert!(app.poll_history_resume(&pending, false, window, cx));
            assert!(app.history_manager.as_ref().unwrap().state.error.is_some());
            app.close_history_manager(window, cx);
            assert!(app.poll_history_resume(&pending, false, window, cx));
            assert_eq!(app.tabs.len(), 2);
        });
    }

    #[gpui::test]
    fn history_carrier_unknown_machine_has_no_effects(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        app.update_in(&mut vcx, |app, window, cx| {
            let workspace = app.workspace;
            assert!(
                app.activate_history_machine(
                    HostId::from_connection_key("unknown-history-carrier"),
                    window,
                    cx
                )
                .is_none()
            );
            assert_eq!(app.workspace, workspace);
            assert_eq!(app.tabs.len(), 2);
        });
    }

    #[gpui::test]
    fn history_carrier_keeps_current_machine_in_this_window(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        app.update_in(&mut vcx, |app, window, cx| {
            let workspace = app.workspace;
            assert_eq!(
                app.activate_history_machine(HostId::LOCAL, window, cx),
                Some(workspace)
            );
            assert_eq!(app.tabs.len(), 2);
        });
    }

    #[gpui::test]
    fn history_resume_foreign_machine_cannot_spawn_locally(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        app.update_in(&mut vcx, |app, window, cx| {
            let original = app.workspace;
            let foreign = HostId::from_connection_key("history-foreign-machine");
            app.open_history_manager(foreign, "foreign".into(), window, cx);
            let snapshot = snapshot();
            let row = snapshot.sessions[0].clone();
            let manager = app.history_manager.as_mut().unwrap();
            manager.state.snapshot = Some(snapshot);
            manager.selected = Some((row.provider, row.id.clone()));
            app.resume_selected_history(window, cx);
            assert!(app.history_manager.as_ref().unwrap().state.error.is_some());
            assert!(app.history_manager.as_ref().unwrap().resuming.is_none());
            assert!(!app.resume_history_session(foreign, &row, window, cx));
            assert_eq!(app.workspace, original);
            assert_eq!(app.tabs.len(), 2);
        });
    }

    #[gpui::test]
    fn history_search_reconciles_visible_identity(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        app.update_in(&mut vcx, |app, window, cx| {
            app.open_history_manager(
                HostId::from_connection_key("history-search-offline"),
                "offline".into(),
                window,
                cx,
            );
            let manager = app.history_manager.as_mut().unwrap();
            let mut data = snapshot();
            let mut second = data.sessions[0].clone();
            second.id = "01900000-0000-7000-8000-000000000002".into();
            second.title = "second".into();
            let second_key = (second.provider, second.id.clone());
            data.sessions.push(second);
            manager.state.snapshot = Some(data);
            manager.selected = Some(second_key.clone());
            manager.reconcile_selection();
            assert_eq!(manager.selected, Some(second_key.clone()));
            manager.filter = "second".into();
            manager.reconcile_selection();
            assert_eq!(manager.selected, Some(second_key));
            manager.filter = "keep".into();
            manager.reconcile_selection();
            assert_eq!(
                manager.selected.as_ref().unwrap().1,
                snapshot().sessions[0].id
            );
            manager.filter = "no result".into();
            manager.reconcile_selection();
            assert!(manager.selected.is_none());
            assert_eq!(manager.state.snapshot.as_ref().unwrap().sessions.len(), 2);
        });
    }

    #[gpui::test]
    fn history_manager_search_and_keyboard_use_visible_rows(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        app.update_in(&mut vcx, |app, window, cx| {
            app.open_history_manager(
                HostId::from_connection_key("history-keyboard-offline"),
                "offline".into(),
                window,
                cx,
            );
            let manager = app.history_manager.as_mut().unwrap();
            let mut data = snapshot();
            let mut second = data.sessions[0].clone();
            second.id = "01900000-0000-7000-8000-000000000002".into();
            second.title = "second".into();
            data.sessions.push(second);
            manager.state.snapshot = Some(data);
            manager.reconcile_selection();
        });
        vcx.run_until_parked();
        vcx.simulate_keystrokes("down");
        app.update_in(&mut vcx, |app, _, _| {
            let manager = app.history_manager.as_ref().unwrap();
            assert!(manager.selected.as_ref().unwrap().1.ends_with('2'));
        });
        app.update_in(&mut vcx, |app, _, _| {
            app.history_manager.as_mut().unwrap().resuming = Some(Uuid::new_v4());
        });
        vcx.simulate_input("not-found");
        vcx.run_until_parked();
        app.update_in(&mut vcx, |app, _, _| {
            let manager = app.history_manager.as_ref().unwrap();
            assert!(manager.selected.is_none());
            assert!(manager.resuming.is_none());
            assert!(
                manager.state.generation.is_none(),
                "typing never initiates discovery"
            );
        });
        vcx.simulate_keystrokes("enter");
        app.update_in(&mut vcx, |app, _, _| assert_eq!(app.tabs.len(), 2));
        vcx.simulate_keystrokes("escape");
        app.update_in(&mut vcx, |app, _, _| assert!(app.history_manager.is_none()));
    }

    #[gpui::test]
    fn history_keyboard_scroll_keeps_selection_inside_card(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        app.update_in(&mut vcx, |app, window, cx| {
            app.open_history_manager(
                HostId::from_connection_key("history-scroll-offline"),
                "offline".into(),
                window,
                cx,
            );
            let manager = app.history_manager.as_mut().unwrap();
            let mut data = snapshot();
            data.sessions = (1..=30)
                .map(|i| {
                    let mut row = snapshot().sessions.remove(0);
                    row.id = format!("01900000-0000-7000-8000-{i:012}");
                    row.title = format!("Session {i}");
                    row
                })
                .collect();
            manager.state.snapshot = Some(data);
            manager.reconcile_selection();
        });
        vcx.simulate_resize(gpui::size(px(768.), px(560.)));
        vcx.run_until_parked();
        vcx.simulate_keystrokes("up");
        vcx.run_until_parked();
        let selected = vcx.debug_bounds("history-selected-row").unwrap();
        let card = vcx.debug_bounds("history-manager-card").unwrap();
        assert!(selected.origin.y >= card.origin.y);
        assert!(
            selected.origin.y + selected.size.height <= card.origin.y + card.size.height,
            "selected {selected:?}, card {card:?}"
        );
        app.update_in(&mut vcx, |app, _, _| {
            assert!(
                app.history_manager
                    .as_ref()
                    .unwrap()
                    .selected
                    .as_ref()
                    .unwrap()
                    .1
                    .ends_with("030")
            );
        });
    }

    #[gpui::test]
    fn history_manager_shell_bounds_follow_viewport(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        app.update_in(&mut vcx, |app, window, cx| {
            app.open_history_manager(
                HostId::from_connection_key("history-layout-offline"),
                "offline".into(),
                window,
                cx,
            );
        });
        for (width, height) in [(1440., 900.), (768., 560.), (375., 300.)] {
            vcx.simulate_resize(gpui::size(px(width), px(height)));
            vcx.run_until_parked();
            let bounds = vcx.debug_bounds("history-manager-card").unwrap();
            let layout = manager_shell::Layout::new(width, height);
            assert!((bounds.size.width.as_f32() - layout.width).abs() < 1.);
            assert!((bounds.origin.y.as_f32() - layout.top).abs() < 1.);
            assert!(bounds.origin.x >= px(0.));
            assert!(bounds.origin.x + bounds.size.width <= px(width));
            assert!(bounds.origin.y + bounds.size.height <= px(height));
        }
        app.update_in(&mut vcx, |app, _, _| assert_eq!(app.tabs.len(), 2));
    }

    #[gpui::test]
    fn rejoin_route_preserves_non_ssh_targets(cx: &mut gpui::TestAppContext) {
        use crate::core::session::RemoteTarget;
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 0);
        app.update_in(&mut vcx, |_, _, cx| {
            for target in [
                RemoteTarget::Wsl {
                    distro: "fixture".into(),
                },
                RemoteTarget::LocalStdio {
                    program: "fixture".into(),
                    args: vec![],
                },
            ] {
                assert!(matches!(rejoin_route_spec(&target, cx), Ok(None)));
            }
            assert!(rejoin_route_spec(&RemoteTarget::Profile { id: Uuid::new_v4() }, cx).is_err());
        });
    }

    #[gpui::test]
    fn rejoin_origin_detects_current_window_edits(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        app.update_in(&mut vcx, |app, _, _| {
            let origin = RejoinOrigin::capture(app);
            assert!(origin.matches(app));
            app.tabs.swap(0, 1);
            assert!(
                !origin.matches(app),
                "reordered tabs invalidate pending commit"
            );
            app.tabs.swap(0, 1);
            let name = app.tabs[0].name.clone();
            app.tabs[0].name = Some("edited while joining".into());
            assert!(!origin.matches(app), "renaming cannot be lost");
            app.tabs[0].name = name;
            app.active = 1 - app.active;
            assert!(
                !origin.matches(app),
                "explicit selection wins over late commit"
            );
            app.active = 1 - app.active;
            let pane = std::mem::replace(&mut app.tabs[0].pane, super::super::pane::Pane::Empty);
            assert!(!origin.matches(app), "removed pane cannot be overwritten");
            app.tabs[0].pane = pane;
            assert!(origin.matches(app));
            let left = std::mem::replace(&mut app.tabs[0].pane, super::super::pane::Pane::Empty);
            let right = std::mem::replace(&mut app.tabs[1].pane, super::super::pane::Pane::Empty);
            app.tabs[0].pane =
                super::super::pane::Pane::split_node(gpui::Axis::Horizontal, 0.5, left, right);
            assert!(!origin.matches(app), "split topology changed");
            let split_origin = RejoinOrigin::capture(app);
            if let super::super::pane::Pane::Split { ratio, .. } = &app.tabs[0].pane {
                ratio.set(0.6);
            }
            assert!(!split_origin.matches(app), "split resize changed");
        });
    }

    #[gpui::test]
    fn history_rejoin_cancellation_reaches_background_generation(cx: &mut gpui::TestAppContext) {
        use std::sync::atomic::Ordering;
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        app.update_in(&mut vcx, |app, window, cx| {
            let workspace = app.workspace;
            app.open_history_manager(
                HostId::from_connection_key("cancel-fixture"),
                "offline".into(),
                window,
                cx,
            );
            let manager = app.history_manager.as_mut().unwrap();
            let first = manager.start_action(Uuid::new_v4());
            assert!(first.load(Ordering::Acquire));
            let second = manager.start_action(Uuid::new_v4());
            assert!(!first.load(Ordering::Acquire));
            assert!(second.load(Ordering::Acquire));
            manager.cancel_action();
            assert!(!second.load(Ordering::Acquire));
            let third = manager.start_action(Uuid::new_v4());
            app.close_history_manager(window, cx);
            assert!(!third.load(Ordering::Acquire));
            assert_eq!(app.workspace, workspace);
            assert_eq!(app.tabs.len(), 2);
        });
    }

    #[gpui::test]
    fn history_manager_offline_open_has_no_pane_effects(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        app.update_in(&mut vcx, |app, window, cx| {
            let workspace = app.workspace;
            app.open_history_manager(
                HostId::from_connection_key("history-test-offline"),
                "offline".into(),
                window,
                cx,
            );
            let manager = app.history_manager.as_ref().unwrap();
            assert!(manager.state.error.is_some());
            assert!(manager.state.snapshot.is_none());
            assert_eq!(app.tabs.len(), 2);
            assert_eq!(app.workspace, workspace);
        });
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("history-manager-card").is_some(),
            "offline status is rendered, not silently dismissed"
        );
        app.update_in(&mut vcx, |app, window, cx| {
            app.close_history_manager(window, cx);
            assert!(app.history_manager.is_none());
        });
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("history-manager-card").is_none());
    }
    fn snapshot() -> HistorySnapshot {
        HistorySnapshot {
            sessions: vec![HistorySession {
                provider: Provider::Codex,
                id: "01900000-0000-7000-8000-000000000001".into(),
                title: "keep".into(),
                cwd: "/project".into(),
                source_path: "/provider/rollout.jsonl".into(),
                modified_ms: 1,
            }],
            missing: vec![],
            limited: false,
        }
    }
    #[test]
    fn nonresident_rejoin_rejects_duplicate_split_leaf() {
        use tty7_core::core::machine::{
            AgentFacts, Axis, Machine, PaneNode, PaneRecord, Tab, Workspace,
        };
        let session = snapshot().sessions.remove(0);
        let mut pane = PaneRecord::new(71);
        pane.live = true;
        pane.agent = Some(AgentFacts {
            agent: crate::core::cli_agent::CLIAgent::Codex,
            session_id: Some(session.id.clone()),
            launch_argv: None,
            status: None,
        });
        let mut tab = Tab::leaf(71);
        tab.root = PaneNode::Split {
            axis: Axis::Horizontal,
            ratio: 0.5,
            a: Box::new(PaneNode::Leaf { pane: 71 }),
            b: Box::new(PaneNode::Leaf { pane: 71 }),
        };
        let machine = Machine {
            workspaces: vec![Workspace {
                tabs: vec![tab],
                ..Workspace::default()
            }],
            panes: vec![pane],
        };
        assert!(nonresident_rejoin_target(&machine, &session).is_none());
    }

    #[test]
    fn nonresident_rejoin_requires_unique_live_unoccupied_owner() {
        use tty7_core::core::machine::{
            AgentFacts, Attachment, Machine, PaneRecord, Tab, Workspace,
        };
        let session = snapshot().sessions.remove(0);
        let mut pane = PaneRecord::new(71);
        pane.live = true;
        pane.agent = Some(AgentFacts {
            agent: crate::core::cli_agent::CLIAgent::Codex,
            session_id: Some(session.id.clone()),
            launch_argv: None,
            status: None,
        });
        let mut workspace = Workspace::default();
        workspace.tabs = vec![Tab::leaf(71), Tab::leaf(72), Tab::leaf(73)];
        let mut machine = Machine {
            workspaces: vec![workspace],
            panes: vec![pane, PaneRecord::new(72), PaneRecord::new(73)],
        };
        let target = nonresident_rejoin_target(&machine, &session).unwrap();
        assert_eq!(target.pane, 71);
        machine.panes[0].live = false;
        assert!(nonresident_rejoin_target(&machine, &session).is_none());
        machine.panes[0].live = true;
        machine.workspaces[0].attachment = Some(Attachment::new("other", "another client"));
        assert!(nonresident_rejoin_target(&machine, &session).is_none());
        machine.workspaces[0].attachment = None;
        let copy = machine.workspaces[0].tabs[0].clone();
        machine.workspaces[0].tabs.push(copy);
        assert!(nonresident_rejoin_target(&machine, &session).is_none());
        machine.workspaces[0].tabs.pop();
        let mut duplicate = machine.panes[0].clone();
        duplicate.id = 72;
        machine.panes[1] = duplicate;
        assert!(nonresident_rejoin_target(&machine, &session).is_none());
        machine.panes[1] = PaneRecord::new(72);
        machine.workspaces[0].tabs.remove(0);
        assert!(nonresident_rejoin_target(&machine, &session).is_none());
        assert_eq!(machine.workspaces[0].tabs.len(), 2);
    }
    #[test]
    fn history_manager_stale_cancelled_and_failed_results_preserve_rows() {
        let mut state = ScanState::default();
        let initial = state.begin();
        state.finish(initial, Ok(snapshot()));
        let stale = state.begin();
        let current = state.begin();
        assert!(!state.finish(
            stale,
            Ok(HistorySnapshot {
                sessions: vec![],
                missing: vec![],
                limited: false
            })
        ));
        assert!(state.finish(current, Err("offline".into())));
        assert_eq!(state.snapshot, Some(snapshot()));
        let cancelled = state.begin();
        state.cancel();
        assert!(!state.finish(
            cancelled,
            Ok(HistorySnapshot {
                sessions: vec![],
                missing: vec![],
                limited: false
            })
        ));
        assert_eq!(state.snapshot, Some(snapshot()));
    }
    #[test]
    fn history_manager_missing_provider_preserves_rows() {
        let mut state = ScanState::default();
        let token = state.begin();
        state.finish(token, Ok(snapshot()));
        let token = state.begin();
        let mut missing = snapshot();
        missing.missing.push(Provider::Codex);
        missing.sessions.clear();
        state.finish(token, Ok(missing));
        assert_eq!(state.snapshot, Some(snapshot()));
        assert!(state.error.is_some());
    }
}
