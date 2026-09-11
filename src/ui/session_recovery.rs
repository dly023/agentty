//! Target-owned recovery presentation and explicit original-slot coordination.
use super::app::Tty7App;
use super::i18n::{L10nKey, t};
use super::pane::PaneSlot;
use super::pending_pane::{PendingPane, PendingState};
use crate::core::session::WorkspaceStore;
use gpui::{App, div, prelude::*};
use gpui_component::{ActiveTheme as _, WindowExt as _};
use std::collections::HashMap;
use std::sync::Arc;
use tty7_core::agent_sessions::{Provider, SessionIdentity};
use tty7_core::core::machine::{Machine, PaneNode, PaneRecord, Tab};
use tty7_core::core::{machine::TabId, session::WorkspaceId, tab_view::PaneRecovery};
use tty7_core::daemon::control::{
    AttachmentProof, ControlClient, ControlRequest, ReplyOk, StoppedResumeRequest,
};
use tty7_core::host::HostId;

#[derive(Clone)]
pub(super) struct SleepUi {
    members: Option<TabSleepPlan>,
    tab_reply: Option<tty7_core::daemon::control::SleepTabReply>,
    target: ResumeTarget,
    connection: std::sync::Weak<ControlClient>,
    view: gpui::WeakEntity<crate::terminal::view::TerminalView>,
    epoch: uuid::Uuid,
    generation: uuid::Uuid,
    busy: bool,
    accepted: bool,
}

#[derive(Clone)]
struct TabSleepMember {
    target: ResumeTarget,
    entity: gpui::EntityId,
    live: bool,
}

#[derive(Clone)]
struct TabSleepPlan {
    members: Vec<TabSleepMember>,
}

impl TabSleepPlan {
    fn same_members(&self, other: &Self) -> bool {
        self.members.len() == other.members.len()
            && self
                .members
                .iter()
                .zip(&other.members)
                .all(|(a, b)| a.entity == b.entity && a.target.same_slot(&b.target))
    }

    fn wire_members(&self) -> Vec<tty7_core::daemon::control::SleepTabPane> {
        self.members
            .iter()
            .map(|m| tty7_core::daemon::control::SleepTabPane {
                pane: m.target.pane,
                identity: m.target.identity.clone(),
            })
            .collect()
    }
}

fn valid_tab_sleep_reply(panes: &[u64], reply: &tty7_core::daemon::control::SleepTabReply) -> bool {
    use std::collections::HashSet;
    let stopped: HashSet<_> = reply.already_stopped.iter().copied().collect();
    if stopped.len() != reply.already_stopped.len()
        || panes
            .iter()
            .copied()
            .filter(|p| stopped.contains(p))
            .collect::<Vec<_>>()
            != reply.already_stopped
    {
        return false;
    }
    let live: Vec<_> = panes
        .iter()
        .copied()
        .filter(|p| !stopped.contains(p))
        .collect();
    if !live.starts_with(&reply.accepted) {
        return false;
    }
    match &reply.failed {
        Some(failure) => live.get(reply.accepted.len()) == Some(&failure.pane),
        None => live == reply.accepted,
    }
}

impl Tty7App {
    // Observing keeps an existing operation scoped through exit/unknown runtime
    // transitions; new submissions use the stricter live-or-confirmed-stopped path.
    fn tab_sleep_plan(&self, tab_id: TabId, observing: bool, cx: &App) -> Option<TabSleepPlan> {
        let mut tabs = self.tabs.iter().filter(|t| t.tree_id.get() == tab_id);
        let tab = tabs.next()?;
        if tabs.next().is_some() {
            return None;
        }
        let owner = WorkspaceStore::all(cx).get(self.workspace)?;
        let host = owner.host_id();
        let workspace = owner.host.as_ref().map_or(self.workspace, |r| r.workspace);
        let machine = super::machine_mirror::MachineMirrors::machine(cx, host)?;
        let mut members = Vec::new();
        for slot in tab.pane.leaves() {
            let entity = slot.entity_id();
            if self
                .tabs
                .iter()
                .flat_map(|t| t.pane.leaves())
                .filter(|s| s.entity_id() == entity)
                .count()
                != 1
            {
                return None;
            }
            let pane = match &slot {
                PaneSlot::Ready(view) => {
                    let view = view.read(cx);
                    if view.owner_workspace() != Some(self.workspace)
                        || view.host_id() != host
                        || view.ssh_spec().is_some()
                    {
                        return None;
                    }
                    view.pane_id
                }
                PaneSlot::Connecting(pending) => {
                    let pending = pending.read(cx);
                    if pending.spawn.owner != Some(self.workspace)
                        || !matches!(pending.state, PendingState::Stopped)
                        || pending.resume.is_some()
                    {
                        return None;
                    }
                    pending.spawn.restore_pane?
                }
            };
            let facts = machine.panes.iter().find(|p| p.id == pane)?;
            if facts.ssh_spec.is_some() {
                return None;
            }
            let live =
                ResumeTarget::from_live_machine(self.workspace, host, workspace, pane, machine);
            let target = if observing {
                ResumeTarget::from_bound_machine(self.workspace, host, workspace, pane, machine)
            } else {
                live.clone().or_else(|| {
                    ResumeTarget::from_machine(self.workspace, host, workspace, pane, machine)
                })
            }?;
            if target.tab != tab_id {
                return None;
            }
            if !observing
                && live.is_some()
                && slot
                    .terminal()
                    .is_none_or(|v| v.read(cx).terminal.child_exited())
            {
                return None;
            }
            members.push(TabSleepMember {
                target,
                entity,
                live: live.is_some(),
            });
        }
        let first = members.first()?;
        if first.target.root.pane_ids() != members.iter().map(|m| m.target.pane).collect::<Vec<_>>()
        {
            return None;
        }
        Some(TabSleepPlan { members })
    }

    fn visible_session_view(
        &self,
        window: &gpui::Window,
        cx: &App,
    ) -> Option<gpui::Entity<crate::terminal::view::TerminalView>> {
        let tab = self.tabs.get(self.active)?;
        let view = self
            .maximized
            .clone()
            .or_else(|| tab.pane.focused_or_first(window, cx))?;
        tab.pane
            .leaves()
            .iter()
            .any(|s| s.entity_id() == view.entity_id())
            .then_some(view)
    }

    fn live_sleep_target(
        &self,
        view: &gpui::Entity<crate::terminal::view::TerminalView>,
        cx: &App,
    ) -> Option<ResumeTarget> {
        let terminal = view.read(cx);
        if terminal.owner_workspace() != Some(self.workspace)
            || terminal.terminal.child_exited()
            || terminal.ssh_spec().is_some()
        {
            return None;
        }
        let owner = WorkspaceStore::all(cx).get(self.workspace)?;
        let host = owner.host_id();
        if host != terminal.host_id() {
            return None;
        }
        let workspace = owner.host.as_ref().map_or(self.workspace, |r| r.workspace);
        let machine = super::machine_mirror::MachineMirrors::machine(cx, host)?;
        let target = ResumeTarget::from_live_machine(
            self.workspace,
            host,
            workspace,
            terminal.pane_id,
            machine,
        )?;
        let owners: Vec<_> = self
            .tabs
            .iter()
            .flat_map(|t| t.pane.leaves().into_iter().map(move |s| (t, s)))
            .filter(|(_, s)| s.entity_id() == view.entity_id())
            .collect();
        matches!(owners.as_slice(), [(tab, _)] if tab.tree_id.get() == target.tab).then_some(target)
    }

    fn sleep_scope_current(&self, request: &SleepUi, cx: &App) -> bool {
        self.workspace_view_epoch.get() == request.epoch
            && self
                .sleep_ui
                .as_ref()
                .is_some_and(|s| s.generation == request.generation)
            && match &request.members {
                Some(plan) => self
                    .tab_sleep_plan(request.target.tab, true, cx)
                    .is_some_and(|now| plan.same_members(&now)),
                None => request.view.upgrade().is_some_and(|view| {
                    self.live_sleep_target(&view, cx)
                        .is_some_and(|now| now.same_slot(&request.target))
                }),
            }
    }

    fn sleep_connection_current(request: &SleepUi, cx: &mut App) -> bool {
        request
            .connection
            .upgrade()
            .zip(super::tree_sync::control_for(cx, request.target.host))
            .is_some_and(|(expected, current)| Arc::ptr_eq(&expected, &current))
    }

    pub(super) fn render_session_sleep(
        &self,
        window: &gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let single = self.render_single_sleep(window, cx);
        let whole = self.render_tab_sleep(cx);
        if single.is_none() && whole.is_none() {
            return None;
        }
        Some(
            gpui_component::h_flex()
                .gap_2()
                .flex_wrap()
                .children(single)
                .children(whole)
                .into_any_element(),
        )
    }

    fn render_tab_sleep(&self, cx: &mut gpui::Context<Self>) -> Option<gpui::AnyElement> {
        use gpui_component::{
            Disableable as _, Sizable as _,
            button::{Button, ButtonVariants as _},
        };
        let tab = self.tabs.get(self.active)?;
        if tab.pane.leaves().len() < 2 {
            return None;
        }
        let tab_id = tab.tree_id.get();
        let plan = self
            .tab_sleep_plan(tab_id, false, cx)
            .filter(|p| p.members.iter().any(|m| m.live));
        let pending = self
            .sleep_ui
            .as_ref()
            .filter(|s| self.sleep_scope_current(s, cx) && Self::sleep_connection_current(s, cx));
        let own = pending.filter(|s| s.members.is_some() && s.target.tab == tab_id);
        let refresh = own.is_some();
        let disabled =
            pending.is_some_and(|s| s.busy || own.is_none()) || (!refresh && plan.is_none());
        let mut pill = super::notice::pill(cx.theme().border, cx).text_sm();
        if let Some(state) = own {
            if let Some(reply) = state
                .tab_reply
                .as_ref()
                .filter(|r| r.failed.is_some() && !state.busy)
            {
                pill = pill.child(super::i18n::t_fmt(
                    L10nKey::SessionSleepPartial,
                    &[
                        ("accepted", &reply.accepted.len().to_string()),
                        ("stopped", &reply.already_stopped.len().to_string()),
                    ],
                ));
            } else {
                pill = pill.child(t(if state.busy {
                    L10nKey::SessionSleepWorking
                } else if state.accepted {
                    L10nKey::SessionSleepAwaitingExit
                } else {
                    L10nKey::SessionSleepUnknown
                }));
            }
        }
        Some(
            pill.child(
                Button::new("current-tab-sleep")
                    .debug_selector(|| "current-tab-sleep".into())
                    .label(t(if refresh {
                        L10nKey::HistoryRefresh
                    } else {
                        L10nKey::SessionSleepTab
                    }))
                    .tooltip(t(L10nKey::SessionSleepTabRequirement))
                    .ghost()
                    .small()
                    .disabled(disabled)
                    .on_click(cx.listener(move |app, _, window, cx| {
                        if refresh {
                            app.refresh_sleep(window, cx);
                        } else if let Some(plan) = &plan {
                            app.sleep_entire_tab(plan, window, cx);
                        }
                    })),
            )
            .into_any_element(),
        )
    }

