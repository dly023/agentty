use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, MouseButton, MouseDownEvent, Subscription,
    Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};

use agentty_core::core::machine::TabId;

#[derive(Default)]
pub(crate) struct TabSwitchState {
    hold: Option<TabSwitchHold>,
}

struct TabSwitchHold {
    origin: TabId,
    snapshot: Vec<TabId>,
    cursor: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabSwitchFinish {
    Commit(TabId),
    Restore(TabId),
}

#[derive(Clone, Copy)]
pub(crate) enum TabSwitchFinishKind {
    Escape,
    Deactivation,
}

impl TabSwitchState {
    pub(crate) fn step(&mut self, active: TabId, mru: &[TabId], forward: bool) -> Option<TabId> {
        if self.hold.is_none() {
            let mut snapshot: Vec<_> = mru.iter().copied().filter(|id| *id != active).collect();
            snapshot.insert(0, active);
            snapshot.dedup();
            if snapshot.len() < 2 {
                return None;
            }
            self.hold = Some(TabSwitchHold {
                origin: active,
                snapshot,
                cursor: 0,
            });
        }
        let hold = self.hold.as_mut()?;
        hold.cursor = if forward {
            (hold.cursor + 1) % hold.snapshot.len()
        } else {
            (hold.cursor + hold.snapshot.len() - 1) % hold.snapshot.len()
        };
        Some(hold.snapshot[hold.cursor])
    }

    pub(crate) fn release(&mut self, live: &[TabId]) -> Option<TabSwitchFinish> {
        let hold = self.hold.take()?;
        let candidate = hold.snapshot[hold.cursor];
        live.contains(&candidate)
            .then_some(TabSwitchFinish::Commit(candidate))
            .or_else(|| live.first().copied().map(TabSwitchFinish::Commit))
    }

    pub(crate) fn cancel(
        &mut self,
        live: &[TabId],
        _kind: TabSwitchFinishKind,
    ) -> Option<TabSwitchFinish> {
        let hold = self.hold.take()?;
        live.contains(&hold.origin)
            .then_some(TabSwitchFinish::Restore(hold.origin))
    }

    pub(crate) fn is_holding(&self) -> bool {
        self.hold.is_some()
    }
}
use agentty_core::core::session::{RemoteTarget, WorkspaceId};

#[cfg(test)]
mod tab_switch_hold_tests {
    use super::*;

    fn ids() -> [TabId; 3] {
        [TabId::new(), TabId::new(), TabId::new()]
    }

    #[test]
    fn tab_switch_hold_uses_a_fixed_mru_snapshot() {
        let [a, b, c] = ids();
        let mut state = TabSwitchState::default();
        assert_eq!(state.step(a, &[a, b, c], true), Some(b));
        assert_eq!(state.step(b, &[c, b, a], true), Some(c));
        assert_eq!(state.step(c, &[c, b, a], false), Some(b));
    }

    #[test]
    fn tab_switch_release_commits_exactly_once() {
        let [a, b, _] = ids();
        let mut state = TabSwitchState::default();
        state.step(a, &[a, b], true);
        assert_eq!(state.release(&[a, b]), Some(TabSwitchFinish::Commit(b)));
        assert_eq!(state.release(&[a, b]), None);
    }

    #[test]
    fn tab_switch_escape_and_deactivation_restore_the_origin() {
        let [a, b, _] = ids();
        for finish in [
            TabSwitchFinishKind::Escape,
            TabSwitchFinishKind::Deactivation,
        ] {
            let mut state = TabSwitchState::default();
            state.step(a, &[a, b], true);
            assert_eq!(
                state.cancel(&[a, b], finish),
                Some(TabSwitchFinish::Restore(a))
            );
        }
    }

    #[test]
    fn tab_switch_revalidates_removed_origin_and_candidate() {
        let [a, b, c] = ids();
        let mut state = TabSwitchState::default();
        state.step(a, &[a, b, c], true);
        assert_eq!(state.release(&[c]), Some(TabSwitchFinish::Commit(c)));
        let mut state = TabSwitchState::default();
        state.step(a, &[a, b, c], true);
        assert_eq!(state.cancel(&[b, c], TabSwitchFinishKind::Escape), None);
    }