    fn render_single_sleep(
        &self,
        window: &gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> Option<gpui::AnyElement> {
        use gpui_component::{
            Disableable as _, Sizable as _,
            button::{Button, ButtonVariants as _},
        };
        let view = self.visible_session_view(window, cx)?;
        let target = self.live_sleep_target(&view, cx)?;
        let pending = self
            .sleep_ui
            .as_ref()
            .filter(|s| self.sleep_scope_current(s, cx) && Self::sleep_connection_current(s, cx));
        if pending.is_some_and(|s| s.members.is_some()) {
            return None;
        }
        let own = pending.filter(|s| s.target.same_slot(&target));
        let disabled = pending.is_some_and(|s| s.busy || !s.target.same_slot(&target));
        let refresh = own.is_some();
        let label = if refresh {
            L10nKey::HistoryRefresh
        } else {
            L10nKey::SessionSleepCurrent
        };
        let mut pill = super::notice::pill(cx.theme().border, cx).text_sm();
        if let Some(state) = own {
            pill = pill.child(t(if state.busy {
                L10nKey::SessionSleepWorking
            } else if state.accepted {
                L10nKey::SessionSleepAwaitingExit
            } else {
                L10nKey::SessionSleepUnknown
            }));
        }
        Some(
            pill.child(
                Button::new("current-session-sleep")
                    .debug_selector(|| "current-session-sleep".into())
                    .label(t(label))
                    .ghost()
                    .small()
                    .disabled(disabled)
                    .on_click(cx.listener(move |app, _, window, cx| {
                        if refresh {
                            app.refresh_sleep(window, cx);
                        } else {
                            app.sleep_current_session(view.clone(), &target, window, cx);
                        }
                    })),
            )
            .into_any_element(),
        )
    }

    fn sleep_current_session(
        &mut self,
        view: gpui::Entity<crate::terminal::view::TerminalView>,
        expected: &ResumeTarget,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.sleep_ui.as_ref().is_some_and(|s| {
            self.sleep_scope_current(s, cx) && Self::sleep_connection_current(s, cx)
        }) {
            return;
        }
        let Some(target) = self
            .live_sleep_target(&view, cx)
            .filter(|t| t.same_slot(expected))
        else {
            return;
        };
        let Some(client) = super::tree_sync::control_for(cx, target.host) else {
            window.push_notification(t(L10nKey::SessionSleepUnknown), cx);
            return;
        };
        let request = SleepUi {
            members: None,
            tab_reply: None,
            target,
            connection: Arc::downgrade(&client),
            view: view.downgrade(),
            epoch: self.workspace_view_epoch.get(),
            generation: uuid::Uuid::new_v4(),
            busy: true,
            accepted: false,
        };
        self.begin_sleep(request, client, window, cx);
    }

    fn sleep_entire_tab(
        &mut self,
        expected: &TabSleepPlan,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.sleep_ui.as_ref().is_some_and(|s| {
            self.sleep_scope_current(s, cx) && Self::sleep_connection_current(s, cx)
        }) {
            return;
        }
        let Some(first) = expected.members.first() else {
            return;
        };
        let Some(plan) = self
            .tab_sleep_plan(first.target.tab, false, cx)
            .filter(|p| p.same_members(expected))
        else {
            return;
        };
        let Some(anchor) = plan.members.iter().find(|m| m.live) else {
            return;
        };
        let Some(view) = self
            .tabs
            .iter()
            .flat_map(|t| t.pane.leaves())
            .find(|s| s.entity_id() == anchor.entity)
            .and_then(|s| s.terminal().cloned())
        else {
            return;
        };
        let Some(client) = super::tree_sync::control_for(cx, anchor.target.host) else {
            window.push_notification(t(L10nKey::SessionSleepUnknown), cx);
            return;
        };
        let request = SleepUi {
            target: anchor.target.clone(),
            view: view.downgrade(),
            members: Some(plan),
            tab_reply: None,
            connection: Arc::downgrade(&client),
            epoch: self.workspace_view_epoch.get(),
            generation: uuid::Uuid::new_v4(),
            busy: true,
            accepted: false,
        };
        self.begin_sleep(request, client, window, cx);
    }

    fn begin_sleep(
        &mut self,
        request: SleepUi,
        client: Arc<ControlClient>,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.sleep_ui = Some(request.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let worker = client.clone();
            let workspace = request.target.workspace;
            let acquired =
                super::host_ops::off_thread(move || worker.acquire_workspace_use(workspace).ok())
                    .await
                    .flatten();
            let orphan = acquired.clone();
            let orphan_client = client.clone();
            let landed = this.update_in(cx, |app, window, cx| {
                let current_link = super::tree_sync::control_for(cx, request.target.host);
                let scoped = app.sleep_scope_current(&request, cx)
                    && current_link
                        .as_ref()
                        .is_some_and(|now| Arc::ptr_eq(now, &client));
                let current = scoped
                    && request.members.as_ref().is_none_or(|plan| {
                        app.tab_sleep_plan(request.target.tab, false, cx)
                            .is_some_and(|now| plan.same_members(&now))
                    });
                let Some(usage) = acquired else {
                    if scoped {
                        app.finish_sleep_reply(&request, false, cx);
                    }
                    return;
                };
                if !current {
                    if scoped {
                        app.finish_sleep_reply(&request, false, cx);
                    }
                    release(client, usage, cx);
                    return;
                }
                let proof = usage.proof().clone();
                app.retain_workspace_use(client.clone(), usage, cx);
                app.submit_sleep(request, client, proof, window, cx);
            });
            if landed.is_err()
                && let Some(usage) = orphan
            {
                cx.background_executor()
                    .spawn(async move {
                        let _ = super::host_ops::off_thread(move || {
                            orphan_client.finish_workspace_use(usage)
                        })
                        .await;
                    })
                    .detach();
            }
        })
        .detach();
    }

    fn finish_sleep_reply(
        &mut self,
        request: &SleepUi,
        accepted: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.sleep_scope_current(request, cx)
            && let Some(state) = self.sleep_ui.as_mut()
        {
            state.busy = false;
            state.accepted = accepted;
            cx.notify();
        }
    }

    fn submit_sleep(
        &mut self,
        request: SleepUi,
        client: Arc<ControlClient>,
        proof: AttachmentProof,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let worker = client.clone();
            let target = request.target.clone();
            let members = request.members.clone();
            let reply = super::host_ops::off_thread(move || {
                let operation = if let Some(plan) = members {
                    ControlRequest::SleepTab {
                        request: tty7_core::daemon::control::SleepTabRequest {
                            proof,
                            tab: target.tab,
                            panes: plan.wire_members(),
                        },
                    }
                } else {
                    ControlRequest::SleepSession {
                        request: tty7_core::daemon::control::SleepSessionRequest {
                            proof,
                            tab: target.tab,
                            pane: target.pane,
                            identity: target.identity,
                        },
                    }
                };
                worker.call(operation).ok()
            })
            .await
            .flatten();
            let _ = this.update_in(cx, |app, _, cx| {
                if super::tree_sync::control_for(cx, request.target.host)
                    .is_some_and(|now| Arc::ptr_eq(&now, &client))
                    && app.sleep_scope_current(&request, cx)
                {
                    let (accepted, details) = match (&request.members, reply) {
                        (None, Some(ReplyOk::Unit)) => (true, None),
                        (Some(plan), Some(ReplyOk::TabSleep(reply)))
                            if valid_tab_sleep_reply(
                                &plan
                                    .members
                                    .iter()
                                    .map(|m| m.target.pane)
                                    .collect::<Vec<_>>(),
                                &reply,
                            ) =>
                        {
                            (reply.failed.is_none(), Some(reply))
                        }
                        _ => (false, None),
                    };
                    app.finish_sleep_reply(&request, accepted, cx);
                    if let Some(state) = app.sleep_ui.as_mut() {
                        state.tab_reply = details;
                    }
                }
            });
        })
        .detach();
    }

    fn refresh_sleep(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let Some(request) = self.sleep_ui.clone().filter(|s| {
            !s.busy && self.sleep_scope_current(s, cx) && Self::sleep_connection_current(s, cx)
        }) else {
            return;
        };
        let Some(client) = super::tree_sync::control_for(cx, request.target.host) else {
            return;
        };
        self.sleep_ui.as_mut().unwrap().busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let worker = client.clone();
            let snapshot = super::host_ops::off_thread(move || {
                match worker.call(ControlRequest::MachineGet) {
                    Ok(ReplyOk::MachineTree(machine)) => Some(machine),
                    _ => None,
                }
            })
            .await
            .flatten();
            let _ = this.update_in(cx, |app, _, cx| {
                if !app.sleep_scope_current(&request, cx)
                    || !super::tree_sync::control_for(cx, request.target.host)
                        .is_some_and(|now| Arc::ptr_eq(&now, &client))
                {
                    return;
                }
                let target = &request.target;
                let live = if let Some(plan) = &request.members {
                    snapshot.as_ref().is_some_and(|machine| {
                        plan.members.iter().all(|member| {
                            let t = &member.target;
                            ResumeTarget::from_live_machine(
                                t.client_workspace,
                                t.host,
                                t.workspace,
                                t.pane,
                                machine,
                            )
                            .or_else(|| {
                                ResumeTarget::from_machine(
                                    t.client_workspace,
                                    t.host,
                                    t.workspace,
                                    t.pane,
                                    machine,
                                )
                            })
                            .is_some_and(|now| now.same_slot(t))
                        })
                    })
                } else {
                    snapshot
                        .as_ref()
                        .and_then(|m| {
                            ResumeTarget::from_live_machine(
                                target.client_workspace,
                                target.host,
                                target.workspace,
                                target.pane,
                                m,
                            )
                        })
                        .is_some_and(|now| now.same_slot(target))
                };
                if live {
                    app.sleep_ui = None;
                    cx.notify();
                } else {
                    app.finish_sleep_reply(&request, false, cx);
                    // The shared target projection owns stopped facts. Never
                    // synthesize exit or replace output from an RPC acknowledgement.
                    super::machine_mirror::MachineMirrors::refresh(cx, target.host);
                }
            });
        })
        .detach();
    }

    fn retained_candidate(
        &self,
        view: &gpui::Entity<crate::terminal::view::TerminalView>,
        cx: &App,
    ) -> Option<ResumeTarget> {
        let terminal = view.read(cx);
        if terminal.owner_workspace() != Some(self.workspace)
            || !terminal.terminal.child_exited()
            || terminal.ssh_spec().is_some()
        {
            return None;
        }
        let target = candidate(cx, self.workspace, terminal.pane_id)?;
        if target.host != terminal.host_id() {
            return None;
        }
        let occurrences: Vec<_> = self
            .tabs
            .iter()
            .filter(|tab| {
                tab.pane
                    .leaves()
                    .iter()
                    .any(|slot| slot.entity_id() == view.entity_id())
            })
            .collect();
        if occurrences.len() != 1 || occurrences[0].tree_id.get() != target.tab {
            return None;
        }
        Some(target)
    }

    pub(super) fn render_retained_resume(
        &self,
        window: &gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> Option<gpui::AnyElement> {
        use gpui_component::{
            Sizable as _,
            button::{Button, ButtonVariants as _},
        };
        let view = self.visible_session_view(window, cx)?;
        let target = self.retained_candidate(&view, cx)?;
        Some(
            super::notice::pill(cx.theme().border, cx)
                .text_sm()
                .child(t(L10nKey::SessionTerminalStopped))
                .child(
                    Button::new("retained-pane-resume")
                        .debug_selector(|| "retained-pane-resume".into())
                        .label(t(L10nKey::HistoryResume))
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |app, _, window, cx| {
                            app.resume_retained(view.clone(), &target, window, cx)
                        })),
                )
                .into_any_element(),
        )
    }

    fn resume_retained(
        &mut self,
        view: gpui::Entity<crate::terminal::view::TerminalView>,
        expected: &ResumeTarget,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(target) = self
            .retained_candidate(&view, cx)
            .filter(|target| target.same_slot(expected))
        else {
            return;
        };
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.tree_id.get() == target.tab)
            .unwrap();
        if self.hold_resume_tab(target.tab, cx) {
            return;
        }
        let workspace = self.window_workspace(cx);
        let stopped = crate::core::session::SessionPane::Stopped {
            pane_id: target.pane,
            cwd: Some(target.identity.cwd.clone().into()),
            shell: None,
        };
        let Some(pane) = super::app::session_to_pane(
            workspace.as_ref(),
            self.workspace,
            &stopped,
            None,
            view.read(cx).font_size.as_f32(),
            crate::terminal::RestorePolicy::AttachOnly,
            window,
            cx,
        ) else {
            return;
        };
        let PaneSlot::Connecting(pending) = pane.leaves()[0].clone() else {
            return;
        };
        pending.update(cx, |pending, _| pending.retained_view = Some(view.clone()));
        if !self.tabs[index]
            .pane
            .replace_leaf(view.entity_id(), PaneSlot::Connecting(pending.clone()))
        {
            return;
        }
        pending.update(cx, |pending, cx| pending.begin_resume(target, false, cx));
        cx.notify();
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ResumeTarget {
    client_workspace: WorkspaceId,
    host: HostId,
    workspace: WorkspaceId,
    tab: TabId,
    pane: u64,
    identity: SessionIdentity,
    root: PaneNode,
}

fn identity(pane: &PaneRecord) -> Option<SessionIdentity> {
    let binding = pane.recovery_binding.as_ref()?;
    let provider = match binding.agent {
        crate::core::cli_agent::CLIAgent::Codex => Provider::Codex,
        crate::core::cli_agent::CLIAgent::Claude => Provider::Claude,
        _ => return None,
    };
    let mut identity = SessionIdentity {
        provider,
        id: binding.session_id.clone(),
        cwd: binding.cwd.clone(),
    };
    identity.id = identity.canonical_id().ok()?;
    Some(identity)
}

fn unique_tab(machine: &Machine, workspace: WorkspaceId, pane: u64) -> Option<&Tab> {
    let owners: Vec<_> = machine
        .workspaces
        .iter()
        .flat_map(|ws| {
            ws.tabs.iter().flat_map(move |tab| {
                tab.root
                    .pane_ids()
                    .into_iter()
                    .filter(move |id| *id == pane)
                    .map(move |_| (ws.id, tab))
            })
        })
        .collect();
    match owners.as_slice() {
        [(owner, tab)] if *owner == workspace => Some(*tab),
        _ => None,
    }
}

impl ResumeTarget {
    fn from_live_machine(
        client_workspace: WorkspaceId,
        host: HostId,
        workspace: WorkspaceId,
        pane: u64,
        machine: &Machine,
    ) -> Option<Self> {
        let target = Self::from_bound_machine(client_workspace, host, workspace, pane, machine)?;
        let facts = machine.panes.iter().find(|p| p.id == pane)?;
        let expected = match target.identity.provider {
            Provider::Codex => crate::core::cli_agent::CLIAgent::Codex,
            Provider::Claude => crate::core::cli_agent::CLIAgent::Claude,
        };
        if !facts.live
            || facts.exit.is_some()
            || facts.ssh_spec.is_some()
            || facts.agent.as_ref().is_some_and(|a| {
                a.agent != expected || a.session_id.as_deref() != Some(target.identity.id.as_str())
            })
        {
            return None;
        }
        Some(target)
    }

    fn from_bound_machine(
        client_workspace: WorkspaceId,
        host: HostId,
        workspace: WorkspaceId,
        pane: u64,
        machine: &Machine,
    ) -> Option<Self> {
        let tab = unique_tab(machine, workspace, pane)?;
        let mut records = machine.panes.iter().filter(|p| p.id == pane);
        let facts = records.next()?;
        if records.next().is_some() {
            return None;
        }
        Some(Self {
            client_workspace,
            host,
            workspace,
            tab: tab.id,
            pane,
            identity: identity(facts)?,
            root: tab.root.clone(),
        })
    }

    fn same_slot(&self, other: &Self) -> bool {
        self.client_workspace == other.client_workspace
            && self.host == other.host
            && self.workspace == other.workspace
            && self.tab == other.tab
            && self.pane == other.pane
            && self.identity == other.identity
            && same_slots(&self.root, &other.root)
    }

    fn from_machine(
        client_workspace: WorkspaceId,
        host: HostId,
        workspace: WorkspaceId,
        pane: u64,
        machine: &Machine,
    ) -> Option<Self> {
        let target = Self::from_bound_machine(client_workspace, host, workspace, pane, machine)?;
        let facts = machine.panes.iter().find(|p| p.id == pane)?;
        if facts.exit.is_none() || facts.live || facts.agent.is_some() || facts.ssh_spec.is_some() {
            return None;
        }
        Some(target)
    }

    /// Read-only reconciliation. No first-match or latest-session inference.
    fn reconcile<'a>(&self, machine: &'a Machine) -> Option<(&'a PaneRecord, &'a Tab)> {
        let mut matches = machine.panes.iter().filter_map(|pane| {
            if pane.ssh_spec.is_some()
                || identity(pane).as_ref() != Some(&self.identity)
                || (!pane.live && pane.exit.is_none())
            {
                return None;
            }
            let tab = unique_tab(machine, self.workspace, pane.id)?;
            let mut expected = self.root.clone();
            expected.replace_leaf(self.pane, pane.id);
            (tab.id == self.tab && same_slots(&tab.root, &expected)).then_some((pane, tab))
        });
        let found = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(found)
    }

    fn reconcile_current(
        &self,
        snapshot: &Machine,
        current: &Machine,
    ) -> Option<(PaneRecord, Tab)> {
        let (facts, tab) = self.reconcile(snapshot)?;
        let (latest_facts, latest) = self.reconcile(current)?;
        if !same_slots(&latest.root, &self.root) && !same_slots(&latest.root, &tab.root) {
            return None;
        }
        if latest_facts.id == facts.id {
            return Some((latest_facts.clone(), latest.clone()));
        }
        Some((facts.clone(), tab.clone()))
    }
}

/// Pane identity includes its split position and neighbors, not the amount of
/// screen space currently assigned to it. Do not flatten the tree: equal leaf
/// order alone would also accept reparenting into a different split.
fn same_slots(a: &PaneNode, b: &PaneNode) -> bool {
    match (a, b) {
        (PaneNode::Leaf { pane: a }, PaneNode::Leaf { pane: b }) => a == b,
        (
            PaneNode::Split { axis: ax, a, b, .. },
            PaneNode::Split {
                axis: bx,
                a: c,
                b: d,
                ..
            },
        ) => ax == bx && same_slots(a, c) && same_slots(b, d),
        _ => false,
    }
}

pub(crate) fn candidate(cx: &App, owner: WorkspaceId, pane: u64) -> Option<ResumeTarget> {
    let view = WorkspaceStore::all(cx).get(owner)?;
    let host = view.host_id();
    let workspace = view.host.as_ref().map_or(owner, |remote| remote.workspace);
    ResumeTarget::from_machine(
        owner,
        host,
        workspace,
        pane,
        super::machine_mirror::MachineMirrors::machine(cx, host)?,
    )
}

#[derive(Clone)]
pub(crate) struct ResumeRequested {
    pub target: ResumeTarget,
    pub generation: u64,
    pub action: RecoveryAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryAction {
    Resume,
    Refresh,
    Output,
}

pub(crate) struct ResumeUi {
    pub request: ResumeRequested,
    pub busy: bool,
}

fn accepted_resume_reply(target: &ResumeTarget, reply: ReplyOk) -> Option<u64> {
    match reply {
        ReplyOk::StoppedSessionResumed {
            workspace,
            tab,
            predecessor,
            pane,
        } if workspace == target.workspace
            && tab == target.tab
            && predecessor == target.pane
            && pane != predecessor =>
        {
            Some(pane)
        }
        _ => None,
    }
}

fn unavailable(pending: &gpui::Entity<PendingPane>, generation: u64, cx: &mut App) {
    pending.update(cx, |pending, cx| {
        if let Some(state) = &mut pending.resume
            && state.request.generation == generation
        {
            if state.request.action == RecoveryAction::Output {
                pending.resume = None;
                pending.output_unavailable = true;
            } else {
                state.busy = false;
            }
            cx.notify();
        }
    });
}

fn cancel_presentation(pending: &gpui::Entity<PendingPane>, generation: u64, cx: &mut App) {
    pending.update(cx, |p, cx| {
        if p.resume
            .as_ref()
            .is_some_and(|s| s.request.generation == generation)
        {
            p.resume = None;
            p.resume_unavailable = true;
            cx.notify();
        }
    });
}

pub(super) fn release(
    client: Arc<ControlClient>,
    usage: tty7_core::daemon::control::WorkspaceUse,
    cx: &mut App,
) {
    cx.background_executor()
        .spawn(async move {
            let _ = super::host_ops::off_thread(move || client.finish_workspace_use(usage)).await;
        })
        .detach();
}

impl Tty7App {
    pub(super) fn retire_workspace_use(&self, cx: &mut App) {
        self.workspace_view_epoch.set(uuid::Uuid::new_v4());
        let old = self.workspace_use.borrow_mut().take();
        if let Some((client, usage)) = old {
            release(client, usage, cx);
        }
    }

    pub(super) fn retain_workspace_use(
        &self,
        client: Arc<ControlClient>,
        usage: tty7_core::daemon::control::WorkspaceUse,
        cx: &mut App,
    ) {
        let old = self.workspace_use.replace(Some((client, usage)));
        if let Some((client, usage)) = old {
            release(client, usage, cx);
        }
    }

    pub(crate) fn hold_resume_tab(&self, tab: TabId, cx: &App) -> bool {
        self.tabs
            .iter()
            .filter(|t| t.tree_id.get() == tab)
            .flat_map(|t| t.pane.leaves())
            .any(|slot| matches!(slot, PaneSlot::Connecting(p) if p.read(cx).resume.is_some()))
    }

    pub(crate) fn has_resume_pending(&self, cx: &App) -> bool {
        self.tabs
            .iter()
            .any(|tab| self.hold_resume_tab(tab.tree_id.get(), cx))
    }

    fn resume_current(
        &self,
        pending: &gpui::Entity<PendingPane>,
        request: &ResumeRequested,
        cx: &App,
    ) -> bool {
        let target = &request.target;
        self.workspace == target.client_workspace
            && WorkspaceStore::all(cx)
                .get(self.workspace)
                .is_some_and(|view| {
                    view.host_id() == target.host
                        && view.host.as_ref().map_or(self.workspace, |r| r.workspace)
                            == target.workspace
                })
            && self.tabs.iter().any(|tab| {
                tab.tree_id.get() == target.tab
                    && tab
                        .pane
                        .leaves()
                        .iter()
                        .any(|slot| slot.entity_id() == pending.entity_id())
            })
            && pending
                .read(cx)
                .resume
                .as_ref()
                .is_some_and(|s| s.request.generation == request.generation)
    }

    pub(crate) fn resume_stopped(
        &mut self,
        pending: gpui::Entity<PendingPane>,
        request: &ResumeRequested,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let request = request.clone();
        if !self.resume_current(&pending, &request, cx) {
            cancel_presentation(&pending, request.generation, cx);
            return;
        }
        // Reconciliation holds a complete tab topology. Two independent
        // replacements from that same old topology cannot run concurrently.
        if self.tabs.iter().filter(|tab| tab.tree_id.get() == request.target.tab)
            .flat_map(|tab| tab.pane.leaves()).any(|slot| {
                slot.entity_id() != pending.entity_id()
                    && matches!(slot, PaneSlot::Connecting(other) if other.read(cx).resume.is_some())
            }) {
            cancel_presentation(&pending, request.generation, cx);
            window.push_notification(t(L10nKey::HistoryResumePending), cx);
            return;
        }
        let Some(client) = super::tree_sync::control_for(cx, request.target.host) else {
            unavailable(&pending, request.generation, cx);
            return;
        };
        if !client
            .hello()
            .has_feature(crate::daemon::protocol::FEATURE_CHECKED_ATTACH)
        {
            unavailable(&pending, request.generation, cx);
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let worker = client.clone();
            let workspace = request.target.workspace;
            let acquired =
                super::host_ops::off_thread(move || worker.acquire_workspace_use(workspace).ok())
                    .await
                    .flatten();
            let orphaned_lease = acquired.clone();
            let orphaned_client = client.clone();
            let landed = this.update_in(cx, |app, window, cx| {
                let Some(usage) = acquired else {
                    unavailable(&pending, request.generation, cx);
                    return;
                };
                let current_link = super::tree_sync::control_for(cx, request.target.host);
                if !app.resume_current(&pending, &request, cx)
                    || !current_link
                        .as_ref()
                        .is_some_and(|now| Arc::ptr_eq(now, &client))
                    || (request.action != RecoveryAction::Refresh
                        && !candidate(cx, request.target.client_workspace, request.target.pane)
                            .as_ref()
                            .is_some_and(|candidate| candidate.same_slot(&request.target)))
                {
                    release(client, usage, cx);
                    unavailable(&pending, request.generation, cx);
                    return;
                }
                let proof = usage.proof().clone();
                app.retain_workspace_use(client.clone(), usage, cx);
                if request.action == RecoveryAction::Output {
                    app.submit_stopped_output(pending, request, client, proof, window, cx);
                } else {
                    app.submit_stopped_resume(pending, request, client, proof, window, cx);
                }
            });
            if landed.is_err()
                && let Some(usage) = orphaned_lease
            {
                cx.background_executor()
                    .spawn(async move {
                        let _ = super::host_ops::off_thread(move || {
                            orphaned_client.finish_workspace_use(usage)
                        })
                        .await;
                    })
                    .detach();
            }
        })
        .detach();
    }