    #[test]
    fn tab_switch_single_tab_is_a_noop() {
        let [a, _, _] = ids();
        let mut state = TabSwitchState::default();
        assert_eq!(state.step(a, &[a], true), None);
        assert_eq!(state.release(&[a]), None);
    }
}

use crate::core::session::WorkspaceStore;
use crate::daemon::install::InstallPhase;
use crate::terminal::pane_liveness::Liveness;
use crate::ui::app::AgenttyApp;
use crate::ui::remote_connect::{self, HostChoice, RemoteWorkspaceRow, human_bytes};
use crate::ui::remote_workspace::ConnectFlow;

const CARD_W: f32 = 560.0;

const CARD_TOP: f32 = 120.0;

const BODY_MAX_H: f32 = 420.0;

const ROW_AVATAR: f32 = 20.0;

const ROW_H: f32 = 32.0;
const HOST_H: f32 = 34.0;

const GUTTER: f32 = 26.0;

const ICON: f32 = 16.0;

const KID_INDENT: f32 = 16.0;

const RAIL_X: f32 = ROW_PAD + GUTTER / 2.;

const ROW_PAD: f32 = 8.0;

const WHEN_W: f32 = 96.0;

const PROGRESS_H: f32 = 3.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Link {
    Local,
    Connected,
    Connecting,
    Failed,
    Offline,
}

struct Group {
    key: String,
    label: String,
    endpoint: String,
    target: Option<RemoteTarget>,
    link: Link,
    home: Option<PathBuf>,
    error: Option<String>,
    installing: Option<InstallPhase>,
    rows: Vec<Row>,
}

struct Row {
    id: WorkspaceId,
    name: String,
    path: String,
    when: String,
    live: Liveness,
    open: bool,
    current: bool,
    adopt: Option<Box<RemoteWorkspaceRow>>,
    remote_id: Option<WorkspaceId>,
}

pub(crate) struct HostSnapshot {
    pub target: RemoteTarget,
    pub rows: Vec<RemoteWorkspaceRow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialectFailureActions {
    None,
    RetryOnly,
    ReplaceOnly,
}

fn dialect_failure_actions(error: &str, has_target: bool) -> DialectFailureActions {
    if !has_target {
        DialectFailureActions::None
    } else if crate::daemon::control::is_dialect_refusal(error) {
        DialectFailureActions::ReplaceOnly
    } else {
        DialectFailureActions::RetryOnly
    }
}

pub(crate) struct Switcher {
    pub query: Entity<InputState>,
    collapsed: HashSet<String>,
    show_others: bool,
    renaming: Option<(WorkspaceId, Entity<InputState>)>,
    _subs: Vec<Subscription>,
}

impl Switcher {
    fn text(&self, cx: &App) -> String {
        self.query.read(cx).value().trim().to_lowercase()
    }
}

impl AgenttyApp {
    pub(crate) fn toggle_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.switcher.is_some() {
            self.close_switcher(window, cx);
        } else {
            self.open_switcher(window, cx);
        }
    }