    fn submit_stopped_output(
        &mut self,
        pending: gpui::Entity<PendingPane>,
        request: ResumeRequested,
        client: Arc<ControlClient>,
        proof: AttachmentProof,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let epoch = self.workspace_view_epoch.get();
        let workspace = self.window_workspace(cx);
        let font_size = pending.read(cx).spawn.font_size;
        let policy = crate::terminal::RestorePolicy::Checked(Arc::new(
            [(
                request.target.pane,
                crate::daemon::protocol::CheckedPaneAttachment {
                    proof,
                    tab: request.target.tab,
                    identity: crate::daemon::protocol::PaneAttachExpectation::Stopped(
                        request.target.identity.clone(),
                    ),
                },
            )]
            .into(),
        ));
        cx.spawn_in(window, async move |this, cx| {
            let target = request.target.clone();
            let preparing_policy = policy.clone();
            let parts = super::host_ops::off_thread(move || {
                crate::terminal::view::TerminalView::spawn_shell_terminal_in(
                    workspace,
                    Some(target.identity.cwd.into()),
                    Some(target.pane),
                    None,
                    Some(target.client_workspace),
                    preparing_policy,
                )
                .map_err(|_| ())
            })
            .await;
            let _ = this.update_in(cx, |app, window, cx| {
                if app.workspace_view_epoch.get() != epoch
                    || !app.resume_current(&pending, &request, cx)
                    || !super::tree_sync::control_for(cx, request.target.host)
                        .is_some_and(|now| Arc::ptr_eq(&now, &client))
                    || !candidate(cx, request.target.client_workspace, request.target.pane)
                        .is_some_and(|now| now.same_slot(&request.target))
                {
                    // Dropping checked read-only parts only closes their stream.
                    // It cannot kill, Resume, or delete the stopped target.
                    unavailable(&pending, request.generation, cx);
                    return;
                }
                let Some(Ok(parts)) = parts else {
                    unavailable(&pending, request.generation, cx);
                    return;
                };
                if !parts.restored || parts.pane_id != request.target.pane {
                    unavailable(&pending, request.generation, cx);
                    return;
                }
                pending.update(cx, |p, _| {
                    p.resume = None;
                    p.output_unavailable = false;
                    p.spawn.restore_policy = policy;
                });
                app.land_pane(
                    pending.entity_id(),
                    &pending,
                    Ok(parts),
                    font_size,
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    fn submit_stopped_resume(
        &mut self,
        pending: gpui::Entity<PendingPane>,
        request: ResumeRequested,
        client: Arc<ControlClient>,
        proof: AttachmentProof,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let worker = client.clone();
            let target = request.target.clone();
            let refresh = request.action == RecoveryAction::Refresh;
            let worker_proof = proof.clone();
            let outcome = super::host_ops::off_thread(move || {
                let expected = if !refresh {
                    // Errors and foreign replies are unknown outcomes. Only
                    // an explicit Refresh may reconcile them; never retry.
                    let reply = worker
                        .call(ControlRequest::ResumeStoppedSession {
                            request: StoppedResumeRequest {
                                proof: worker_proof.clone(),
                                tab: target.tab,
                                pane: target.pane,
                                identity: target.identity.clone(),
                                size: crate::daemon::protocol::WinSize {
                                    cols: 80,
                                    rows: 24,
                                    cell_w: 8,
                                    cell_h: 16,
                                },
                            },
                        })
                        .ok()?;
                    Some(accepted_resume_reply(&target, reply)?)
                } else {
                    None
                };
                let Ok(ReplyOk::MachineTree(machine)) = worker.call(ControlRequest::MachineGet)
                else {
                    return None;
                };
                if expected
                    .is_some_and(|id| target.reconcile(&machine).map(|(p, _)| p.id) != Some(id))
                {
                    return None;
                }
                match worker.call(ControlRequest::WorkspaceAttachmentProof {
                    workspace: target.workspace,
                }) {
                    Ok(ReplyOk::AttachmentProof(current)) if current == worker_proof => {
                        Some(machine)
                    }
                    _ => None,
                }
            })
            .await
            .flatten();
            let _ = this.update_in(cx, |app, window, cx| {
                if !app.resume_current(&pending, &request, cx) {
                    cancel_presentation(&pending, request.generation, cx);
                    return;
                }
                let current = super::tree_sync::control_for(cx, request.target.host);
                if !current
                    .as_ref()
                    .is_some_and(|now| Arc::ptr_eq(now, &client))
                {
                    unavailable(&pending, request.generation, cx);
                    return;
                }
                let Some(machine) = outcome else {
                    unavailable(&pending, request.generation, cx);
                    return;
                };
                let reconciled =
                    super::machine_mirror::MachineMirrors::machine(cx, request.target.host)
                        .and_then(|current| request.target.reconcile_current(&machine, current));
                let Some((facts, tab)) = reconciled else {
                    unavailable(&pending, request.generation, cx);
                    return;
                };
                let index = app
                    .tabs
                    .iter()
                    .position(|t| t.tree_id.get() == tab.id)
                    .unwrap();
                if !super::tree_sync::acknowledge_recovery(
                    cx,
                    request.target.client_workspace,
                    request.target.host,
                    request.target.workspace,
                    &tab,
                    &facts,
                ) {
                    unavailable(&pending, request.generation, cx);
                    return;
                }
                if facts.id == request.target.pane {
                    // Only the target can rearm an unchanged stopped slot.
                    if facts.exit.is_none() {
                        unavailable(&pending, request.generation, cx);
                        return;
                    }
                    pending.update(cx, |p, cx| {
                        p.resume = None;
                        p.resume_unavailable = true;
                        cx.notify();
                    });
                    return;
                }
                let stopped = facts.exit.is_some();
                pending.update(cx, |p, cx| {
                    p.resume = None;
                    p.resume_unavailable = false;
                    p.spawn.restore_pane = Some(facts.id);
                    p.spawn.working_directory = facts.cwd.as_ref().map(Into::into);
                    p.spawn.workspace = app.window_workspace(cx);
                    p.spawn.restore_policy = crate::terminal::RestorePolicy::Checked(Arc::new(
                        [(
                            facts.id,
                            crate::daemon::protocol::CheckedPaneAttachment {
                                identity: crate::daemon::protocol::PaneAttachExpectation::Owned,
                                proof,
                                tab: tab.id,
                            },
                        )]
                        .into(),
                    ));
                    p.state = if stopped {
                        PendingState::Stopped
                    } else {
                        PendingState::Connecting
                    };
                    cx.notify();
                });
                app.rebuild_tab_from_tree(index, &tab, window, cx);
                if !stopped {
                    super::app::start_pane_spawn(pending, window, cx);
                }
                super::machine_mirror::MachineMirrors::refresh(cx, request.target.host);
                cx.notify();
            });
        })
        .detach();
    }
}

pub(super) fn for_workspace(cx: &App, workspace: WorkspaceId) -> HashMap<TabId, PaneRecovery> {
    // A missing mapping cannot be promoted to Local for recovery metadata.
    if crate::core::session::WorkspaceStore::all(cx)
        .get(workspace)
        .is_none()
    {
        return HashMap::new();
    }
    super::machine_mirror::tab_views_for(cx, workspace)
        .into_iter()
        .flat_map(|(tabs, _)| tabs)
        .filter_map(|tab| tab.recovery.map(|recovery| (tab.id, recovery)))
        .collect()
}

fn label(recovery: &PaneRecovery) -> String {
    format!(
        "{} · {}",
        recovery.binding.agent.display_name(),
        t(if recovery.pane_live {
            L10nKey::SessionAgentExited
        } else {
            L10nKey::SessionTerminalStopped
        })
    )
}

pub(super) fn badge(recovery: &PaneRecovery, cx: &App) -> impl IntoElement + use<> {
    div()
        .flex_shrink_0()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(label(recovery))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::core::session::{RemoteRef, RemoteTarget, WindowView, WindowViews, WorkspaceStore};
    use tty7_core::core::machine::{AgentRecoveryBinding, Machine, PaneRecord, Tab, Workspace};

    fn install_stopped_output_slot(
        app: &gpui::Entity<Tty7App>,
        vcx: &mut gpui::VisualTestContext,
    ) -> gpui::Entity<PendingPane> {
        app.update_in(vcx, |app, window, cx| {
            let mut machine = stopped_machine();
            machine.workspaces[0].id = app.workspace;
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![WindowView {
                        id: app.workspace,
                        ..WindowView::default()
                    }],
                    active: Some(app.workspace),
                },
            );
            super::super::machine_mirror::MachineMirrors::install(
                cx,
                HostId::LOCAL,
                machine.clone(),
            );
            let stopped = crate::core::session::SessionPane::Stopped {
                pane_id: 71,
                cwd: Some("/retained".into()),
                shell: None,
            };
            let pane = super::super::app::session_to_pane(
                None,
                app.workspace,
                &stopped,
                None,
                14.,
                crate::terminal::RestorePolicy::AttachOnly,
                window,
                cx,
            )
            .unwrap();
            let PaneSlot::Connecting(pending) = pane.leaves()[0].clone() else {
                panic!("stopped slot")
            };
            app.tabs[0].tree_id.set(machine.workspaces[0].tabs[0].id);
            app.tabs[0].pane = pane;
            cx.notify();
            pending
        })
    }

    #[gpui::test]
    fn stopped_output_missing_link_preserves_resume(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 1);
        let pending = install_stopped_output_slot(&app, &mut vcx);
        vcx.run_until_parked();
        let button = vcx
            .debug_bounds("stopped-pane-output")
            .expect("explicit View output button");
        vcx.simulate_mouse_down(
            button.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        vcx.simulate_mouse_up(
            button.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        vcx.run_until_parked();
        app.read_with(&vcx, |app, cx| {
            assert_eq!(
                app.tabs[0].pane.leaves()[0].entity_id(),
                pending.entity_id()
            );
            assert!(matches!(pending.read(cx).state, PendingState::Stopped));
            assert!(
                pending.read(cx).resume.is_none(),
                "output failure must not enter uncertain Resume"
            );
        });
        assert!(
            vcx.debug_bounds("stopped-pane-resume").is_some(),
            "Resume must remain available"
        );
    }

    #[cfg(unix)]
    #[gpui::test]
    fn stopped_output_click_replays_in_original_slot(cx: &mut gpui::TestAppContext) {
        exercise_stopped_output(
            cx,
            "ui::session_recovery::tests::stopped_output_click_replays_in_original_slot",
            OutputInterruption::None,
        );
    }

    #[cfg(unix)]
    #[gpui::test]
    fn stopped_output_late_response_preserves_stopped_slot(cx: &mut gpui::TestAppContext) {
        exercise_stopped_output(
            cx,
            "ui::session_recovery::tests::stopped_output_late_response_preserves_stopped_slot",
            OutputInterruption::Epoch,
        );
    }

    #[cfg(unix)]
    #[gpui::test]
    fn stopped_output_replaced_connection_rejects_old_response(cx: &mut gpui::TestAppContext) {
        exercise_stopped_output(
            cx,
            "ui::session_recovery::tests::stopped_output_replaced_connection_rejects_old_response",
            OutputInterruption::Connection,
        );
    }

    #[cfg(unix)]
    #[gpui::test]
    fn stopped_output_replaced_entity_rejects_old_response(cx: &mut gpui::TestAppContext) {
        exercise_stopped_output(
            cx,
            "ui::session_recovery::tests::stopped_output_replaced_entity_rejects_old_response",
            OutputInterruption::Entity,
        );
    }

    #[cfg(unix)]
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum OutputInterruption {
        None,
        Epoch,
        Connection,
        Entity,
    }

    #[cfg(unix)]
    fn exercise_stopped_output(
        cx: &mut gpui::TestAppContext,
        name: &str,
        interruption: OutputInterruption,
    ) {
        const CHILD: &str = "AGENTTY_STOPPED_OUTPUT_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--nocapture", "--test-threads=1"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
            return;
        }
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::time::{Duration, Instant};
        use tty7_core::daemon::{protocol::*, transport};
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 1);
        let pending = install_stopped_output_slot(&app, &mut vcx);
        let target = app.read_with(&vcx, |app, cx| candidate(cx, app.workspace, 71).unwrap());
        let proof = AttachmentProof {
            workspace: target.workspace,
            nonce: uuid::Uuid::new_v4(),
        };
        let guard = CheckedPaneAttachment {
            proof: proof.clone(),
            tab: target.tab,
            identity: PaneAttachExpectation::Stopped(target.identity.clone()),
        };
        // Pinning precedes every transport call; never unlink an existing endpoint.
        let dir = std::env::temp_dir().join(format!("tty7-covtest-{}", std::process::id()));
        assert_eq!(crate::core::config::config_dir_path().unwrap(), dir);
        assert_eq!(
            transport::endpoint_display(),
            dir.join("daemon.sock").display().to_string()
        );
        let listener = transport::bind().unwrap();
        listener.set_nonblocking(true).unwrap();
        struct Endpoint(std::path::PathBuf);
        impl Drop for Endpoint {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let endpoint = Endpoint(dir.join("daemon.sock"));
        let (attached_tx, attached_rx) = std::sync::mpsc::channel();
        let (continue_tx, continue_rx) = std::sync::mpsc::channel();
        let pane_server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            for probe in [true, false] {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "no stopped output connection");
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("accept: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let message = ClientMsg::read(&mut stream).unwrap();
                if probe {
                    assert_eq!(message, ClientMsg::Version);
                    DaemonMsg::Version(DaemonVersion::current())
                        .encode(&mut stream)
                        .unwrap();
                } else {
                    let ClientMsg::AttachChecked {
                        pane_id,
                        guard: actual,
                        size,
                        ..
                    } = message
                    else {
                        panic!("expected stopped checked attachment: {message:?}");
                    };
                    assert_eq!(pane_id, 71);
                    assert_eq!(actual, guard);
                    attached_tx.send(()).unwrap();
                    continue_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                    DaemonMsg::Size(size).encode(&mut stream).unwrap();
                    DaemonMsg::Snapshot(b"saved-output\r\n".to_vec())
                        .encode(&mut stream)
                        .unwrap();
                    DaemonMsg::Exited { code: Some(0) }
                        .encode(&mut stream)
                        .unwrap();
                }
            }
            (listener, endpoint)
        });
        let observed = Arc::new(AtomicUsize::new(0));
        let counter = observed.clone();
        let (client, server) = super::super::test_control::control_fixture_with_features(
            vec![
                (
                    ControlRequest::WorkspaceAttachIfFree {
                        id: proof.workspace.to_string(),
                    },
                    ReplyOk::WorkspaceAcquired {
                        proof: proof.clone(),
                        newly_acquired: true,
                    },
                    false,
                ),
                (
                    ControlRequest::WorkspaceReleaseProof { proof },
                    ReplyOk::Unit,
                    false,
                ),
            ],
            Arc::new(AtomicBool::new(true)),
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            },
            vec![FEATURE_CHECKED_ATTACH.into()],
        );
        let client = Arc::new(client);
        vcx.update(|_, cx| {
            super::super::local_link::LocalLink::install_client_for_test(cx, client.clone())
        });
        vcx.run_until_parked();
        let button = vcx
            .debug_bounds("stopped-pane-output")
            .expect("View output button");
        vcx.simulate_mouse_down(
            button.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        vcx.simulate_mouse_up(
            button.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            vcx.run_until_parked();
            if attached_rx.try_recv().is_ok() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "output did not reach checked attachment"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        app.read_with(&vcx, |app, cx| {
            assert_eq!(
                app.tabs[0].pane.leaves()[0].entity_id(),
                pending.entity_id()
            );
            let request = pending.read(cx).resume.as_ref().unwrap();
            assert!(request.busy);
            assert_eq!(request.request.action, RecoveryAction::Output);
        });
        // Both actions must stay inert while the checked response is outstanding.
        for selector in ["stopped-pane-output", "stopped-pane-resume"] {
            let button = vcx
                .debug_bounds(selector)
                .expect("disabled recovery action");
            vcx.simulate_mouse_down(
                button.center(),
                gpui::MouseButton::Left,
                gpui::Modifiers::default(),
            );
            vcx.simulate_mouse_up(
                button.center(),
                gpui::MouseButton::Left,
                gpui::Modifiers::default(),
            );
        }
        vcx.run_until_parked();
        assert_eq!(observed.load(Ordering::SeqCst), 1);
        let epoch = app.read_with(&vcx, |app, _| app.workspace_view_epoch.get());
        let mut current_pending = pending.clone();
        let mut replacement_peer = None;
        match interruption {
            OutputInterruption::None => {}
            OutputInterruption::Epoch => {
                app.update(&mut vcx, |app, cx| app.retire_workspace_use(cx));
            }
            OutputInterruption::Connection => {
                let (replacement, peer) = super::super::test_control::control_fixture_with_features(
                    vec![],
                    Arc::new(AtomicBool::new(true)),
                    |_| panic!("old output must never submit control on the replacement client"),
                    vec![FEATURE_CHECKED_ATTACH.into()],
                );
                let replacement = Arc::new(replacement);
                assert_eq!(replacement.hello().instance, client.hello().instance);
                assert!(!Arc::ptr_eq(&replacement, &client));
                vcx.update(|_, cx| {
                    super::super::local_link::LocalLink::install_client_for_test(cx, replacement)
                });
                replacement_peer = Some(peer);
            }
            OutputInterruption::Entity => {
                current_pending = app.update_in(&mut vcx, |app, window, cx| {
                    let stopped = crate::core::session::SessionPane::Stopped {
                        pane_id: target.pane,
                        cwd: Some(target.identity.cwd.clone().into()),
                        shell: None,
                    };
                    let pane = super::super::app::session_to_pane(
                        None,
                        app.workspace,
                        &stopped,
                        None,
                        14.,
                        crate::terminal::RestorePolicy::AttachOnly,
                        window,
                        cx,
                    )
                    .unwrap();
                    let PaneSlot::Connecting(fresh) = pane.leaves()[0].clone() else {
                        panic!("fresh stopped placeholder");
                    };
                    assert_ne!(fresh.entity_id(), pending.entity_id());
                    app.tabs[0].pane = pane;
                    cx.notify();
                    fresh
                });
            }
        }
        if interruption != OutputInterruption::Epoch {
            assert_eq!(
                app.read_with(&vcx, |app, _| app.workspace_view_epoch.get()),
                epoch
            );
        }
        continue_tx.send(()).unwrap();
        loop {
            vcx.run_until_parked();
            let landed = app.read_with(&vcx, |app, cx| {
                if interruption != OutputInterruption::None {
                    assert_eq!(
                        app.tabs[0].pane.leaves()[0].entity_id(),
                        current_pending.entity_id(),
                        "stale output replaced the stopped placeholder"
                    );
                    return pending.read(cx).resume.is_none();
                }
                let Some(view) = app.tabs[0].pane.leaves()[0].terminal().cloned() else {
                    return false;
                };
                let view = view.read(cx);
                if !view.terminal.child_exited() {
                    return false;
                }
                let terminal = view.terminal.term.lock();
                let output: String = (0..12)
                    .map(|col| {
                        terminal.grid()[alacritty_terminal::index::Line(0)]
                            [alacritty_terminal::index::Column(col)]
                        .c
                    })
                    .collect();
                view.pane_id == 71 && output == "saved-output"
            });
            if landed {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "saved output did not land in original slot"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        app.read_with(&vcx, |app, cx| {
            assert_eq!(app.tabs.len(), 1);
            assert_eq!(app.tabs[0].tree_id.get(), target.tab);
            assert!(candidate(cx, app.workspace, 71).unwrap().same_slot(&target));
            assert!(pending.read(cx).resume.is_none());
        });
        if interruption != OutputInterruption::None {
            pending.read_with(&vcx, |pending, _| {
                assert!(matches!(pending.state, PendingState::Stopped));
                assert!(pending.output_unavailable);
            });
            if interruption == OutputInterruption::Entity {
                current_pending.read_with(&vcx, |fresh, _| {
                    assert!(matches!(fresh.state, PendingState::Stopped));
                    assert!(fresh.resume.is_none());
                    assert!(
                        !fresh.output_unavailable,
                        "old failure contaminated replacement view"
                    );
                });
            }
            assert!(vcx.debug_bounds("stopped-pane-resume").is_some());
        } else {
            assert!(
                vcx.debug_bounds("retained-pane-resume").is_some(),
                "viewing output must retain Resume"
            );
            assert_eq!(
                observed.load(Ordering::SeqCst),
                1,
                "preview must not Resume"
            );
        }
        if interruption != OutputInterruption::Epoch {
            assert_eq!(observed.load(Ordering::SeqCst), 1);
            app.update(&mut vcx, |app, cx| app.retire_workspace_use(cx));
        }
        while observed.load(Ordering::SeqCst) != 2 {
            vcx.run_until_parked();
            assert!(
                Instant::now() < deadline,
                "tracked output ownership was not retired"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        vcx.run_until_parked();
        vcx.update(|_, cx| super::super::local_link::LocalLink::invalidate(cx));
        drop(client);
        server.join().unwrap();
        if let Some(peer) = replacement_peer {
            peer.join().unwrap();
        }
        drop(pane_server.join().unwrap());
        drop(current_pending);
        drop(pending);
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn live_sleep_requires_exact_visible_target(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 1);
        let _output_peer = app.update_in(&mut vcx, |app, window, cx| {
            let foreign = app.tabs[0].pane.leaves()[0].terminal().unwrap().clone();
            let (view, _stream) =
                crate::terminal::view::quiet_test_owned_pane(71, app.workspace, window, cx);
            let mut machine = stopped_machine();
            machine.workspaces[0].id = app.workspace;
            machine.panes[0].exit = None;
            machine.panes[0].live = true;
            app.tabs[0].tree_id.set(machine.workspaces[0].tabs[0].id);
            app.tabs[0].pane = super::super::pane::Pane::leaf(PaneSlot::Ready(view.clone()));
            window.focus(&view.read(cx).focus_handle.clone(), cx);
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![WindowView {
                        id: app.workspace,
                        ..WindowView::default()
                    }],
                    active: Some(app.workspace),
                },
            );
            super::super::machine_mirror::MachineMirrors::install(
                cx,
                HostId::LOCAL,
                machine.clone(),
            );
            let expected = app
                .live_sleep_target(&view, cx)
                .expect("exact target is eligible");
            let request = SleepUi {
                members: None,
                tab_reply: None,
                target: expected.clone(),
                connection: std::sync::Weak::new(),
                view: view.downgrade(),
                epoch: app.workspace_view_epoch.get(),
                generation: uuid::Uuid::new_v4(),
                busy: true,
                accepted: false,
            };
            app.sleep_ui = Some(request.clone());
            app.sleep_current_session(view.clone(), &expected, window, cx);
            assert_eq!(
                app.sleep_ui.as_ref().unwrap().generation,
                request.generation,
                "duplicate click replaced an in-flight request"
            );
            app.workspace_view_epoch.set(uuid::Uuid::new_v4());
            app.finish_sleep_reply(&request, true, cx);
            assert!(
                app.sleep_ui.as_ref().unwrap().busy,
                "stale reply updated a retired view"
            );
            app.workspace_view_epoch.set(request.epoch);
            app.finish_sleep_reply(&request, true, cx);
            assert!(app.sleep_ui.as_ref().unwrap().accepted);
            assert!(
                !view.read(cx).terminal.child_exited(),
                "ack fabricated termination"
            );
            app.sleep_ui = None;
            for case in ["unknown", "exit", "binding", "duplicate", "tab", "runtime"] {
                let mut changed = machine.clone();
                match case {
                    "unknown" => changed.panes[0].live = false,
                    "exit" => {
                        changed.panes[0].exit =
                            Some(tty7_core::core::machine::PaneExit { code: Some(0) })
                    }
                    "binding" => changed.panes[0].recovery_binding = None,
                    "duplicate" => changed.workspaces[0].tabs.push(Tab::leaf(71)),
                    "tab" => changed.workspaces[0].tabs[0].id = TabId::new(),
                    "runtime" => {
                        changed.panes[0].agent = Some(tty7_core::core::machine::AgentFacts {
                            agent: crate::core::cli_agent::CLIAgent::Claude,
                            session_id: None,
                            launch_argv: None,
                            status: None,
                        })
                    }
                    _ => unreachable!(),
                }
                super::super::machine_mirror::MachineMirrors::install(cx, HostId::LOCAL, changed);
                assert!(app.live_sleep_target(&view, cx).is_none(), "{case}");
                assert!(app.render_session_sleep(window, cx).is_none(), "{case}");
            }
            super::super::machine_mirror::MachineMirrors::install(
                cx,
                HostId::LOCAL,
                machine.clone(),
            );
            let original = app.tabs[0].pane.leaves()[0].entity_id();
            app.sleep_current_session(view.clone(), &expected, window, cx);
            assert_eq!(
                app.tabs[0].pane.leaves()[0].entity_id(),
                original,
                "missing control must not close or replace the terminal"
            );
            assert!(
                app.sleep_ui.is_none(),
                "missing control cannot fabricate a request"
            );
            app.maximized = Some(foreign);
            assert!(
                app.render_session_sleep(window, cx).is_none(),
                "hidden focused terminal cannot sleep through a different maximized view"
            );
            app.maximized = None;
            _stream
        });
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("current-session-sleep").is_some(),
            "eligible target must render the actual action"
        );
    }

    fn install_sleep_split(
        app: &mut Tty7App,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Tty7App>,
    ) -> (Vec<crate::daemon::transport::Stream>, Machine) {
        let (a, peer_a) =
            crate::terminal::view::quiet_test_owned_pane(71, app.workspace, window, cx);
        let (b, peer_b) =
            crate::terminal::view::quiet_test_owned_pane(72, app.workspace, window, cx);
        let mut machine = stopped_machine();
        machine.workspaces[0].id = app.workspace;
        machine.panes[0].live = true;
        machine.panes[0].exit = None;
        let mut second = machine.panes[0].clone();
        second.recovery_binding.as_mut().unwrap().session_id =
            "01900000-0000-7000-8000-000000000002".into();
        second.id = 72;
        machine.panes.push(second);
        machine.workspaces[0].tabs[0].root.split_leaf(
            71,
            72,
            tty7_core::core::machine::Axis::Horizontal,
            0.5,
            false,
        );
        app.tabs[0].tree_id.set(machine.workspaces[0].tabs[0].id);
        app.tabs[0].pane = super::super::pane::Pane::split_node(
            gpui::Axis::Horizontal,
            0.5,
            super::super::pane::Pane::leaf(PaneSlot::Ready(a.clone())),
            super::super::pane::Pane::leaf(PaneSlot::Ready(b)),
        );
        window.focus(&a.read(cx).focus_handle.clone(), cx);
        WorkspaceStore::install_for_test(
            cx,
            WindowViews {
                views: vec![WindowView {
                    id: app.workspace,
                    ..WindowView::default()
                }],
                active: Some(app.workspace),
            },
        );
        super::super::machine_mirror::MachineMirrors::install(cx, HostId::LOCAL, machine.clone());
        cx.notify();
        (vec![peer_a, peer_b], machine)
    }

    #[gpui::test]
    fn whole_tab_sleep_button_is_rendered(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 1);
        let (_peers, _machine) = app.update_in(&mut vcx, |app, window, cx| {
            install_sleep_split(app, window, cx)
        });
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("current-tab-sleep").is_some(),
            "multi-pane tab needs an explicit whole-tab action"
        );
        assert!(
            vcx.debug_bounds("current-session-sleep").is_some(),
            "single-session action remains available"
        );
    }