    pub(crate) fn open_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        remote_connect::register(cx);
        remote_connect::sweep_wsl(cx);
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::core::i18n::current(
                cx,
                "switcher.search_placeholder",
            ))
        });
        query.update(cx, |state, cx| state.focus(window, cx));
        let subs = vec![cx.subscribe_in(
            &query,
            window,
            |_this, _input, ev: &InputEvent, _window, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            },
        )];
        self.switcher = Some(Switcher {
            query,
            collapsed: HashSet::new(),
            show_others: false,
            renaming: None,
            _subs: subs,
        });
        cx.notify();
    }

    pub(crate) fn close_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.switcher.take().is_some() {
            if matches!(self.connect, Some(ConnectFlow::Failed { .. })) {
                self.connect = None;
            }
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    fn switcher_groups(&self, cx: &mut Context<Self>) -> Vec<Group> {
        let now = crate::ui::home::now_secs();
        let current = self.workspace;
        crate::terminal::pane_liveness::sweep(cx);

        let mut groups: Vec<Group> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        {
            let app: &App = cx;
            let store = WorkspaceStore::all(app);
            for w in &store.windows {
                let (key, label, target) = match w.remote_ref().as_ref() {
                    None => (String::new(), "This Computer".to_string(), None),
                    Some(r) => {
                        let key = r.target.to_string();
                        (key.clone(), key, Some(r.target.clone()))
                    }
                };
                let slot = *index.entry(key.clone()).or_insert_with(|| {
                    groups.push(Group {
                        key,
                        label,
                        endpoint: String::new(),
                        target,
                        link: Link::Offline,
                        home: None,
                        error: None,
                        installing: None,
                        rows: Vec::new(),
                    });
                    groups.len() - 1
                });
                groups[slot].rows.push(Row {
                    id: w.workspace,
                    name: crate::ui::machine_mirror::display_name(app, w)
                        .unwrap_or_else(|| "Untitled".to_string()),
                    path: crate::ui::machine_mirror::subject_path(app, w)
                        .map(|p| crate::ui::home::display_path(std::path::Path::new(&p)))
                        .unwrap_or_default(),
                    when: crate::ui::home::relative_time(now, w.last_active),
                    live: crate::terminal::pane_liveness::liveness_of(app, w),
                    open: w.open,
                    current: w.workspace == current,
                    adopt: None,
                    remote_id: w.remote_workspace,
                });
            }
        }

        for target in self.pending_machines() {
            let key = target.to_string();
            if index.contains_key(&key) {
                continue;
            }
            index.insert(key.clone(), groups.len());
            groups.push(Group {
                label: key.clone(),
                key,
                endpoint: String::new(),
                target: Some(target),
                link: Link::Offline,
                home: None,
                error: None,
                installing: None,
                rows: Vec::new(),
            });
        }

        if !index.contains_key("") {
            groups.insert(
                0,
                Group {
                    key: String::new(),
                    label: "This Computer".to_string(),
                    endpoint: String::new(),
                    target: None,
                    link: Link::Offline,
                    home: None,
                    error: None,
                    installing: None,
                    rows: Vec::new(),
                },
            );
        }

        for group in &mut groups {
            group.rows.sort_by(|a, b| {
                b.current
                    .cmp(&a.current)
                    .then_with(|| b.open.cmp(&a.open))
                    .then_with(|| a.name.cmp(&b.name))
            });
        }
        groups.sort_by(|a, b| a.key.is_empty().cmp(&b.key.is_empty()).reverse());

        let configured = remote_connect::available_hosts(cx);
        for group in &mut groups {
            let Some(target) = group.target.clone() else {
                group.link = Link::Local;
                continue;
            };
            if let Some(known) = configured.iter().find(|h| h.target == target) {
                group.label = known.label.clone();
                if known.detail != known.label {
                    group.endpoint = known.detail.clone();
                }
            }
            group.link = self.link_state(&target, cx);
            if let Some(ConnectFlow::Failed { choice, error }) = &self.connect
                && choice.target == target
            {
                group.error = Some(error.clone());
            }
            let id = target.host_id();
            let reported = remote_connect::install_progress_for(id);
            if group.link == Link::Connecting
                || group.error.is_some()
                || matches!(reported, Some(InstallPhase::Restarting))
            {
                group.installing = reported;
            }
            group.home = remote_connect::HostLinks::home(cx, id);
            if let Some(snapshot) = self.host_snapshots.get(&id) {
                group.merge(&snapshot.rows, now);
            }
        }
        groups
    }

    fn pending_machines(&self) -> Vec<RemoteTarget> {
        let mut out: Vec<RemoteTarget> = self
            .host_snapshots
            .values()
            .map(|s| s.target.clone())
            .collect();
        if let Some(choice) = self.connect.as_ref().and_then(ConnectFlow::choice) {
            out.push(choice.target.clone());
        }
        out
    }

    fn link_state(&self, target: &RemoteTarget, cx: &mut Context<Self>) -> Link {
        match &self.connect {
            Some(ConnectFlow::Connecting { choice, .. }) if &choice.target == target => {
                return Link::Connecting;
            }
            Some(ConnectFlow::Failed { choice, .. }) if &choice.target == target => {
                return Link::Failed;
            }
            _ => {}
        }
        match crate::ui::remote_workspace::RemoteLinks::status_for_host(cx, target.host_id()) {
            crate::ui::remote_workspace::RemoteStatus::Attached => Link::Connected,
            crate::ui::remote_workspace::RemoteStatus::Connecting
            | crate::ui::remote_workspace::RemoteStatus::Reconnecting { .. } => Link::Connecting,
            crate::ui::remote_workspace::RemoteStatus::Failed(_) => Link::Failed,
            crate::ui::remote_workspace::RemoteStatus::Disconnected
            | crate::ui::remote_workspace::RemoteStatus::Preempted { .. } => Link::Offline,
        }
    }

    fn other_hosts(&self, groups: &[Group], cx: &App) -> Vec<HostChoice> {
        let known: HashSet<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        remote_connect::available_hosts(cx)
            .into_iter()
            .filter(|h| !known.contains(h.target.to_string().as_str()))
            .collect()
    }

    fn switcher_toggle_host(&mut self, group: &GroupRef, cx: &mut Context<Self>) {
        if group.link == Link::Offline
            && let Some(target) = group.target.clone()
        {
            let choice = HostChoice {
                target,
                label: group.label.clone(),
                detail: String::new(),
            };
            self.connect_to_host(choice, cx);
            if let Some(sw) = self.switcher.as_mut() {
                sw.collapsed.remove(&group.key);
            }
            return;
        }
        if let Some(sw) = self.switcher.as_mut() {
            if !sw.collapsed.remove(&group.key) {
                sw.collapsed.insert(group.key.clone());
            }
        }
        cx.notify();
    }

    fn switcher_open(
        &mut self,
        row: RowRef,
        new_window: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_switcher(window, cx);
        match row.adopt {
            Some((target, remote)) => self.open_remote_workspace(target, *remote, window, cx),
            None if new_window => crate::ui::windows::open(cx, Some(row.id)),
            None => self.reveal_workspace(row.id, window, cx),
        }
    }

    fn switcher_rename(&mut self, id: WorkspaceId, window: &mut Window, cx: &mut Context<Self>) {
        let current = crate::ui::machine_mirror::display_name_for(cx, id).unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        input.update(cx, |state, cx| state.focus(window, cx));
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, _input, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    this.switcher_commit_rename(window, cx)
                }
                _ => {}
            },
        );
        if let Some(sw) = self.switcher.as_mut() {
            sw.renaming = Some((id, input));
            sw._subs.push(sub);
        }
        cx.notify();
    }

    fn switcher_commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, input)) = self.switcher.as_mut().and_then(|sw| sw.renaming.take()) else {
            return;
        };
        let value = input.read(cx).value().trim().to_string();
        crate::ui::tree_sync::rename_workspace(cx, id, (!value.is_empty()).then_some(value));
        crate::ui::windows::refresh_menu(cx);
        if id == self.workspace {
            self.sync_window_title(window, cx);
        }
        if let Some(sw) = self.switcher.as_ref() {
            sw.query.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    fn switcher_disconnect(&mut self, target: &RemoteTarget, cx: &mut Context<Self>) {
        crate::ui::remote_workspace::RemoteLinks::disconnect(cx, target.host_id());
        if self
            .connect
            .as_ref()
            .and_then(ConnectFlow::choice)
            .is_some_and(|c| &c.target == target)
        {
            self.connect = None;
        }
        cx.notify();
    }

    fn switcher_new(&mut self, group: &GroupRef, window: &mut Window, cx: &mut Context<Self>) {
        self.close_switcher(window, cx);
        match (group.target.clone(), group.home.clone()) {
            (Some(target), Some(home)) => self.create_remote_workspace(target, home, window, cx),
            (Some(_), None) => {}
            (None, _) => crate::ui::windows::open(cx, None),
        }
    }

    pub(crate) fn render_switcher(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.switcher.as_ref()?;
        let groups = self.switcher_groups(cx);
        let others = self.other_hosts(&groups, cx);
        let query = self
            .switcher
            .as_ref()
            .map(|sw| sw.text(cx))
            .unwrap_or_default();

        let theme = cx.theme();
        let (border, card_bg) = (theme.border, theme.popover);
        let scrim = crate::ui::presets::scrim_fill(cx);

        let mut body = v_flex().gap(px(6.));
        let mut shown = 0usize;
        for group in &groups {
            let Some(rendered) = self.render_group(group, &query, cx) else {
                continue;
            };
            shown += 1;
            body = body.child(rendered);
        }
        if let Some(band) = self.render_other_hosts(&others, &query, cx) {
            shown += 1;
            body = body.child(band);
        }
        if shown == 0 {
            body = body.child(
                div()
                    .px(px(ROW_PAD))
                    .py(px(14.))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::core::i18n::current(cx, "switcher.no_match")),
            );
        }

        let card = v_flex()
            .w(px(CARD_W))
            .bg(card_bg)
            .border_1()
            .border_color(border)
            .rounded(px(10.))
            .shadow_xl()
            .overflow_hidden()
            .child(self.render_search(cx))
            .child(
                div()
                    .id("switcher-body")
                    .max_h(px(BODY_MAX_H))
                    .overflow_y_scroll()
                    .p(px(6.))
                    .child(body),
            )
            .child(self.render_footer(cx));

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(CARD_TOP))
                .bg(scrim)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                    if ev.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.close_switcher(window, cx);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.close_switcher(window, cx)
                    }),
                )
                .child(div().occlude().child(card))
                .into_any_element(),
        )
    }

    fn render_search(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        h_flex()
            .items_center()
            .gap(px(8.))
            .pl(px(6. + ROW_PAD))
            .pr(px(12.))
            .h(px(42.))
            .border_b_1()
            .border_color(border)
            .child(glyph_col(
                GUTTER,
                Icon::new(IconName::Search).size(px(ICON)).text_color(muted),
            ))
            .children(
                self.switcher
                    .as_ref()
                    .map(|sw| Input::new(&sw.query).appearance(false).small().pl_0()),
            )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (muted, dim, border) = (
            theme.muted_foreground,
            theme.muted_foreground.opacity(0.7),
            theme.border,
        );
        let hover = hover_fill(cx);
        h_flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(border)
            .p(px(6.))
            .child(
                h_flex()
                    .id("switcher-add-host")
                    .items_center()
                    .gap(px(8.))
                    .h(px(ROW_H))
                    .px(px(ROW_PAD))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(move |r| r.bg(hover))
                    .text_sm()
                    .text_color(muted)
                    .child(glyph_col(
                        GUTTER,
                        Icon::new(IconName::Plus).size(px(ICON)).text_color(dim),
                    ))
                    .child(crate::core::i18n::current(cx, "switcher.add_ssh_host"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.close_switcher(window, cx);
                        this.open_settings_section(
                            crate::ui::settings::SettingsSection::Ssh,
                            window,
                            cx,
                        );
                    })),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap(px(6.))
                    .pr(px(ROW_PAD))
                    .text_xs()
                    .text_color(dim)
                    .child(
                        div()
                            .px(px(5.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(border)
                            .child("⌘"),
                    )
                    .child(crate::core::i18n::current(cx, "switcher.new_window_hint")),
            )
    }

    fn render_group(
        &self,
        group: &Group,
        query: &str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let matched_host = group.label.to_lowercase().contains(query);
        let rows: Vec<&Row> = group
            .rows
            .iter()
            .filter(|r| {
                query.is_empty()
                    || matched_host
                    || r.name.to_lowercase().contains(query)
                    || r.path.to_lowercase().contains(query)
            })
            .collect();
        if !query.is_empty() && !matched_host && rows.is_empty() {
            return None;
        }

        let collapsed = self
            .switcher
            .as_ref()
            .map(|sw| sw.collapsed.contains(&group.key))
            .unwrap_or(false);
        let expanded = (!collapsed || !query.is_empty()) && group.link != Link::Offline;

        let mut block = v_flex().gap(px(1.));
        block = block.child(self.render_group_header(group, expanded, cx));
        if let Some(phase) = group.installing {
            block = block.child(self.render_install_progress(phase, cx));
        }
        if let Some(error) = group.error.as_ref().filter(|_| group.installing.is_none()) {
            let retry = GroupRef::of(group);
            let replace = retry.clone();
            let actions = dialect_failure_actions(error, group.target.is_some());
            let theme = cx.theme();
            block =
                block.child(
                    v_flex()
                        .gap(px(4.))
                        .ml(px(KID_INDENT))
                        .mr(px(4.))
                        .mb(px(2.))
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(theme.danger.opacity(0.35))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(error.clone()),
                        )
                        .when(actions == DialectFailureActions::RetryOnly, |card| {
                            card.child(
                                h_flex().gap(px(4.)).child(
                                    Button::new(gpui::SharedString::from(format!(
                                        "switcher-retry:{}",
                                        group.key
                                    )))
                                    .label(crate::core::i18n::current(cx, "common.retry"))
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        if let Some(target) = retry.target.clone() {
                                            this.connect_to_host(
                                                HostChoice {
                                                    target,
                                                    label: retry.label.clone(),
                                                    detail: String::new(),
                                                },
                                                cx,
                                            );
                                        }
                                    })),
                                ),
                            )
                        })
                        .when(actions == DialectFailureActions::ReplaceOnly, |card| {
                            card.child(
                                h_flex().gap(px(4.)).child(
                                    Button::new(gpui::SharedString::from(format!(
                                        "switcher-replace:{}",
                                        group.key
                                    )))
                                    .label(crate::core::i18n::current(cx, "common.replace_server"))
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(target) = replace.target.clone() {
                                            this.confirm_replace_remote_server(
                                                target,
                                                replace.label.clone(),
                                                window,
                                                cx,
                                            );
                                        }
                                    })),
                                ),
                            )
                        }),
                );
        }
        if expanded && !rows.is_empty() {
            let mut kids = v_flex().gap(px(1.));
            for row in rows {
                kids = kids.child(self.render_row(group, row, cx));
            }
            block = block.child(self.indent(group, kids, cx));
        }
        Some(block.into_any_element())
    }

    fn render_install_progress(
        &self,
        phase: InstallPhase,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let accent = theme.warning;
        let fraction = phase.fraction().unwrap_or(0.0);
        let caption = match phase {
            InstallPhase::Restarting => "Restarting agentty's server\u{2026}".to_string(),
            InstallPhase::Downloading { done, total } => match total {
                Some(total) => format!(
                    "Downloading agentty's server\u{2026} {} / {}",
                    human_bytes(done),
                    human_bytes(total)
                ),
                None => format!("Downloading agentty's server\u{2026} {}", human_bytes(done)),
            },
            InstallPhase::Uploading { done, total } => format!(
                "Copying agentty's server\u{2026} {} / {}",
                human_bytes(done),
                human_bytes(total)
            ),
        };

        v_flex()
            .gap(px(6.))
            .ml(px(KID_INDENT))
            .mr(px(4.))
            .mb(px(2.))
            .px(px(10.))
            .py(px(8.))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(caption),
            )
            .child(
                div()
                    .w_full()
                    .h(px(PROGRESS_H))
                    .rounded_full()
                    .bg(theme.border)
                    .child(
                        div()
                            .h_full()
                            .w(gpui::relative(fraction))
                            .rounded_full()
                            .bg(accent),
                    ),
            )
    }

    fn indent(&self, group: &Group, kids: impl IntoElement, cx: &mut Context<Self>) -> AnyElement {
        let rail = cx.theme().border;
        div()
            .relative()
            .child(div().pl(px(KID_INDENT)).child(kids))
            .when(group.target.is_some(), |wrap| {
                wrap.child(
                    div()
                        .absolute()
                        .left(px(RAIL_X))
                        .top(px(0.))
                        .bottom(px(ROW_H / 2.))
                        .w(px(1.))
                        .bg(rail),
                )
            })
            .into_any_element()
    }

    fn render_group_header(
        &self,
        group: &Group,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (fg, muted, dim) = (
            theme.foreground,
            theme.muted_foreground,
            theme.muted_foreground.opacity(0.75),
        );
        let hover = hover_fill(cx);
        let gref = GroupRef::of(group);
        let menu_ref = gref.clone();
        let ctx_ref = gref.clone();
        let app = cx.entity().downgrade();
        let app2 = app.clone();

        let glyph = match group.target {
            None => "icons/machine-local.svg",
            Some(_) => "icons/machine-remote.svg",
        };

        let (dot, word): (Option<gpui::Hsla>, Option<&'static str>) = match group.link {
            Link::Local => (None, None),
            Link::Connected => (Some(gpui::rgb(crate::ui::tab_strip::LIVE_DOT).into()), None),
            Link::Connecting if matches!(group.installing, Some(InstallPhase::Restarting)) => {
                (Some(theme.warning), Some("restarting…"))
            }
            Link::Connecting if group.installing.is_some() => {
                (Some(theme.warning), Some("installing…"))
            }
            Link::Connecting => (Some(theme.warning), Some("connecting…")),
            Link::Failed => (Some(theme.danger), Some("couldn't connect")),
            Link::Offline => (
                Some(gpui::rgb(crate::ui::tab_strip::UNKNOWN_DOT).into()),
                Some("not connected"),
            ),
        };
        let word_color = match group.link {
            Link::Connecting => theme.warning,
            Link::Failed => theme.danger,
            _ => muted,
        };

        h_flex()
            .id(gpui::SharedString::from(format!(
                "switcher-host:{}",
                group.key
            )))
            .items_center()
            .gap(px(8.))
            .h(px(HOST_H))
            .px(px(ROW_PAD))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(move |r| r.bg(hover))
            .child(glyph_col(
                GUTTER,
                Icon::empty()
                    .path(glyph)
                    .size(px(ICON))
                    .text_color(if group.link == Link::Local { muted } else { fg }),
            ))
            .child(
                div()
                    .flex_shrink_0()
                    .truncate()
                    .text_sm()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(fg)
                    .child(group.label.clone()),
            )
            .when(group.endpoint.is_empty(), |head| head.child(div().flex_1()))
            .when(!group.endpoint.is_empty(), |head| {
                head.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(dim)
                        .child(group.endpoint.clone()),
                )
            })
            .children(dot.map(|c| div().flex_shrink_0().size(px(6.)).rounded_full().bg(c)))
            .children(word.map(|w| {
                div()
                    .flex_shrink_0()
                    .ml(px(-2.))
                    .text_xs()
                    .text_color(word_color)
                    .child(w)
            }))
            .when(!group.rows.is_empty(), |head| {
                head.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(dim)
                        .child(format!("{}", group.rows.len())),
                )
            })
            .child(
                div()
                    .flex_shrink_0()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new(gpui::SharedString::from(format!(
                            "switcher-host-more:{}",
                            group.key
                        )))
                        .icon(IconName::Ellipsis)
                        .ghost()
                        .xsmall()
                        .dropdown_menu(move |menu, _window, _cx| {
                            group_menu(menu, &menu_ref, app.clone(), _cx)
                        }),
                    ),
            )
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(px(ICON))
                .text_color(dim),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.switcher_toggle_host(&gref, cx)
            }))
            .context_menu(move |menu, _window, _cx| group_menu(menu, &ctx_ref, app2.clone(), _cx))
    }

    fn render_row(&self, group: &Group, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        if let Some(sw) = self.switcher.as_ref()
            && let Some((id, input)) = sw.renaming.as_ref()
            && *id == row.id
        {
            return h_flex()
                .id(("switcher-rename", row.id.element_key() as usize))
                .items_center()
                .h(px(ROW_H))
                .px(px(ROW_PAD))
                .rounded(px(6.))
                .bg(hover_fill(cx))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(Input::new(input).appearance(false).xsmall())
                .into_any_element();
        }

        let theme = cx.theme();
        let (fg, muted, dim) = (
            theme.foreground,
            theme.muted_foreground,
            theme.muted_foreground.opacity(0.7),
        );
        let sf = rungs(cx);
        let hover = gpui::rgb(sf.hover);
        let rref = RowRef::of(group, row);
        let click_ref = rref.clone();
        let menu_ref = rref.clone();
        let ctx_ref = rref.clone();
        let app = cx.entity().downgrade();
        let app2 = app.clone();
        let key = row.id.element_key() as usize;

        let badge = if row.current {
            Some(("this window", true))
        } else if row.open {
            Some(("open", false))
        } else {
            None
        };

        h_flex()
            .id(("switcher-row", key))
            .group("switcher-row")
            .items_center()
            .gap(px(8.))
            .h(px(ROW_H))
            .px(px(ROW_PAD))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(move |r| r.bg(hover))
            .child(crate::ui::tab_strip::workspace_avatar(
                &row.name,
                row.live,
                row.current,
                ROW_AVATAR,
                cx,
            ))
            .child(
                div()
                    .flex_shrink_0()
                    .truncate()
                    .text_sm()
                    .when(row.current, |d| d.font_weight(gpui::FontWeight::MEDIUM))
                    .text_color(fg)
                    .child(row.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(dim)
                    .child(row.path.clone()),
            )
            .children(badge.map(|(label, here)| {
                div()
                    .flex_shrink_0()
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .text_xs()
                    .bg(gpui::rgb(sf.selected))
                    .text_color(if here { fg.opacity(0.85) } else { muted })
                    .child(label)
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(WHEN_W))
                    .truncate()
                    .text_right()
                    .text_xs()
                    .text_color(dim)
                    .child(row.when.clone()),
            )
            .child(
                div()
                    .invisible()
                    .flex_shrink_0()
                    .group_hover("switcher-row", |x| x.visible())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new(("switcher-row-more", key))
                            .icon(IconName::Ellipsis)
                            .ghost()
                            .xsmall()
                            .dropdown_menu(move |menu, _window, _cx| {
                                row_menu(menu, &menu_ref, app.clone(), _cx)
                            }),
                    ),
            )
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                this.switcher_open(click_ref.clone(), ev.modifiers().platform, window, cx)
            }))
            .context_menu(move |menu, _window, _cx| row_menu(menu, &ctx_ref, app2.clone(), _cx))
            .into_any_element()
    }

    fn render_other_hosts(
        &self,
        others: &[HostChoice],
        query: &str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if others.is_empty() {
            return None;
        }
        let hits: Vec<HostChoice> = match query.is_empty() {
            true => others.to_vec(),
            false => remote_connect::filter_hosts(others, query),
        };
        if hits.is_empty() {
            return None;
        }
        let expanded = self
            .switcher
            .as_ref()
            .map(|sw| sw.show_others)
            .unwrap_or(false)
            || !query.is_empty();

        let theme = cx.theme();
        let (muted, dim) = (theme.muted_foreground, theme.muted_foreground.opacity(0.7));
        let hover = hover_fill(cx);

        let mut block = v_flex().gap(px(1.)).child(
            h_flex()
                .id("switcher-others")
                .items_center()
                .gap(px(8.))
                .h(px(HOST_H))
                .px(px(ROW_PAD))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(move |r| r.bg(hover))
                .child(glyph_col(
                    GUTTER,
                    Icon::new(IconName::Globe).size(px(ICON)).text_color(dim),
                ))
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(crate::core::i18n::current(cx, "switcher.other_machines")),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_xs()
                        .text_color(dim)
                        .child(format!("{}", others.len())),
                )
                .child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(px(ICON))
                    .text_color(dim),
                )
                .on_click(cx.listener(|this, _, _window, cx| {
                    if let Some(sw) = this.switcher.as_mut() {
                        sw.show_others = !sw.show_others;
                    }
                    cx.notify();
                })),
        );

        if expanded {
            let mut kids = v_flex().gap(px(1.));
            for (i, host) in hits.iter().enumerate() {
                let choice = (*host).clone();
                kids = kids.child(
                    h_flex()
                        .id(("switcher-other", i))
                        .items_center()
                        .gap(px(8.))
                        .h(px(ROW_H))
                        .px(px(ROW_PAD))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(move |r| r.bg(hover))
                        .child(glyph_col(
                            ROW_AVATAR,
                            Icon::empty()
                                .path("icons/machine-remote.svg")
                                .size(px(ICON))
                                .text_color(dim),
                        ))
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .text_color(muted)
                                .child(host.label.clone()),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .flex_shrink_0()
                                .truncate()
                                .text_xs()
                                .text_color(dim)
                                .child(host.detail.clone()),
                        )
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.connect_to_host(choice.clone(), cx)
                        })),
                );
            }
            block = block.child(div().pl(px(KID_INDENT)).child(kids));
        }
        Some(block.into_any_element())
    }
}

impl Group {
    fn merge(&mut self, remote: &[RemoteWorkspaceRow], now: u64) {
        if self.target.is_none() {
            return;
        }
        let known: HashSet<WorkspaceId> = self.rows.iter().filter_map(|r| r.remote_id).collect();
        for r in remote {
            if known.contains(&r.id) {
                continue;
            }
            self.rows.push(Row {
                id: r.id,
                name: r.name.clone(),
                path: String::new(),
                when: crate::ui::home::relative_time(now, r.last_active),
                live: Liveness::Stopped,
                open: false,
                current: false,
                adopt: Some(Box::new(r.clone())),
                remote_id: Some(r.id),
            });
        }
    }
}

#[derive(Clone)]
struct GroupRef {
    key: String,
    label: String,
    target: Option<RemoteTarget>,
    home: Option<PathBuf>,
    link: Link,
}

impl GroupRef {
    fn of(g: &Group) -> Self {
        Self {
            key: g.key.clone(),
            label: g.label.clone(),
            target: g.target.clone(),
            home: g.home.clone(),
            link: g.link,
        }
    }
}

#[derive(Clone)]
struct RowRef {
    id: WorkspaceId,
    live: bool,
    adopt: Option<(RemoteTarget, Box<RemoteWorkspaceRow>)>,
}