    #[gpui::test]
    fn whole_tab_sleep_scope_requires_every_member(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 1);
        let _peers = app.update_in(&mut vcx, |app, window, cx| {
            let (mut peers, machine) = install_sleep_split(app, window, cx);
            let tab = app.tabs[0].tree_id.get();
            let plan = app.tab_sleep_plan(tab, false, cx).unwrap();
            assert_eq!(
                plan.wire_members()
                    .iter()
                    .map(|m| m.pane)
                    .collect::<Vec<_>>(),
                vec![71, 72]
            );
            let view = app.tabs[0].pane.leaves()[0].terminal().unwrap().clone();
            let request = SleepUi {
                target: plan.members[0].target.clone(),
                view: view.downgrade(),
                members: Some(plan.clone()),
                tab_reply: None,
                connection: std::sync::Weak::new(),
                epoch: app.workspace_view_epoch.get(),
                generation: uuid::Uuid::new_v4(),
                busy: true,
                accepted: false,
            };
            app.sleep_ui = Some(request.clone());
            for mode in ["exit", "unknown", "binding", "foreign-owner", "session"] {
                let mut changed = machine.clone();
                match mode {
                    "exit" => {
                        changed.panes[0].exit =
                            Some(tty7_core::core::machine::PaneExit { code: Some(0) });
                        changed.panes[0].live = false;
                        changed.panes[0].agent = None;
                    }
                    "unknown" => changed.panes[1].live = false,
                    "binding" => changed.panes[1].recovery_binding = None,
                    "foreign-owner" => changed.workspaces[0].tabs.push(Tab::leaf(72)),
                    "session" => {
                        changed.panes[1]
                            .recovery_binding
                            .as_mut()
                            .unwrap()
                            .session_id = "01900000-0000-7000-8000-000000000003".into()
                    }
                    _ => unreachable!(),
                }
                super::super::machine_mirror::MachineMirrors::install(cx, HostId::LOCAL, changed);
                assert_eq!(
                    app.sleep_scope_current(&request, cx),
                    matches!(mode, "exit" | "unknown"),
                    "{mode}"
                );
                if mode == "exit" {
                    let mixed = app.tab_sleep_plan(tab, false, cx).unwrap();
                    assert!(plan.same_members(&mixed));
                    assert_eq!(
                        mixed.members.iter().map(|m| m.live).collect::<Vec<_>>(),
                        vec![false, true]
                    );
                } else if mode != "session" {
                    assert!(app.tab_sleep_plan(tab, false, cx).is_none(), "{mode}");
                }
            }
            super::super::machine_mirror::MachineMirrors::install(cx, HostId::LOCAL, machine);
            app.workspace_view_epoch.set(uuid::Uuid::new_v4());
            assert!(!app.sleep_scope_current(&request, cx));
            app.workspace_view_epoch.set(request.epoch);
            let (replacement, peer) =
                crate::terminal::view::quiet_test_owned_pane(72, app.workspace, window, cx);
            peers.push(peer);
            app.tabs[0].pane = super::super::pane::Pane::split_node(
                gpui::Axis::Horizontal,
                0.5,
                super::super::pane::Pane::leaf(PaneSlot::Ready(view)),
                super::super::pane::Pane::leaf(PaneSlot::Ready(replacement)),
            );
            assert!(app.tab_sleep_plan(tab, false, cx).is_some());
            assert!(
                !app.sleep_scope_current(&request, cx),
                "same pane ID on a replacement entity cannot consume an old reply"
            );
            peers
        });
    }

    #[test]
    fn whole_tab_sleep_reply_rejects_foreign_and_unordered_results() {
        use tty7_core::daemon::control::{
            SleepTabFailure, SleepTabReply, WireError, WireErrorKind,
        };
        for (accepted, stopped, failed, valid) in [
            (vec![71, 72, 73], vec![], None, true),
            (vec![71, 73], vec![72], None, true),
            (vec![], vec![71, 72, 73], None, true),
            (vec![71], vec![], Some(72), true),
            (vec![71], vec![72], Some(73), true),
            (vec![71], vec![], None, false),
            (vec![72, 71, 73], vec![], None, false),
            (vec![71, 71, 73], vec![], None, false),
            (vec![71, 72, 99], vec![], None, false),
            (vec![71, 73], vec![72, 72], None, false),
            (vec![], vec![72, 71, 73], None, false),
            (vec![71], vec![], Some(73), false),
            (vec![71, 72, 73], vec![], Some(99), false),
        ] {
            let reply = SleepTabReply {
                accepted,
                already_stopped: stopped,
                failed: failed.map(|pane| SleepTabFailure {
                    pane,
                    error: WireError::new(WireErrorKind::Other, "fixture"),
                }),
            };
            assert_eq!(
                valid_tab_sleep_reply(&[71, 72, 73], &reply),
                valid,
                "{reply:?}"
            );
        }
    }

    #[gpui::test]
    fn whole_tab_sleep_late_success_rejects_replaced_connection(cx: &mut gpui::TestAppContext) {
        exercise_sleep_late_success(cx, "connection");
    }

    #[gpui::test]
    fn whole_tab_sleep_late_success_rejects_replaced_entity(cx: &mut gpui::TestAppContext) {
        exercise_sleep_late_success(cx, "entity");
    }

    #[gpui::test]
    fn whole_tab_sleep_late_success_rejects_changed_epoch(cx: &mut gpui::TestAppContext) {
        exercise_sleep_late_success(cx, "epoch");
    }

    fn exercise_sleep_late_success(cx: &mut gpui::TestAppContext, interruption: &str) {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 1);
        let (mut peers, machine) = app.update_in(&mut vcx, |app, window, cx| {
            install_sleep_split(app, window, cx)
        });
        let proof = AttachmentProof {
            workspace: machine.workspaces[0].id,
            nonce: uuid::Uuid::new_v4(),
        };
        let received = Arc::new(AtomicUsize::new(0));
        let observed = received.clone();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let (client, server) = super::super::test_control::control_fixture(
            vec![
                (
                    ControlRequest::WorkspaceAttachIfFree {
                        id: proof.workspace.to_string(),
                    },
                    ReplyOk::WorkspaceAcquired {
                        proof: proof.clone(),
                        newly_acquired: true,
                    },
                    false,
                ),
                (
                    ControlRequest::SleepTab {
                        request: tty7_core::daemon::control::SleepTabRequest {
                            proof: proof.clone(),
                            tab: machine.workspaces[0].tabs[0].id,
                            panes: machine
                                .panes
                                .iter()
                                .map(|p| tty7_core::daemon::control::SleepTabPane {
                                    pane: p.id,
                                    identity: identity(p).unwrap(),
                                })
                                .collect(),
                        },
                    },
                    ReplyOk::TabSleep(tty7_core::daemon::control::SleepTabReply {
                        accepted: vec![71, 72],
                        already_stopped: vec![],
                        failed: None,
                    }),
                    false,
                ),
                (
                    ControlRequest::WorkspaceReleaseProof { proof },
                    ReplyOk::Unit,
                    false,
                ),
            ],
            Arc::new(AtomicBool::new(true)),
            move |req| {
                observed.fetch_add(1, Ordering::SeqCst);
                if matches!(req, ControlRequest::SleepTab { .. }) {
                    resume_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                }
            },
        );
        let client = Arc::new(client);
        vcx.update(|_, cx| {
            super::super::local_link::LocalLink::install_client_for_test(cx, client.clone());
        });
        // The fixture and LocalLink are the only pre-operation strong owners.
        let idle_owners = Arc::strong_count(&client);
        assert_eq!(idle_owners, 2);
        app.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        let bounds = vcx.debug_bounds("current-tab-sleep").unwrap();
        vcx.simulate_mouse_down(
            bounds.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        vcx.simulate_mouse_up(
            bounds.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while received.load(Ordering::SeqCst) < 2 {
            vcx.run_until_parked();
            assert!(
                std::time::Instant::now() < deadline,
                "SleepTab never reached peer"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let original = app.read_with(&vcx, |app, _| app.sleep_ui.as_ref().unwrap().clone());
        assert!(original.busy);
        let mut replacement_server = None;
        match interruption {
            "connection" => {
                let (replacement, peer) = super::super::test_control::control_fixture(
                    vec![],
                    Arc::new(AtomicBool::new(true)),
                    |_| panic!("stale Sleep must not submit on the replacement connection"),
                );
                assert_eq!(replacement.hello().instance, client.hello().instance);
                vcx.update(|_, cx| {
                    super::super::local_link::LocalLink::install_client_for_test(
                        cx,
                        Arc::new(replacement),
                    );
                });
                replacement_server = Some(peer);
            }
            "entity" => app.update_in(&mut vcx, |app, window, cx| {
                let first = app.tabs[0].pane.leaves()[0].clone();
                let (replacement, peer) =
                    crate::terminal::view::quiet_test_owned_pane(72, app.workspace, window, cx);
                peers.push(peer);
                app.tabs[0].pane = super::super::pane::Pane::split_node(
                    gpui::Axis::Horizontal,
                    0.5,
                    super::super::pane::Pane::leaf(first),
                    super::super::pane::Pane::leaf(PaneSlot::Ready(replacement)),
                );
                cx.notify();
            }),
            "epoch" => app.update(&mut vcx, |app, cx| {
                app.workspace_view_epoch.set(uuid::Uuid::new_v4());
                cx.notify();
            }),
            _ => unreachable!(),
        }
        let current_entities = app.read_with(&vcx, |app, cx| {
            assert_eq!(
                app.workspace_view_epoch.get() == original.epoch,
                interruption != "epoch"
            );
            assert_eq!(
                app.sleep_scope_current(&original, cx),
                interruption == "connection"
            );
            app.tabs[0]
                .pane
                .leaves()
                .iter()
                .map(|p| p.entity_id())
                .collect::<Vec<_>>()
        });
        resume_tx.send(()).unwrap();
        // One retained workspace-use owner remains. Losing the cached connection
        // removes one owner. Until the async response handler finishes it owns
        // another Arc, so this is a completion barrier, not a timing guess.
        let settled_owners = idle_owners + 1 - usize::from(interruption == "connection");
        loop {
            vcx.run_until_parked();
            if Arc::strong_count(&client) == settled_owners {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "old Sleep task did not retire"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app.update(&mut vcx, |app, cx| {
            let state = app.sleep_ui.as_ref().unwrap();
            assert_eq!(state.generation, original.generation);
            assert!(state.busy, "old reply mutated retired presentation");
            assert!(!state.accepted);
            assert!(state.tab_reply.is_none());
            assert_eq!(app.tabs[0].tree_id.get(), machine.workspaces[0].tabs[0].id);
            assert_eq!(
                app.tabs[0]
                    .pane
                    .leaves()
                    .iter()
                    .map(|p| p.entity_id())
                    .collect::<Vec<_>>(),
                current_entities
            );
            for slot in app.tabs[0].pane.leaves() {
                assert!(!slot.terminal().unwrap().read(cx).terminal.child_exited());
            }
            assert_eq!(
                received.load(Ordering::SeqCst),
                2,
                "stale reply triggered another request"
            );
            app.retire_workspace_use(cx);
        });
        loop {
            vcx.run_until_parked();
            if received.load(Ordering::SeqCst) == 3
                && Arc::strong_count(&client) == settled_owners - 1
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "tracked release did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        vcx.update(|_, cx| super::super::local_link::LocalLink::invalidate(cx));
        drop(client);
        server.join().unwrap();
        if let Some(peer) = replacement_server {
            peer.join().unwrap();
        }
    }

    #[gpui::test]
    fn whole_tab_sleep_preserves_cold_stopped_neighbor(cx: &mut gpui::TestAppContext) {
        exercise_sleep_control(cx, "tab-cold");
    }

    #[gpui::test]
    fn whole_tab_sleep_uses_one_checked_request(cx: &mut gpui::TestAppContext) {
        exercise_sleep_control(cx, "tab-accepted");
    }

    #[gpui::test]
    fn whole_tab_sleep_partial_reply_never_retries(cx: &mut gpui::TestAppContext) {
        exercise_sleep_control(cx, "tab-partial");
    }

    #[gpui::test]
    fn whole_tab_sleep_unknown_reply_never_retries(cx: &mut gpui::TestAppContext) {
        exercise_sleep_control(cx, "tab-unknown");
    }

    #[gpui::test]
    fn whole_tab_sleep_failed_acquisition_clears_busy_after_runtime_changes(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 1);
        let (_peers, mut machine) = app.update_in(&mut vcx, |app, window, cx| {
            install_sleep_split(app, window, cx)
        });
        let received = Arc::new(AtomicBool::new(false));
        let seen = received.clone();
        let (release, wait) = std::sync::mpsc::channel();
        let (client, server) = super::super::test_control::control_fixture(
            vec![(
                ControlRequest::WorkspaceAttachIfFree {
                    id: machine.workspaces[0].id.to_string(),
                },
                ReplyOk::Pong,
                false,
            )],
            Arc::new(AtomicBool::new(true)),
            move |_| {
                seen.store(true, Ordering::SeqCst);
                wait.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            },
        );
        let client = Arc::new(client);
        vcx.update(|_, cx| {
            super::super::local_link::LocalLink::install_client_for_test(cx, client.clone())
        });
        app.update_in(&mut vcx, |app, window, cx| {
            let plan = app
                .tab_sleep_plan(app.tabs[0].tree_id.get(), false, cx)
                .unwrap();
            app.sleep_entire_tab(&plan, window, cx);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !received.load(Ordering::SeqCst) {
            vcx.run_until_parked();
            assert!(
                std::time::Instant::now() < deadline,
                "acquisition did not reach peer"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        machine.panes[1].live = false;
        vcx.update(|_, cx| {
            super::super::machine_mirror::MachineMirrors::install(cx, HostId::LOCAL, machine)
        });
        release.send(()).unwrap();
        loop {
            vcx.run_until_parked();
            if app.read_with(&vcx, |app, _| {
                app.sleep_ui.as_ref().is_some_and(|s| !s.busy)
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "failed acquisition must not leave a matching tab permanently busy"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app.update(&mut vcx, |app, cx| {
            assert!(!app.sleep_ui.as_ref().unwrap().accepted);
            assert!(
                app.tab_sleep_plan(app.tabs[0].tree_id.get(), false, cx)
                    .is_none()
            );
            app.retire_workspace_use(cx);
        });
        vcx.update(|_, cx| super::super::local_link::LocalLink::invalidate(cx));
        drop(client);
        server.join().unwrap();
    }

    pub(crate) fn stopped_machine() -> Machine {
        Machine {
            workspaces: vec![Workspace {
                tabs: vec![Tab::leaf(71)],
                ..Workspace::default()
            }],
            panes: vec![PaneRecord {
                exit: Some(tty7_core::core::machine::PaneExit { code: Some(0) }),
                recovery_binding: Some(AgentRecoveryBinding {
                    agent: crate::core::cli_agent::CLIAgent::Codex,
                    session_id: "01900000-0000-7000-8000-000000000001".into(),
                    cwd: "/retained".into(),
                    launch_argv: None,
                }),
                ..PaneRecord::new(71)
            }],
        }
    }

    #[gpui::test]
    fn sleep_button_uses_checked_control_and_refresh_never_retries(cx: &mut gpui::TestAppContext) {
        exercise_sleep_control(cx, "single-accepted");
    }

    #[gpui::test]
    fn sleep_unknown_reply_preserves_view_until_explicit_refresh(cx: &mut gpui::TestAppContext) {
        exercise_sleep_control(cx, "single-unknown");
    }

    fn exercise_sleep_control(cx: &mut gpui::TestAppContext, case: &str) {
        let whole = case.starts_with("tab-");
        let cold = case == "tab-cold";
        let accepted = matches!(case, "single-accepted" | "tab-accepted" | "tab-cold");
        let selector = if whole {
            "current-tab-sleep"
        } else {
            "current-session-sleep"
        };

        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use tty7_core::daemon::control::SleepSessionRequest;
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 1);
        let (view, _output_peer, machine, proof) = app.update_in(&mut vcx, |app, window, cx| {
            if whole {
                let (peers, mut machine) = install_sleep_split(app, window, cx);
                let view = app.tabs[0].pane.leaves()[0].terminal().unwrap().clone();
                if cold {
                    machine.panes[1].live = false;
                    machine.panes[1].exit =
                        Some(tty7_core::core::machine::PaneExit { code: Some(0) });
                    let stopped = super::super::app::session_to_pane(
                        None,
                        app.workspace,
                        &crate::core::session::SessionPane::Stopped {
                            pane_id: 72,
                            cwd: Some("/retained".into()),
                            shell: None,
                        },
                        None,
                        14.,
                        crate::terminal::RestorePolicy::AttachOnly,
                        window,
                        cx,
                    )
                    .unwrap();
                    app.tabs[0].pane = super::super::pane::Pane::split_node(
                        gpui::Axis::Horizontal,
                        0.5,
                        super::super::pane::Pane::leaf(PaneSlot::Ready(view.clone())),
                        stopped,
                    );
                    super::super::machine_mirror::MachineMirrors::install(
                        cx,
                        HostId::LOCAL,
                        machine.clone(),
                    );
                }
                let proof = AttachmentProof {
                    workspace: app.workspace,
                    nonce: uuid::Uuid::new_v4(),
                };
                return (view, peers, machine, proof);
            }

            let (view, peer) =
                crate::terminal::view::quiet_test_owned_pane(71, app.workspace, window, cx);
            let mut machine = stopped_machine();
            machine.workspaces[0].id = app.workspace;
            machine.panes[0].exit = None;
            machine.panes[0].live = true;
            app.tabs[0].tree_id.set(machine.workspaces[0].tabs[0].id);
            app.tabs[0].pane = super::super::pane::Pane::leaf(PaneSlot::Ready(view.clone()));
            window.focus(&view.read(cx).focus_handle.clone(), cx);
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![WindowView {
                        id: app.workspace,
                        ..WindowView::default()
                    }],
                    active: Some(app.workspace),
                },
            );
            super::super::machine_mirror::MachineMirrors::install(
                cx,
                HostId::LOCAL,
                machine.clone(),
            );
            let proof = AttachmentProof {
                workspace: app.workspace,
                nonce: uuid::Uuid::new_v4(),
            };
            (view, vec![peer], machine, proof)
        });
        let original_entities = app.read_with(&vcx, |app, _| {
            app.tabs[0]
                .pane
                .leaves()
                .iter()
                .map(|p| p.entity_id())
                .collect::<Vec<_>>()
        });
        let observed = Arc::new(AtomicUsize::new(0));
        let counter = observed.clone();
        let exchanges = vec![
            (
                ControlRequest::WorkspaceAttachIfFree {
                    id: proof.workspace.to_string(),
                },
                ReplyOk::WorkspaceAcquired {
                    proof: proof.clone(),
                    newly_acquired: true,
                },
                false,
            ),
            (
                if whole {
                    ControlRequest::SleepTab {
                        request: tty7_core::daemon::control::SleepTabRequest {
                            proof: proof.clone(),
                            tab: machine.workspaces[0].tabs[0].id,
                            panes: machine
                                .panes
                                .iter()
                                .map(|p| tty7_core::daemon::control::SleepTabPane {
                                    pane: p.id,
                                    identity: identity(p).unwrap(),
                                })
                                .collect(),
                        },
                    }
                } else {
                    ControlRequest::SleepSession {
                        request: SleepSessionRequest {
                            proof: proof.clone(),
                            tab: machine.workspaces[0].tabs[0].id,
                            pane: 71,
                            identity: identity(&machine.panes[0]).unwrap(),
                        },
                    }
                },
                if whole && case != "tab-unknown" {
                    ReplyOk::TabSleep(tty7_core::daemon::control::SleepTabReply {
                        accepted: if accepted && !cold {
                            vec![71, 72]
                        } else {
                            vec![71]
                        },
                        already_stopped: if cold { vec![72] } else { vec![] },
                        failed: (!accepted).then(|| tty7_core::daemon::control::SleepTabFailure {
                            pane: 72,
                            error: tty7_core::daemon::control::WireError::new(
                                tty7_core::daemon::control::WireErrorKind::Other,
                                "fixture failure",
                            ),
                        }),
                    })
                } else if accepted {
                    ReplyOk::Unit
                } else {
                    ReplyOk::Pong
                },
                false,
            ),
            (
                ControlRequest::MachineGet,
                ReplyOk::MachineTree(Box::new(machine.clone())),
                false,
            ),
            (
                ControlRequest::WorkspaceReleaseProof { proof },
                ReplyOk::Unit,
                false,
            ),
        ];
        let (client, server) = super::super::test_control::control_fixture(
            exchanges,
            Arc::new(AtomicBool::new(true)),
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        );
        let client = Arc::new(client);
        vcx.update(|_, cx| {
            super::super::local_link::LocalLink::install_client_for_test(cx, client.clone())
        });
        app.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        let button = vcx.debug_bounds(selector).expect("rendered Sleep button");
        vcx.simulate_mouse_down(
            button.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        vcx.simulate_mouse_up(
            button.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            vcx.run_until_parked();
            if app.read_with(&vcx, |app, _| {
                app.sleep_ui.as_ref().is_some_and(|s| !s.busy)
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Sleep reply did not reach the UI"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        app.update_in(&mut vcx, |app, window, cx| {
            assert_eq!(app.sleep_ui.as_ref().unwrap().accepted, accepted);
            assert_eq!(
                app.sleep_ui
                    .as_ref()
                    .unwrap()
                    .tab_reply
                    .as_ref()
                    .is_some_and(|r| r.failed.is_some()),
                case == "tab-partial"
            );
            assert_eq!(app.tabs[0].pane.leaves().len(), if whole { 2 } else { 1 });
            assert_eq!(
                app.tabs[0]
                    .pane
                    .leaves()
                    .iter()
                    .map(|p| p.entity_id())
                    .collect::<Vec<_>>(),
                original_entities
            );
            if cold {
                let PaneSlot::Connecting(pending) = app.tabs[0].pane.leaves()[1].clone() else {
                    panic!("cold neighbor was materialized");
                };
                assert!(matches!(pending.read(cx).state, PendingState::Stopped));
                assert!(pending.read(cx).resume.is_none());
                assert_eq!(
                    app.sleep_ui
                        .as_ref()
                        .unwrap()
                        .tab_reply
                        .as_ref()
                        .unwrap()
                        .already_stopped,
                    vec![72]
                );
            }
            if whole {
                let plan = app
                    .tab_sleep_plan(app.tabs[0].tree_id.get(), false, cx)
                    .unwrap();
                app.sleep_entire_tab(&plan, window, cx);
            }
            assert_eq!(app.tabs[0].pane.leaves()[0].entity_id(), view.entity_id());
            assert!(!view.read(cx).terminal.child_exited());
            assert!(app.render_retained_resume(window, cx).is_none());
            let expected = app.live_sleep_target(&view, cx).unwrap();
            app.sleep_current_session(view.clone(), &expected, window, cx);
        });
        assert_eq!(
            observed.load(Ordering::SeqCst),
            2,
            "duplicate stop was sent"
        );
        let (replacement, replacement_server) = super::super::test_control::control_fixture(
            vec![],
            Arc::new(AtomicBool::new(true)),
            |_| {},
        );
        let replacement = Arc::new(replacement);
        app.update(&mut vcx, |app, cx| {
            let request = app.sleep_ui.as_ref().unwrap();
            assert!(Tty7App::sleep_connection_current(request, cx));
            super::super::local_link::LocalLink::install_client_for_test(cx, replacement.clone());
            assert!(
                !Tty7App::sleep_connection_current(request, cx),
                "a same-instance replacement connection inherited an old pending action"
            );
            super::super::local_link::LocalLink::install_client_for_test(cx, client.clone());
        });
        drop(replacement);
        replacement_server.join().unwrap();
        let refresh = vcx.debug_bounds(selector).expect("explicit refresh button");
        vcx.simulate_mouse_down(
            refresh.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        vcx.simulate_mouse_up(
            refresh.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        loop {
            vcx.run_until_parked();
            if app.read_with(&vcx, |app, _| app.sleep_ui.is_none()) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fresh live facts did not rearm explicit action"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        app.update(&mut vcx, |app, cx| {
            assert_eq!(
                app.tabs[0]
                    .pane
                    .leaves()
                    .iter()
                    .map(|p| p.entity_id())
                    .collect::<Vec<_>>(),
                original_entities
            );
            app.retire_workspace_use(cx);
        });
        loop {
            vcx.run_until_parked();
            if observed.load(Ordering::SeqCst) == 4 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "tracked ownership was not retired"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        vcx.run_until_parked();
        vcx.update(|_, cx| super::super::local_link::LocalLink::invalidate(cx));
        drop(client);
        server.join().unwrap();
        drop(view);
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn retained_resume_requires_target_exit_and_preserves_output(cx: &mut gpui::TestAppContext) {
        use crate::daemon::protocol::DaemonMsg;
        use crate::ui::pane::Pane;
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 1);
        let (view, mut stream, machine) = app.update_in(&mut vcx, |app, window, cx| {
            let (view, stream) =
                crate::terminal::view::quiet_test_owned_pane(71, app.workspace, window, cx);
            let mut machine = stopped_machine();
            machine.workspaces[0].id = app.workspace;
            machine.workspaces[0].tabs[0].root.split_leaf(
                71,
                1,
                tty7_core::core::machine::Axis::Horizontal,
                0.35,
                false,
            );
            machine.panes.push(PaneRecord {
                live: true,
                ..PaneRecord::new(1)
            });
            app.tabs[0].tree_id.set(machine.workspaces[0].tabs[0].id);
            app.tabs[0].pane = Pane::split_node(
                gpui::Axis::Horizontal,
                0.35,
                Pane::leaf(PaneSlot::Ready(view.clone())),
                std::mem::replace(&mut app.tabs[0].pane, Pane::Empty),
            );
            app.maximized = Some(view.clone());
            window.focus(&view.read(cx).focus_handle.clone(), cx);
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![WindowView {
                        id: app.workspace,
                        ..WindowView::default()
                    }],
                    active: Some(app.workspace),
                },
            );
            super::super::machine_mirror::MachineMirrors::install(
                cx,
                HostId::LOCAL,
                machine.clone(),
            );
            cx.notify();
            (view, stream, machine)
        });
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("retained-pane-resume").is_none(),
            "target stop alone cannot replace a live view"
        );
        DaemonMsg::Output(b"retained-output\r\n".to_vec())
            .encode(&mut stream)
            .unwrap();
        DaemonMsg::Exited { code: Some(0) }
            .encode(&mut stream)
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !view.read_with(&vcx, |view, _| view.terminal.child_exited()) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        app.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        app.update_in(&mut vcx, |app, window, cx| {
            app.maximized = Some(app.tabs[0].pane.leaves()[1].terminal().unwrap().clone());
            assert!(
                app.render_retained_resume(window, cx).is_none(),
                "hidden focused pane must not offer Resume over the maximized live pane"
            );
            app.maximized = Some(view.clone());
            let expected = app.retained_candidate(&view, cx).unwrap();
            let mut changed = machine.clone();
            changed.panes[0]
                .recovery_binding
                .as_mut()
                .unwrap()
                .session_id = "01900000-0000-7000-8000-000000000002".into();
            super::super::machine_mirror::MachineMirrors::install(cx, HostId::LOCAL, changed);
            app.resume_retained(view.clone(), &expected, window, cx);
            assert_eq!(
                app.tabs[0].pane.leaves()[0].entity_id(),
                view.entity_id(),
                "stale button replaced the view"
            );
            let mut unconfirmed = machine.clone();
            unconfirmed.panes[0].exit = None;
            super::super::machine_mirror::MachineMirrors::install(cx, HostId::LOCAL, unconfirmed);
            assert!(
                app.retained_candidate(&view, cx).is_none(),
                "local exit cannot manufacture target confirmation"
            );
            super::super::machine_mirror::MachineMirrors::install(
                cx,
                HostId::LOCAL,
                machine.clone(),
            );
            cx.notify();
        });
        vcx.run_until_parked();
        let button = vcx
            .debug_bounds("retained-pane-resume")
            .expect("exited retained terminal offers Resume");
        vcx.simulate_mouse_down(
            button.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        vcx.simulate_mouse_up(
            button.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        app.read_with(&vcx, |app, cx| {
            assert_eq!(app.tabs.len(), 1);
            assert_eq!(app.tabs[0].tree_id.get(), machine.workspaces[0].tabs[0].id);
            let leaves = app.tabs[0].pane.leaves();
            assert_eq!(leaves.len(), 2);
            assert_eq!(leaves[1].terminal().unwrap().read(cx).pane_id, 1);
            let PaneSlot::Connecting(pending) = &leaves[0] else {
                panic!("expected recovery controller");
            };
            assert_eq!(pending.read(cx).retained_view.as_ref(), Some(&view));
            assert!(
                pending
                    .read(cx)
                    .resume
                    .as_ref()
                    .is_some_and(|state| !state.busy),
                "missing control must preserve an explicit unknown/refresh state"
            );
            assert_eq!(app.maximized.as_ref(), Some(&view));
            let Pane::Split { ratio, .. } = &app.tabs[0].pane else {
                panic!("split lost");
            };
            assert_eq!(ratio.get(), 0.35);
            let terminal = view.read(cx).terminal.term.lock();
            let text: String = (0..15)
                .map(|col| {
                    terminal.grid()[alacritty_terminal::index::Line(0)]
                        [alacritty_terminal::index::Column(col)]
                    .c
                })
                .collect();
            assert_eq!(text, "retained-output");
        });
        assert!(
            vcx.debug_bounds("stopped-pane-refresh").is_some(),
            "maximized retained output must render the recovery controller"
        );
    }

    #[test]
    fn stopped_resume_keeps_slot_identity_across_ratio_changes() {
        use tty7_core::core::machine::Axis;
        let mut machine = stopped_machine();
        machine.workspaces[0].tabs[0]
            .root
            .split_leaf(71, 72, Axis::Horizontal, 0.3, false);
        let ws = machine.workspaces[0].id;
        let target = ResumeTarget::from_machine(ws, HostId::LOCAL, ws, 71, &machine).unwrap();
        let mut resized = machine.clone();
        let PaneNode::Split { ratio, .. } = &mut resized.workspaces[0].tabs[0].root else {
            unreachable!()
        };
        *ratio = 0.7;
        assert!(
            target.reconcile(&resized).is_some(),
            "resizing invalidated the retained slot"
        );
        let updated = ResumeTarget::from_machine(ws, HostId::LOCAL, ws, 71, &resized).unwrap();
        assert!(
            target.same_slot(&updated),
            "preflight treated geometry as identity"
        );
        for case in ["client", "host", "workspace", "tab", "pane", "session"] {
            let mut foreign = updated.clone();
            match case {
                "client" => foreign.client_workspace = WorkspaceId::new(),
                "host" => foreign.host = HostId::from_connection_key("foreign-resize-target"),
                "workspace" => foreign.workspace = WorkspaceId::new(),
                "tab" => foreign.tab = TabId::new(),
                "pane" => foreign.pane = 73,
                "session" => foreign.identity.id = "01900000-0000-7000-8000-000000000002".into(),
                _ => unreachable!(),
            }
            assert!(!target.same_slot(&foreign), "{case}");
        }
        let mut snapshot = machine.clone();
        snapshot.workspaces[0].tabs[0].root.replace_leaf(71, 99);
        snapshot.panes[0].id = 99;
        snapshot.panes[0].exit = None;
        snapshot.panes[0].live = true;
        let mut current = snapshot.clone();
        let PaneNode::Split { ratio, .. } = &mut current.workspaces[0].tabs[0].root else {
            unreachable!()
        };
        *ratio = 0.8;
        current.workspaces[0].tabs[0].name = Some("new title".into());
        let (_, selected) = target
            .reconcile_current(&snapshot, &current)
            .expect("resized successor");
        assert_eq!(
            selected, current.workspaces[0].tabs[0],
            "stale geometry won"
        );
        for case in ["axis", "order", "neighbor", "binding"] {
            let mut changed = current.clone();
            let PaneNode::Split { axis, a, b, .. } = &mut changed.workspaces[0].tabs[0].root else {
                unreachable!()
            };
            match case {
                "axis" => *axis = Axis::Vertical,
                "order" => std::mem::swap(a, b),
                "neighbor" => **b = PaneNode::Leaf { pane: 73 },
                "binding" => changed.panes[0].recovery_binding = None,
                _ => unreachable!(),
            }
            assert!(
                target.reconcile_current(&snapshot, &changed).is_none(),
                "{case}"
            );
        }
    }

    #[test]
    fn stopped_resume_projection_rejects_ambiguous_or_changed_target() {
        let machine = stopped_machine();
        let ws = machine.workspaces[0].id;
        let host = tty7_core::host::HostId::LOCAL;
        let target = ResumeTarget::from_machine(ws, host, ws, 71, &machine).unwrap();
        assert!(accepted_resume_reply(&target, ReplyOk::Unit).is_none());
        assert!(
            accepted_resume_reply(
                &target,
                ReplyOk::StoppedSessionResumed {
                    workspace: ws,
                    tab: target.tab,
                    predecessor: 71,
                    pane: 71,
                }
            )
            .is_none()
        );
        assert_eq!(target.reconcile(&machine).unwrap().0.id, 71);
        let mut resumed = machine.clone();
        resumed.workspaces[0].tabs[0].root.replace_leaf(71, 99);
        resumed.panes[0].id = 99;
        resumed.panes[0].exit = None;
        resumed.panes[0].live = true;
        resumed.panes[0].resume_pending = true;
        assert_eq!(target.reconcile(&resumed).unwrap().0.id, 99);
        let mut current = resumed.clone();
        current.panes[0].title = "newer target facts".into();
        current.panes[0].exit = Some(tty7_core::core::machine::PaneExit { code: Some(23) });
        current.panes[0].live = false;
        current.panes[0].resume_pending = false;
        let (fresh, _) = target.reconcile_current(&resumed, &current).unwrap();
        assert_eq!(
            fresh, current.panes[0],
            "snapshot overwrote a newer target exit/title"
        );
        for case in ["binding", "cwd", "duplicate", "layout", "boundary"] {
            let mut changed = resumed.clone();
            match case {
                "binding" => changed.panes[0].recovery_binding = None,
                "cwd" => {
                    changed.panes[0].recovery_binding.as_mut().unwrap().cwd = "/foreign".into()
                }
                "duplicate" => changed.workspaces[0].tabs.push(Tab::leaf(99)),
                "layout" => {
                    changed.workspaces[0].tabs[0].root.split_leaf(
                        99,
                        100,
                        tty7_core::core::machine::Axis::Horizontal,
                        0.5,
                        false,
                    );
                }
                "boundary" => changed.workspaces[0].id = WorkspaceId::new(),
                _ => unreachable!(),
            }
            assert!(target.reconcile(&changed).is_none(), "{case}");
        }
        let mut duplicate = machine.clone();
        duplicate.workspaces[0].tabs.push(Tab::leaf(71));
        assert!(ResumeTarget::from_machine(ws, host, ws, 71, &duplicate).is_none());
        let mut unknown = machine.clone();
        unknown.panes[0].exit = None;
        assert!(ResumeTarget::from_machine(ws, host, ws, 71, &unknown).is_none());
    }

    #[gpui::test]
    fn stopped_resume_button_holds_original_slot(cx: &mut gpui::TestAppContext) {
        use crate::ui::pane::PaneSlot;
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 3);
        let (pending, tree) = app.update_in(&mut vcx, |app, window, cx| {
            let machine = stopped_machine();
            let tree = machine.workspaces[0].tabs[0].clone();
            let remote = RemoteTarget::direct("fixture", "unconnected-resume.test", 22);
            let host = remote.host_id();
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![WindowView {
                        id: app.workspace,
                        ..WindowView::on_remote(RemoteRef::new(remote, machine.workspaces[0].id))
                    }],
                    active: Some(app.workspace),
                },
            );
            super::super::machine_mirror::MachineMirrors::install(cx, host, machine.clone());
            let session =
                super::super::tree_sync::session_from_tree(&machine.workspaces[0], &machine.panes);
            let pane = super::super::app::session_to_pane(
                None,
                app.workspace,
                &session.tabs[0].pane,
                None,
                14.,
                crate::terminal::RestorePolicy::AttachOnly,
                window,
                cx,
            )
            .unwrap();
            let PaneSlot::Connecting(pending) = pane.leaves()[0].clone() else {
                panic!("stopped placeholder")
            };
            app.tabs[0].pane = pane;
            app.tabs[0].tree_id.set(tree.id);
            app.active = 0;
            cx.notify();
            (pending, tree)
        });
        vcx.run_until_parked();
        let bounds = vcx
            .debug_bounds("stopped-pane-resume")
            .expect("Resume button rendered");
        vcx.simulate_mouse_down(
            bounds.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        vcx.simulate_mouse_up(
            bounds.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("stopped-pane-refresh").is_some(),
            "missing control must offer reconciliation, not Local fallback"
        );
        app.update_in(&mut vcx, |app, window, cx| {
            let request = pending.read(cx).resume.as_ref().unwrap().request.clone();
            assert!(app.resume_current(&pending, &request, cx));
            let mut stale = request.clone();
            stale.generation = stale.generation.wrapping_add(1);
            assert!(!app.resume_current(&pending, &stale, cx));
            app.active = 1;
            assert!(
                app.resume_current(&pending, &request, cx),
                "selection is not recovery identity"
            );
            app.active = 0;
            let mut replacement = tree.clone();
            replacement.root.replace_leaf(71, 99);
            assert!(app.apply_layout_delta(
                &tty7_core::core::machine::LayoutDelta::TabRestructured {
                    tab: replacement,
                    pane: None
                },
                window,
                cx
            ));
            assert_eq!(
                app.tabs[0].pane.leaves()[0].entity_id(),
                pending.entity_id()
            );
            assert_eq!(pending.read(cx).spawn.restore_pane, Some(71));
            assert_eq!(app.tabs.len(), 3);
            assert!(app.hold_resume_tab(tree.id, cx));
            let old_tab = app.tabs[0].tree_id.replace(TabId::new());
            assert!(!app.resume_current(&pending, &request, cx));
            app.tabs[0].tree_id.set(old_tab);
            // Passive same-workspace hydration cannot discard the placeholder
            // and ordinary-attach a successor before reconciliation.
            let session = super::super::tree_sync::session_from_tree(
                &Workspace {
                    id: request.target.workspace,
                    tabs: vec![tree.clone()],
                    ..Workspace::default()
                },
                &stopped_machine().panes,
            );
            app.adopt_workspace(app.workspace, session, window, cx);
            assert_eq!(
                app.tabs[0].pane.leaves()[0].entity_id(),
                pending.entity_id()
            );
            assert_eq!(app.tabs.len(), 3);
            pending.update(cx, |p, cx| {
                p.resume.as_mut().unwrap().busy = true;
                cx.notify();
            });
        });
        vcx.run_until_parked();
        let generation =
            pending.read_with(&vcx, |p, _| p.resume.as_ref().unwrap().request.generation);
        let bounds = vcx.debug_bounds("stopped-pane-refresh").unwrap();
        vcx.simulate_mouse_down(
            bounds.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        vcx.simulate_mouse_up(
            bounds.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        vcx.run_until_parked();
        pending.read_with(&vcx, |p, _| {
            assert_eq!(
                p.resume.as_ref().unwrap().request.generation,
                generation,
                "busy button submitted twice"
            )
        });
    }

    #[gpui::test]
    fn recovery_badge_never_reads_local_mirror_for_remote_or_unknown_workspace(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_app, _vcx) = crate::ui::app::test_window::harness(cx);
        cx.update(|cx| {
            let ws = WorkspaceId::new();
            let recovery = AgentRecoveryBinding {
                agent: crate::core::cli_agent::CLIAgent::Codex,
                session_id: "local-only".into(),
                cwd: "/local".into(),
                launch_argv: None,
            };
            super::super::machine_mirror::MachineMirrors::install(
                cx,
                tty7_core::host::HostId::LOCAL,
                Machine {
                    workspaces: vec![Workspace {
                        id: ws,
                        tabs: vec![Tab::leaf(1)],
                        ..Workspace::default()
                    }],
                    panes: vec![PaneRecord {
                        recovery_binding: Some(recovery),
                        live: true,
                        ..PaneRecord::new(1)
                    }],
                },
            );
            WorkspaceStore::install_for_test(cx, WindowViews::default());
            assert!(
                for_workspace(cx, ws).is_empty(),
                "unknown mapping must not default to Local"
            );
            let local = WindowView {
                id: ws,
                ..WindowView::default()
            };
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![local],
                    active: None,
                },
            );
            let bound = for_workspace(cx, ws);
            assert_eq!(bound.len(), 1);
            assert!(label(bound.values().next().unwrap()).contains(t(L10nKey::SessionAgentExited)));
            let remote = WindowView {
                id: ws,
                ..WindowView::on_remote(RemoteRef::new(
                    RemoteTarget::direct("dev", "recovery.test", 22),
                    ws,
                ))
            };
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![remote],
                    active: None,
                },
            );
            assert!(
                for_workspace(cx, ws).is_empty(),
                "remote source absence is not local recovery"
            );
        });
    }
}