impl RowRef {
    fn of(group: &Group, row: &Row) -> Self {
        Self {
            id: row.id,
            live: row.live == Liveness::Alive,
            adopt: match (&group.target, &row.adopt) {
                (Some(t), Some(r)) => Some((t.clone(), r.clone())),
                _ => None,
            },
        }
    }
}

fn group_menu(
    menu: gpui_component::menu::PopupMenu,
    group: &GroupRef,
    app: gpui::WeakEntity<AgenttyApp>,
    cx: &gpui::App,
) -> gpui_component::menu::PopupMenu {
    let (a1, a2, a3) = (app.clone(), app.clone(), app);
    let gref = group.clone();
    let can_create = group.target.is_none() || group.home.is_some();
    let menu = menu.item(
        PopupMenuItem::new(crate::core::i18n::current(cx, "menu.new_workspace"))
            .disabled(!can_create)
            .on_click(move |_, window, cx| {
                let _ = a1.update(cx, |this, cx| this.switcher_new(&gref, window, cx));
            }),
    );
    let Some(target) = group.target.clone() else {
        return menu;
    };
    let connected = group.link == Link::Connected;
    let restartable = target.can_restart_server();
    let (label, for_restart) = (group.label.clone(), target.clone());
    let menu = menu.separator().item(
        PopupMenuItem::new(crate::core::i18n::current(cx, "menu.disconnect"))
            .disabled(!connected)
            .on_click(move |_, _window, cx| {
                let _ = a2.update(cx, |this, cx| this.switcher_disconnect(&target, cx));
            }),
    );
    if !restartable {
        return menu;
    }
    menu.item(
        PopupMenuItem::new(crate::core::i18n::current(cx, "menu.restart_server")).on_click(
            move |_, window, cx| {
                let _ = a3.update(cx, |this, cx| {
                    this.confirm_restart_remote_server(
                        for_restart.clone(),
                        label.clone(),
                        window,
                        cx,
                    );
                });
            },
        ),
    )
}

fn row_menu(
    menu: gpui_component::menu::PopupMenu,
    row: &RowRef,
    app: gpui::WeakEntity<AgenttyApp>,
    cx: &gpui::App,
) -> gpui_component::menu::PopupMenu {
    let (a1, a2, a3, a4) = (app.clone(), app.clone(), app.clone(), app);
    let (id, adopt) = (row.id, row.adopt.is_some());
    let stoppable = row.live;
    menu.item(
        PopupMenuItem::new(crate::core::i18n::current(cx, "menu.rename"))
            .disabled(adopt)
            .on_click(move |_, window, cx| {
                let _ = a1.update(cx, |this, cx| this.switcher_rename(id, window, cx));
            }),
    )
    .item(
        PopupMenuItem::new(crate::core::i18n::current(cx, "menu.open_in_new_window"))
            .disabled(adopt)
            .on_click(move |_, window, cx| {
                let _ = a2.update(cx, |this, cx| {
                    this.close_switcher(window, cx);
                    crate::ui::windows::open(cx, Some(id));
                });
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(crate::core::i18n::current(cx, "menu.stop_workspace"))
            .disabled(adopt || !stoppable)
            .on_click(move |_, window, cx| {
                let _ = a3.update(cx, |this, cx| {
                    this.close_switcher(window, cx);
                    this.stop_workspace(id, window, cx);
                });
            }),
    )
    .item(
        PopupMenuItem::new(crate::core::i18n::current(cx, "menu.delete_workspace"))
            .disabled(adopt)
            .on_click(move |_, window, cx| {
                let _ = a4.update(cx, |this, cx| {
                    this.close_switcher(window, cx);
                    this.delete_workspace(id, window, cx);
                });
            }),
    )
}

fn rungs(cx: &App) -> crate::ui::presets::Surface {
    cx.global::<crate::ui::presets::Surfaces>().popover
}

fn hover_fill(cx: &App) -> gpui::Rgba {
    gpui::rgb(rungs(cx).hover)
}

fn glyph_col(w: f32, child: impl IntoElement) -> impl IntoElement {
    div()
        .w(px(w))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .child(child)
}

#[cfg(test)]
mod dialect_failure_tests {
    use super::{DialectFailureActions, dialect_failure_actions};

    #[test]
    fn known_dialect_skew_offers_only_replace_server() {
        let refusal = "control peer (build old) speaks control v6, this build speaks v7";
        assert_eq!(
            dialect_failure_actions(refusal, true),
            DialectFailureActions::ReplaceOnly,
        );
        assert_eq!(
            dialect_failure_actions("Connection refused", true),
            DialectFailureActions::RetryOnly,
        );
        assert_eq!(
            dialect_failure_actions(refusal, false),
            DialectFailureActions::None,
        );
    }
}

#[cfg(test)]
mod remote_attempt_projection_tests {
    use super::{GroupRef, Link};
    use crate::core::session::{
        RemoteRef, RemoteTarget, WindowView, WindowViews, WorkspaceId, WorkspaceStore,
    };
    use crate::ui::remote_workspace::RemoteLinks;
    use gpui::TestAppContext;

    fn profile_target() -> RemoteTarget {
        RemoteTarget::Profile {
            id: uuid::Uuid::nil(),
        }
    }

    fn install_profile_and_remote_workspace(
        cx: &mut gpui::VisualTestContext,
        target: &RemoteTarget,
    ) -> WorkspaceId {
        cx.update(|_, cx| {
            let mut config = cx.global::<crate::core::config::Config>().clone();
            let mut profile = crate::core::ssh_profile::SshProfile::new("in-flight-target");
            profile.id = uuid::Uuid::nil();
            profile.host = "127.0.0.1".into();
            profile.port = 1;
            config.ssh_profiles.push(profile);
            cx.set_global(config);

            let remote = WindowView::on_remote(RemoteRef::new(target.clone(), WorkspaceId::new()));
            let workspace = remote.id;
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![remote],
                    active: Some(workspace),
                },
            );
            RemoteLinks::retry_now(cx, workspace);
            workspace
        })
    }

    #[gpui::test]
    fn switcher_reports_supervised_in_flight_attempt_as_connecting(cx: &mut TestAppContext) {
        let (app, mut visual) = crate::ui::app::test_window::harness(cx);
        let target = profile_target();
        let _workspace = install_profile_and_remote_workspace(&mut visual, &target);

        let projected = app.update_in(&mut visual, |app, _window, cx| {
            // RemoteLinks::retry_now has created a Reconnecting MachineLink,
            // but no HostLinks is published until the attempt completes. The
            // switcher must not project this state as Offline.
            app.link_state(&target, cx)
        });
        assert!(
            projected == Link::Connecting,
            "an in-flight supervised host must stay actionable as Connecting"
        );
    }

    #[gpui::test]
    fn switcher_does_not_start_parallel_connect_for_supervised_host(cx: &mut TestAppContext) {
        let (app, mut visual) = crate::ui::app::test_window::harness(cx);
        let target = profile_target();
        let workspace = install_profile_and_remote_workspace(&mut visual, &target);
        let group = GroupRef {
            key: target.to_string(),
            label: "in-flight-target".into(),
            target: Some(target),
            home: None,
            // This is the stale projection produced by the current link_state.
            link: Link::Offline,
        };

        app.update_in(&mut visual, |app, _window, cx| {
            assert!(
                matches!(
                    RemoteLinks::status_of(cx, workspace),
                    Some(crate::ui::remote_workspace::RemoteStatus::Connecting)
                        | Some(crate::ui::remote_workspace::RemoteStatus::Reconnecting { .. })
                ),
                "the test fixture must begin with a supervised in-flight attempt"
            );
            app.switcher_toggle_host(&group, cx);
            assert!(
                app.connect.is_none(),
                "a click on an in-flight host must not create a second ConnectFlow"
            );
        });
    }
}
