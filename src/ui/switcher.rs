use std::collections::{HashMap, HashSet};

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, MouseButton, MouseDownEvent, Subscription,
    Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};

use tty7_core::core::machine::TabId;
use tty7_core::core::session::{RemoteTarget, RouteSnapshot, WorkspaceId};

use crate::core::actions::{SwitcherAcross, SwitcherAcrossBack};
use crate::core::session::WorkspaceStore;
use crate::daemon::install::InstallPhase;
use crate::terminal::pane_liveness::Liveness;
use crate::ui::app::Tty7App;
use crate::ui::i18n::{L10nKey, t, t_fmt};
use crate::ui::remote_connect::{self, HostChoice, RemoteWorkspaceRow};
use crate::ui::remote_workspace::{ConnectFlow, MachineStatus, RemoteLinks};

use super::manager_shell;
use super::manager_shell::step;
#[cfg(test)]
use super::manager_shell::{CARD_MARGIN, CARD_W, LEFT_W};

pub(crate) const CARD_TOP: f32 = manager_shell::CARD_TOP;

const ROW_AVATAR: f32 = 20.0;

const ROW_H: f32 = 32.0;
const HOST_H: f32 = 34.0;

const ROW_PAD: f32 = 8.0;

/// `Failed` stays a unit variant so `Link` can be `Copy` and travel by value in
/// `GroupRef`; what went wrong rides in `Group::error` instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Link {
    Local,
    Connected,
    Connecting,
    /// The supervisor is retrying this machine on its backoff. Distinct from
    /// `Offline` because the group still has rows worth showing and Disconnect
    /// still has something to call off.
    Reconnecting {
        attempt: u32,
    },
    Failed,
    Offline,
}

/// Whether Disconnect has anything to stop. A machine the supervisor is still
/// retrying has no live link, but calling the retry off is exactly what the
/// verb is for, so it has to stay enabled.
fn link_is_engaged(link: Link) -> bool {
    matches!(
        link,
        Link::Connected | Link::Connecting | Link::Reconnecting { .. }
    )
}

fn machine_link_label(link: Link) -> &'static str {
    t(match link {
        Link::Local => L10nKey::SwitcherThisComputer,
        Link::Connected => L10nKey::MachineConnected,
        Link::Connecting => L10nKey::SwitcherStatusConnecting,
        Link::Reconnecting { .. } => L10nKey::SwitcherStatusReconnecting,
        Link::Failed => L10nKey::SwitcherStatusConnectFailed,
        Link::Offline => L10nKey::SwitcherStatusNotConnected,
    })
}

/// How one machine's row is drawn, from the two things that know.
///
/// This window's own `connect` still comes first — it is the attempt the user
/// is watching, and it is the only one that can be Failed with a reason on
/// screen. Everything after it is the supervisor's, which is the only view that
/// survives the window that started the connect. Reading the `HostLinks` table
/// alone, as this used to, cannot tell a machine that is being retried right
/// now from one nobody has ever connected to: the pump drops the entry the
/// moment the link dies, and the group then collapses as if it were empty.
fn link_from(
    connect: Option<&ConnectFlow>,
    target: &RemoteTarget,
    supervised: Option<&MachineStatus>,
    has_link: bool,
) -> Link {
    match connect {
        Some(ConnectFlow::Connecting { choice }) if &choice.target == target => {
            return Link::Connecting;
        }
        Some(ConnectFlow::Failed { choice, .. }) if &choice.target == target => {
            return Link::Failed;
        }
        _ => {}
    }
    match supervised {
        Some(MachineStatus::Connecting) => Link::Connecting,
        Some(MachineStatus::Attached) => Link::Connected,
        Some(MachineStatus::Reconnecting { attempt, .. }) => {
            Link::Reconnecting { attempt: *attempt }
        }
        Some(MachineStatus::Failed(_)) => Link::Failed,
        None if has_link => Link::Connected,
        None => Link::Offline,
    }
}

struct Group {
    key: String,
    label: String,
    endpoint: String,
    target: Option<RemoteTarget>,
    link: Link,
    error: Option<String>,
    installing: Option<InstallPhase>,
    /// Another client is holding at least one workspace of this machine. The
    /// link itself is fine, so nothing in `link` would ever say so.
    preempted: bool,
    /// The route behind this group no longer resolves — its profile was
    /// deleted or its alias left the ssh config (#485). Parked groups are
    /// not retried; the group shows the snapshot label and offers to forget
    /// its entries instead of a retry button that could never succeed.
    parked: bool,
    rows: Vec<Row>,
}

struct Row {
    id: WorkspaceId,
    name: String,
    path: String,
    when: String,
    /// Raw timestamp behind `when` — what the flat list sorts by.
    last_active: u64,
    live: Liveness,
    open: bool,
    current: bool,
    preempted: bool,
    adopt: Option<Box<RemoteWorkspaceRow>>,
    remote_id: Option<WorkspaceId>,
    tabs: Vec<TabRow>,
}

/// One tab in the right-hand column. Built once per frame for every workspace
/// on the left, so the search can match tab names and the column can render
/// without a second pass over the machine tree.
#[derive(Clone)]
struct TabRow {
    recovery: Option<tty7_core::core::tab_view::PaneRecovery>,
    id: TabId,
    /// Position in the owning workspace's tab order — what `activate` wants.
    index: usize,
    label: String,
    path: String,
    /// Whether `label` is a name someone gave the tab. When it is not, the
    /// label is already derived from the working directory and showing `path`
    /// next to it just prints the same place twice.
    named: bool,
    agent: Option<crate::core::cli_agent::CLIAgent>,
    status: Option<crate::core::cli_agent::AgentStatus>,
    unread: usize,
    ssh: Option<u32>,
    active: bool,
    /// Branch and diff counts, the same line the tab sidebar shows. Only this
    /// window's own tabs have it — the machine tree carries no git state.
    git: Option<tty7_core::core::git::GitStatus>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Column {
    Left,
    Right,
}

/// A selectable line in the left column: `(group, row)` into `Layout::groups`.
/// The list is flat — one workspace per line, machines told apart by the badge
/// on the row itself — and rendering and keyboard navigation walk the same
/// list so an arrow key can never land somewhere the eye cannot see.
type Nav = (usize, usize);

pub(crate) struct HostSnapshot {
    pub target: RemoteTarget,
    pub rows: Vec<RemoteWorkspaceRow>,
}

/// A pane the daemon runs that no workspace holds — what an interrupted
/// `tty7 run` leaves behind, and what `tty7 pane ls --all` points the CLI's
/// reaper at. The switcher is where a GUI user finds and closes one (#596).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OrphanPane {
    pub pane_id: u64,
    pub title: String,
    pub cwd: Option<String>,
    pub owner: Option<String>,
}

/// Every pane id the local machine's workspaces hold — what the registry
/// listing is measured against to find the orphans (#596).
fn held_local_pane_ids(cx: &App) -> HashSet<u64> {
    crate::ui::machine_mirror::MachineMirrors::machine(cx, crate::core::session::HostId::LOCAL)
        .map(|machine| {
            machine
                .workspaces
                .iter()
                .flat_map(|ws| ws.tabs.iter())
                .flat_map(|tab| tab.root.pane_ids())
                .collect()
        })
        .unwrap_or_default()
}

/// The registry's live panes minus the ones a workspace holds. Dead entries
/// drop out too: a corpse the daemon has not reaped yet is not something the
/// user can act on.
pub(crate) fn orphan_panes_of(
    listed: Vec<tty7_core::daemon::protocol::PaneInfo>,
    held: &HashSet<u64>,
) -> Vec<OrphanPane> {
    listed
        .into_iter()
        .filter(|info| info.alive && !held.contains(&info.pane_id))
        .map(|info| OrphanPane {
            pane_id: info.pane_id,
            title: info.title,
            cwd: info.cwd.map(|p| p.display().to_string()),
            owner: info.owner,
        })
        .collect()
}

/// The name this machine shows for the workspace an orphan still claims —
/// `None` once that workspace is gone, which is the ordinary case for a pane
/// nobody holds.
fn workspace_name_of(owner: &str, cx: &App) -> Option<String> {
    let id = owner.parse::<WorkspaceId>().ok()?;
    let machine = crate::ui::machine_mirror::MachineMirrors::machine(
        cx,
        crate::core::session::HostId::LOCAL,
    )?;
    let ws = machine.workspaces.iter().find(|ws| ws.id == id)?;
    Some(crate::ui::machine_mirror::display_name_of(
        ws,
        &machine.panes,
    ))
}

/// What to print where an orphan names its owner. The owner it carries is a
/// `WorkspaceId` — 36 characters of UUID that say nothing on screen and, left
/// whole, push the row's Close button clean out of the card. So: name the
/// workspace when this machine still has one, and otherwise keep the first 8
/// characters, which is what `tty7 pane ls --all` prints and enough to line
/// the two listings up. An owner that is no workspace id at all is an older
/// client's own label (`tty7-cli`) and already reads fine.
fn owner_label(owner: &str, workspace_name: Option<String>) -> String {
    workspace_name.unwrap_or_else(|| match owner.parse::<WorkspaceId>() {
        Ok(_) => owner.chars().take(8).collect(),
        Err(_) => owner.to_string(),
    })
}

pub(crate) struct Switcher {
    pub query: Entity<InputState>,
    /// Panes the local daemon runs that no workspace holds (#596). Filled
    /// asynchronously after the panel opens; empty both while the listing is
    /// in flight and when there is nothing to reap.
    orphans: Vec<OrphanPane>,
    column: Column,
    left_sel: usize,
    right_sel: usize,
    /// Order the tab column most-recently-used first. Set when Ctrl+Tab opened
    /// the panel; the plain Cmd+Shift+O panel keeps strip order.
    mru: bool,
    /// The modifiers held down when Ctrl+Tab opened the panel. Releasing them
    /// commits the highlighted tab, IDEA-style.
    hold: Option<gpui::Modifiers>,
    /// Where the pointer is: inside the card at all, and inside the tab column
    /// specifically. Both are set by hover listeners, so they only mean
    /// anything once the mouse has moved since the panel came up.
    hover_card: bool,
    hover_tabs: bool,
    left_scroll: gpui::ScrollHandle,
    right_scroll: gpui::ScrollHandle,
    /// Anchors on the two scrolls, worn by whichever row is selected. Both
    /// columns hold their rows inside one child element, and `scroll_to_item`
    /// indexes a scroll's *direct* children — so it could only ever find item
    /// 0, and walking the list with the arrows quietly left the selection off
    /// the bottom of the column.
    left_anchor: gpui::ScrollAnchor,
    right_anchor: gpui::ScrollAnchor,
    _subs: Vec<Subscription>,
}

impl Switcher {
    fn text(&self, cx: &App) -> String {
        self.query.read(cx).value().trim().to_lowercase()
    }

    /// The pointer is parked in the card but off the tab column — on a
    /// workspace row, the search box, a banner. Letting go of Ctrl there is
    /// not a commit: the user is reaching for the mouse, and closing the panel
    /// out from under them makes the workspace list unreachable by hand.
    fn hover_keeps_open(&self) -> bool {
        self.hover_card && !self.hover_tabs
    }
}

/// Everything the panel needs for one frame: the groups (one per machine,
/// still the unit that carries link state and errors), and the flat,
/// most-recently-used-first left column the arrow keys walk.
struct Layout {
    groups: Vec<Group>,
    nav: Vec<Nav>,
}

impl Layout {
    /// Which workspace row the tab column is showing.
    fn subject(&self, sel: usize) -> Option<(usize, usize)> {
        self.nav.get(sel).copied()
    }

    fn subject_row(&self, sel: usize) -> Option<&Row> {
        let (g, r) = self.subject(sel)?;
        self.groups[g].rows.get(r)
    }
}

/// The flat left column: every row the query leaves visible, one line per
/// workspace, most recently used first with this window's own workspace on
/// top. A query matching a machine's name keeps all of that machine's rows —
/// searching "devbox" is how the old per-machine grouping is asked for now.
fn flatten(groups: &[Group], query: &str) -> Vec<Nav> {
    let mut nav: Vec<Nav> = Vec::new();
    for (g, group) in groups.iter().enumerate() {
        let matched_host = group.label.to_lowercase().contains(query);
        for (r, row) in group.rows.iter().enumerate() {
            if query.is_empty() || matched_host || row.matches(query) {
                nav.push((g, r));
            }
        }
    }
    nav.sort_by(|&(ga, ra), &(gb, rb)| {
        let (a, b) = (&groups[ga].rows[ra], &groups[gb].rows[rb]);
        b.current
            .cmp(&a.current)
            .then_with(|| b.last_active.cmp(&a.last_active))
            .then_with(|| a.name.cmp(&b.name))
    });
    nav
}

impl Tty7App {
    /// Saved configurations select a managed machine, never a native SSH pane
    /// nested in the current machine's tab tree.
    pub(crate) fn open_configured_machine(
        &mut self,
        id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = RemoteTarget::Profile { id };
        let Some(choice) = remote_connect::available_hosts(cx)
            .into_iter()
            .find(|h| h.target == target)
        else {
            return;
        };
        if let Some(group) = self
            .switcher_groups(cx)
            .into_iter()
            .find(|g| g.target.as_ref() == Some(&target))
        {
            if matches!(group.link, Link::Connecting | Link::Reconnecting { .. }) {
                return;
            }
            if group.link == Link::Connected {
                if let Some(row) = group
                    .rows
                    .iter()
                    .find(|r| r.current)
                    .or_else(|| group.rows.iter().max_by_key(|r| r.last_active))
                {
                    self.switcher_open(RowRef::of(&group, row), false, window, cx);
                } else if let Some(home) = remote_connect::HostLinks::home(cx, target.host_id()) {
                    self.create_remote_workspace(target, home, window, cx);
                }
                return;
            }
        }
        self.connect_to_host(choice, cx);
    }

    fn toggle_machine_pin(&mut self, target: RemoteTarget, cx: &mut Context<Self>) {
        self.update_config(cx, |cfg| {
            let pinned = cfg.machine_rail.pinned.contains(&target);
            cfg.machine_rail.set_pinned(target, !pinned);
        });
    }

    pub(super) fn toggle_machine_fold(&mut self, key: String, cx: &mut Context<Self>) {
        self.update_config(cx, |cfg| cfg.machine_rail.toggle_collapsed(key));
    }

    pub(crate) fn machine_launcher_items(
        &self,
        app: gpui::WeakEntity<Self>,
        cx: &mut App,
    ) -> Vec<PopupMenuItem> {
        self.switcher_groups(cx)
            .into_iter()
            .map(|group| {
                let row = group
                    .rows
                    .iter()
                    .find(|r| r.current)
                    .or_else(|| group.rows.iter().max_by_key(|r| r.last_active))
                    .map(|r| RowRef::of(&group, r));
                let current = group.rows.iter().any(|r| r.current);
                let state = machine_link_label(group.link);
                let label = format!("{} — {}", group.label, state);
                let choice = group.target.map(|target| HostChoice {
                    target,
                    label: group.label,
                    detail: group.endpoint,
                });
                let app = app.clone();
                PopupMenuItem::new(label)
                    .checked(current)
                    .on_click(move |_, window, cx| {
                        let _ = app.update(cx, |this, cx| {
                            if let Some(row) = row.clone() {
                                this.switcher_open(row, false, window, cx);
                            } else if let Some(choice) = choice.clone() {
                                this.connect_to_host(choice, cx);
                            } else {
                                this.switch_workspace(None, window, cx);
                            }
                        });
                    })
            })
            .collect()
    }

    /// Machine navigation uses the same target facts and activation operations
    /// as the switcher; workspace ids remain ownership, not visual grouping.
    pub(crate) fn render_machine_rail(
        &self,
        current_rows: AnyElement,
        query: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut rail = v_flex()
            .id("machine-rail")
            .w_full()
            .flex_1()
            .min_h_0()
            .track_scroll(&self.sidebar_scroll)
            .overflow_y_scroll()
            .gap_1();
        let mut current_rows = Some(current_rows);
        for group in self.switcher_groups(cx) {
            let expanded = cx
                .global::<crate::core::config::Config>()
                .machine_rail
                .expanded(&group.key, !query.is_empty());
            let fold_key = group.key.clone();
            let gref = GroupRef::of(&group);
            let menu_ref = gref.clone();
            let row = group
                .rows
                .iter()
                .find(|r| r.current)
                .or_else(|| group.rows.iter().max_by_key(|r| r.last_active))
                .map(|r| RowRef::of(&group, r));
            let choice = group.target.clone().map(|target| HostChoice {
                target,
                label: group.label.clone(),
                detail: group.endpoint.clone(),
            });
            let current = group.rows.iter().any(|r| r.current);
            let state = machine_link_label(group.link);
            let app = cx.entity().downgrade();
            let menu_app = app.clone();
            let history_host = group
                .target
                .as_ref()
                .map(|target| target.host_id())
                .unwrap_or(tty7_core::host::HostId::LOCAL);
            let history_label = group.label.clone();
            let header = h_flex()
                .id(gpui::SharedString::from(format!(
                    "machine-header:{}",
                    group.key
                )))
                .w_full()
                .px_2()
                .py_1()
                .gap_2()
                .cursor_pointer()
                .when(current, |d| d.bg(cx.theme().secondary))
                .child(
                    Button::new(gpui::SharedString::from(format!(
                        "machine-fold:{}",
                        group.key
                    )))
                    .icon(Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    }))
                    .ghost()
                    .xsmall()
                    .tooltip(t(if expanded {
                        L10nKey::MachineCollapse
                    } else {
                        L10nKey::MachineExpand
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_machine_fold(fold_key.clone(), cx);
                    })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(group.label.clone()),
                )
                .child(div().text_color(cx.theme().muted_foreground).child(state))
                .child(
                    Button::new(gpui::SharedString::from(format!(
                        "machine-history:{}",
                        group.key
                    )))
                    .icon(IconName::Search)
                    .ghost()
                    .xsmall()
                    .tooltip(t(L10nKey::HistoryManagerTitle))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_history_manager(history_host, history_label.clone(), window, cx);
                    })),
                )
                .child(
                    Button::new(gpui::SharedString::from(format!(
                        "machine-more:{}",
                        group.key
                    )))
                    .icon(IconName::Ellipsis)
                    .ghost()
                    .xsmall()
                    .tooltip(t(L10nKey::TabTooltipMore))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .dropdown_menu(move |menu, _, cx| {
                        host_menu(menu, &menu_ref, menu_app.clone(), cx)
                    }),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(row) = row.clone() {
                        this.switcher_open(row, false, window, cx);
                    } else if let Some(choice) = choice.clone() {
                        this.connect_to_host(choice, cx);
                    } else {
                        this.switch_workspace(None, window, cx);
                    }
                }))
                .context_menu(move |menu, _, cx| host_menu(menu, &gref, app.clone(), cx));
            let mut section = v_flex().w_full().flex_shrink_0().child(header);
            if !expanded {
                rail = rail.child(section);
                continue;
            }
            for row in &group.rows {
                if row.current {
                    if let Some(rows) = current_rows.take() {
                        section = section.child(rows);
                    }
                    continue;
                }
                for tab in &row.tabs {
                    if !query.is_empty()
                        && !format!("{} {}", tab.label, tab.path)
                            .to_lowercase()
                            .contains(query)
                    {
                        continue;
                    }
                    let (ws, tab_id, index) = (row.id, tab.id, tab.index);
                    let adopt = RowRef::of(&group, row);
                    let item = v_flex()
                        .id(gpui::SharedString::from(format!(
                            "machine-session:{}:{ws}:{index}",
                            group.key
                        )))
                        .w_full()
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(cx.theme().secondary))
                        .child(
                            h_flex()
                                .gap_1()
                                .child(div().flex_1().min_w_0().truncate().child(tab.label.clone()))
                                .children(
                                    tab.recovery.as_ref().map(|recovery| {
                                        super::session_recovery::badge(recovery, cx)
                                    }),
                                ),
                        )
                        .when(!tab.path.is_empty(), |d| {
                            d.child(
                                div()
                                    .truncate()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(tab.path.clone()),
                            )
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if adopt.adopt.is_some() {
                                if this.switcher_open(adopt.clone(), false, window, cx) {
                                    this.activate_tree_tab(tab_id, window, cx);
                                }
                            } else {
                                this.switcher_open_tab(ws, tab_id, false, window, cx);
                            }
                        }));
                    section = section.child(item);
                }
            }
            rail = rail.child(section);
        }
        rail.into_any_element()
    }

    pub(crate) fn toggle_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.switcher.is_some() {
            self.close_switcher(window, cx);
        } else {
            self.open_switcher(window, cx);
        }
    }

    pub(crate) fn open_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_switcher_in(Column::Left, false, None, window, cx);
    }

    fn open_switcher_in(
        &mut self,
        column: Column,
        mru: bool,
        hold: Option<gpui::Modifiers>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        remote_connect::register(cx);
        remote_connect::sweep_wsl(cx);
        let query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::ui::i18n::t(
                    crate::ui::i18n::L10nKey::SearchWorkspacesAndMachines,
                ))
                // On macOS a held Ctrl turns every click into a right click,
                // and the input answers a right click with Cut/Copy/Paste —
                // so reaching for this box mid-Ctrl+Tab popped a menu instead
                // of placing a caret. The rows already dodge this by dropping
                // their own menus while the gesture is on; this box has no
                // menu worth keeping either, and Cmd+V still pastes.
                .context_menu(false)
        });
        query.update(cx, |state, cx| state.focus(window, cx));
        let subs = vec![cx.subscribe_in(
            &query,
            window,
            |this, _input, ev: &InputEvent, _window, cx| {
                if matches!(ev, InputEvent::Change) {
                    // A narrower list can strand the cursor past its end. Land
                    // it on the first hit so the tab column shows the hits
                    // straight away.
                    if let Some(sw) = this.switcher.as_mut() {
                        sw.left_sel = 0;
                        sw.right_sel = 0;
                    }
                    cx.notify();
                }
            },
        )];
        let (left_scroll, right_scroll) = (gpui::ScrollHandle::new(), gpui::ScrollHandle::new());
        self.switcher = Some(Switcher {
            query,
            orphans: Vec::new(),
            column,
            left_sel: 0,
            right_sel: 0,
            mru,
            hold,
            hover_card: false,
            hover_tabs: false,
            left_scroll: left_scroll.clone(),
            right_scroll: right_scroll.clone(),
            left_anchor: gpui::ScrollAnchor::for_handle(left_scroll),
            right_anchor: gpui::ScrollAnchor::for_handle(right_scroll),
            _subs: subs,
        });
        // Park the left cursor on this window's own workspace so the tab column
        // opens on something useful.
        let layout = self.switcher_layout(cx);
        let here = self.workspace;
        if let Some(at) = layout
            .nav
            .iter()
            .position(|&(g, r)| layout.groups[g].rows[r].id == here)
            && let Some(sw) = self.switcher.as_mut()
        {
            sw.left_sel = at;
        }
        self.refresh_orphan_panes(cx);
        cx.notify();
    }

    /// List the local daemon's panes and keep the ones no workspace holds.
    /// The query is blocking daemon I/O, so it runs off the UI thread; the
    /// filter runs back on it, where the machine mirror lives.
    ///
    /// Local on purpose: a remote machine's orphans belong to its own daemon,
    /// and routing a listing per host is what the CLI's reaper already does.
    fn refresh_orphan_panes(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_spawn(async move { tty7_core::client::PaneClient::local().list() })
                .await;
            let listed = match listed {
                Ok(listed) => listed,
                Err(e) => {
                    log::warn!(target: "tty7::switcher", "orphan pane listing failed: {e}");
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                let held = held_local_pane_ids(cx);
                if let Some(sw) = this.switcher.as_mut() {
                    sw.orphans = orphan_panes_of(listed, &held);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Hang up one orphan pane and show what is left. The kill is
    /// fire-and-forget, so the refresh that follows is also the confirmation:
    /// a pane that survived it simply stays on the list.
    fn close_orphan_pane(&mut self, pane_id: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_spawn(async move {
                    let client = tty7_core::client::PaneClient::local();
                    client.kill(pane_id)?;
                    client.list()
                })
                .await;
            let listed = match listed {
                Ok(listed) => listed,
                Err(e) => {
                    log::warn!(target: "tty7::switcher", "closing orphan %{pane_id} failed: {e}");
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                let held = held_local_pane_ids(cx);
                if let Some(sw) = this.switcher.as_mut() {
                    sw.orphans = orphan_panes_of(listed, &held);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Ctrl+Tab. The first press raises the panel on the tab column with the
    /// previously used tab already highlighted; further presses walk it. Holding
    /// the modifier keeps the panel up, releasing it commits — IDEA's gesture.
    ///
    /// With fewer than two tabs there is nothing to cycle, so the panel opens on
    /// the workspace column and *stays* — no hold, no commit-on-release. The
    /// gesture degrades into "open the switcher" rather than doing nothing.
    pub(crate) fn tab_switch(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.switcher.is_some() {
            let layout = self.switcher_layout(cx);
            self.switcher_step_right(&layout, forward, window, cx);
            return;
        }
        let n = self.tabs.len();
        let cycling = n >= 2;
        let held = window.modifiers();
        self.open_switcher_in(
            match cycling {
                true => Column::Right,
                false => Column::Left,
            },
            cycling,
            (cycling && held.modified()).then_some(held),
            window,
            cx,
        );
        if cycling && let Some(sw) = self.switcher.as_mut() {
            sw.right_sel = match forward {
                true => 1,
                false => n - 1,
            };
        }
        cx.notify();
    }

    /// Watches the modifiers while a Ctrl+Tab panel is up. Letting go of any
    /// part of the combination that raised it is the commit gesture.
    pub(crate) fn switcher_hold_changed(
        &mut self,
        now: &gpui::Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(hold) = self.switcher.as_ref().and_then(|sw| sw.hold) else {
            return;
        };
        if now.modified() && hold.is_subset_of(now) {
            return;
        }
        // The pointer is already on the workspace list or the search box, so
        // the release is the user's hand leaving the keyboard, not a pick.
        // Drop the hold and leave the panel up for the mouse to finish in.
        if self
            .switcher
            .as_ref()
            .is_some_and(Switcher::hover_keeps_open)
        {
            self.switcher_release_hold(cx);
            return;
        }
        self.switcher_commit_hold(window, cx);
    }

    /// Called when the modifier that raised the panel comes back up.
    pub(crate) fn switcher_commit_hold(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.switcher.as_ref().and_then(|sw| sw.hold).is_none() {
            return;
        }
        let layout = self.switcher_layout(cx);
        self.switcher_confirm(&layout, false, window, cx);
        // Confirming a tab already closed the panel; anything else (an empty
        // column, a workspace with nothing in it) still has to come down.
        if self.switcher.is_some() {
            self.close_switcher(window, cx);
        }
    }

    /// Drops the hold without acting on it. The modifier release will never
    /// arrive at a window that is no longer focused, so the panel would
    /// otherwise sit there waiting forever.
    pub(crate) fn switcher_release_hold(&mut self, cx: &mut Context<Self>) {
        if let Some(sw) = self.switcher.as_mut()
            && sw.hold.take().is_some()
        {
            cx.notify();
        }
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

    fn switcher_groups(&self, cx: &mut App) -> Vec<Group> {
        let now = crate::ui::home::now_secs();
        let current = self.workspace;
        crate::terminal::pane_liveness::sweep(cx);

        let mut groups: Vec<Group> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        {
            let app: &App = cx;
            let store = WorkspaceStore::all(app);
            for w in &store.views {
                let (key, label, endpoint, target) = match w.host.as_ref() {
                    None => (
                        String::new(),
                        t(L10nKey::SwitcherThisComputer).to_string(),
                        String::new(),
                        None,
                    ),
                    Some(r) => {
                        let key = r.target.to_string();
                        // The live profile's own name overwrites this below
                        // while the route still resolves; when it does not,
                        // the snapshot — or, for entries saved before
                        // snapshots existed, a placeholder — is all the entry
                        // has. Never the bare profile UUID (#485).
                        let label = r.route_label(t(L10nKey::RemoteProfileGone));
                        let endpoint = r
                            .via
                            .as_ref()
                            .map(RouteSnapshot::endpoint)
                            .filter(|e| e != &label)
                            .unwrap_or_default();
                        (key, label, endpoint, Some(r.target.clone()))
                    }
                };
                let slot = *index.entry(key.clone()).or_insert_with(|| {
                    groups.push(Group {
                        key,
                        label,
                        endpoint,
                        target,
                        link: Link::Offline,
                        error: None,
                        installing: None,
                        preempted: false,
                        parked: false,
                        rows: Vec::new(),
                    });
                    groups.len() - 1
                });
                // A row's path is on the workspace's own machine, so that is
                // the machine whose home may shorten it (#580).
                let home = crate::ui::path_display::home_for_host(app, w.host_id());
                groups[slot].rows.push(Row {
                    id: w.id,
                    name: crate::ui::machine_mirror::display_name(app, w)
                        .unwrap_or_else(|| t(L10nKey::WindowUntitled).to_string()),
                    path: crate::ui::machine_mirror::subject_path(app, w)
                        .map(|p| {
                            crate::ui::home::display_path(std::path::Path::new(&p), home.as_deref())
                        })
                        .unwrap_or_default(),
                    when: crate::ui::home::relative_time(now, w.last_active),
                    last_active: w.last_active,
                    live: crate::terminal::pane_liveness::liveness_of(app, w),
                    open: w.open,
                    current: w.id == current,
                    preempted: false,
                    adopt: None,
                    remote_id: w.host.as_ref().map(|r| r.workspace),
                    tabs: self.tab_rows_for(w.id, app),
                });
            }
        }

        // Listed once for the whole frame: the pending groups below name
        // themselves from it, and so does the pass that settles every group's
        // link state further down.
        let configured = remote_connect::available_hosts(cx);

        for target in self.pending_machines().into_iter().chain(
            cx.global::<crate::core::config::Config>()
                .machine_rail
                .pinned
                .clone(),
        ) {
            let key = target.to_string();
            if index.contains_key(&key) {
                continue;
            }
            index.insert(key.clone(), groups.len());
            groups.push(Group {
                // Not `key`: a `Profile` target spells itself as its config
                // UUID, so a machine whose profile has been deleted would
                // announce itself to the banners below by a raw UUID (#485).
                // The pass below overwrites this while the profile is still
                // configured; this is what is left when it is not.
                label: remote_connect::label_from_hosts(&configured, &target),
                key,
                endpoint: String::new(),
                target: Some(target),
                link: Link::Offline,
                error: None,
                installing: None,
                preempted: false,
                parked: false,
                rows: Vec::new(),
            });
        }

        if !index.contains_key("") {
            groups.insert(
                0,
                Group {
                    key: String::new(),
                    label: t(L10nKey::SwitcherThisComputer).to_string(),
                    endpoint: String::new(),
                    target: None,
                    link: Link::Offline,
                    error: None,
                    installing: None,
                    preempted: false,
                    parked: false,
                    rows: Vec::new(),
                },
            );
        }

        // This window's own workspace has to be in the list even when the store
        // has not caught up with it: Ctrl+Tab reaches its tabs through the same
        // left column, and a missing row would leave the panel with nothing to
        // switch between.
        if !groups
            .iter()
            .any(|g| g.rows.iter().any(|r| r.id == current))
            && let Some(slot) = groups.iter().position(|g| g.key.is_empty())
        {
            let app: &App = cx;
            groups[slot].rows.insert(
                0,
                Row {
                    id: current,
                    name: crate::ui::machine_mirror::display_name_for(app, current)
                        .unwrap_or_else(|| t(L10nKey::WindowUntitled).to_string()),
                    path: String::new(),
                    when: crate::ui::home::relative_time(now, now),
                    last_active: now,
                    live: Liveness::Alive,
                    open: true,
                    current: true,
                    preempted: false,
                    adopt: None,
                    remote_id: None,
                    tabs: self.tab_rows_for(current, app),
                },
            );
        }

        // Workspaces this machine holds that the store has never heard of: the
        // CLI makes them too, and one that never appears here looks to the
        // person who ran `tty7 new` like nothing happened at all. They open
        // like any other row — the id in the tree is the id a window claims.
        if let Some(slot) = groups.iter().position(|g| g.key.is_empty()) {
            // Measured against the rows already listed rather than against the
            // store, which is what put them there: the block above lists this
            // window's own workspace before the store has caught up with it,
            // and two rows under one id would be two ways into one window.
            let listed: Vec<WorkspaceId> = groups
                .iter()
                .flat_map(|g| g.rows.iter().map(|r| r.id))
                .collect();
            let app: &App = cx;
            // Unclaimed *local* workspaces: their paths are on this machine,
            // so this machine's home is the right one to measure them by.
            let local_home = crate::ui::path_display::local_home();
            let rows: Vec<Row> = crate::ui::machine_mirror::unclaimed_local_workspaces(app)
                .into_iter()
                .filter(|ws| !listed.contains(&ws.id))
                .map(|ws| Row {
                    id: ws.id,
                    name: ws.name,
                    path: ws
                        .path
                        .map(|p| {
                            crate::ui::home::display_path(
                                std::path::Path::new(&p),
                                local_home.as_deref(),
                            )
                        })
                        .unwrap_or_default(),
                    when: crate::ui::home::relative_time(now, ws.last_active),
                    last_active: ws.last_active,
                    live: match ws.live {
                        true => Liveness::Alive,
                        false => Liveness::Stopped,
                    },
                    open: false,
                    current: false,
                    preempted: false,
                    adopt: None,
                    remote_id: None,
                    tabs: self.tab_rows_for(ws.id, app),
                })
                .collect();
            groups[slot].rows.extend(rows);
        }

        // Row order is `flatten`'s business now — the left column is one flat
        // most-recently-used list. Groups keep local-first order only so the
        // trouble banners under the list come out in a stable order.
        groups.sort_by_key(|g| {
            cx.global::<crate::core::config::Config>()
                .machine_rail
                .order_key(&g.key)
        });

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
            let id = target.host_id();
            let supervised = RemoteLinks::machine_status(cx, id);
            group.link = self.link_state(&target, supervised.as_ref(), cx);
            // A route that no longer resolves parks the whole group (#485):
            // not retried, labeled from the snapshot, forget-not-retry for
            // actions. A live link is exempt — its panes keep working
            // regardless of what happened to the profile that made them.
            group.parked =
                group.link != Link::Connected && !remote_connect::route_resolvable(cx, &target);
            if let Some(ConnectFlow::Failed { choice, error }) = &self.connect
                && choice.target == target
            {
                group.error = Some(error.clone());
            }
            if group.error.is_none() {
                if let Some(error) = self.remote_host_errors.get(&target.to_string()) {
                    group.error = Some(error.clone());
                }
            }
            // The supervisor's own attempts never touch this window's `connect`,
            // so without this last fallback the route it could not build, and
            // the reconnect that has been failing for an hour, have nowhere at
            // all to be said.
            if group.error.is_none() {
                group.error = match supervised {
                    Some(MachineStatus::Failed(e)) => Some(e),
                    Some(MachineStatus::Reconnecting { last_error, .. }) => last_error,
                    _ => None,
                };
            }
            let taken: HashSet<WorkspaceId> =
                RemoteLinks::preempted_on(cx, id).into_iter().collect();
            group.preempted = !taken.is_empty();
            for row in &mut group.rows {
                row.preempted = taken.contains(&row.id);
            }
            let reported = remote_connect::install_progress_for(id);
            if group.link == Link::Connecting
                || matches!(group.link, Link::Reconnecting { .. })
                || group.error.is_some()
                || matches!(reported, Some(InstallPhase::Restarting))
            {
                group.installing = reported;
            }
            if let Some(snapshot) = self.host_snapshots.get(&id) {
                group.merge(&snapshot.rows, now);
            }
        }
        groups
    }

    /// The tab column's rows for one workspace. This window's own workspace has
    /// live in-memory tabs (agent status, unread counts, MRU order); every other
    /// workspace comes out of the machine mirror, which is the only view this
    /// process has of windows it does not own.
    fn tab_rows_for(&self, id: WorkspaceId, cx: &App) -> Vec<TabRow> {
        if id == self.workspace {
            let recoveries = super::session_recovery::for_workspace(cx, id);
            let order = match self.switcher.as_ref().is_some_and(|sw| sw.mru) {
                true => self.tabs_by_mru(),
                false => (0..self.tabs.len()).collect(),
            };
            return order
                .into_iter()
                .map(|i| {
                    let tab = &self.tabs[i];
                    TabRow {
                        recovery: if tab.agent(cx).is_none() {
                            recoveries.get(&tab.tree_id.get()).cloned()
                        } else {
                            None
                        },
                        id: tab.tree_id.get(),
                        index: i,
                        label: self.tab_label(tab, i, None, cx),
                        named: tab.name.as_deref().is_some_and(|n| !n.trim().is_empty())
                            || tab.agent(cx).is_some(),
                        path: tab
                            .pane
                            .terminals()
                            .first()
                            .and_then(|leaf| {
                                let leaf = leaf.read(cx);
                                Some((leaf.cwd()?, leaf.display_home(cx)))
                            })
                            .map(|(p, home)| crate::ui::home::display_path(&p, home.as_deref()))
                            .unwrap_or_default(),
                        agent: tab.agent(cx),
                        status: tab.agent_status(cx),
                        unread: tab.agent_unread_count(cx),
                        ssh: self.tab_ssh_dot(tab, cx),
                        active: i == self.active,
                        git: tab.git_status(None, cx),
                    }
                })
                .collect();
        }

        let Some((views, active)) = crate::ui::machine_mirror::tab_views_for(cx, id) else {
            return Vec::new();
        };
        // Git state is cached globally per (host, cwd) and outlives the panes
        // that filled it, so a workspace this window recently left still has
        // its branches on hand. Read-only on purpose: probing every cwd of
        // every workspace to populate a panel that closes in a second would
        // cost a git invocation each, and a round trip each when the host is
        // remote.
        let host = WorkspaceStore::all(cx).get(id).map(|w| w.host_id());
        let git = |cwd: Option<&str>| -> Option<tty7_core::core::git::GitStatus> {
            let (host, cwd) = (host?, cwd?);
            cx.try_global::<crate::terminal::git_status::GitStatusCache>()?
                .status_for(host, std::path::Path::new(cwd))
        };
        // These rows describe a workspace on `host`, and the cwds they carry
        // are that machine's. Only its home may shorten them (#580).
        let home = host.and_then(|host| crate::ui::path_display::home_for_host(cx, host));
        views
            .into_iter()
            .enumerate()
            .map(|(i, v)| TabRow {
                label: tab_view_label(&v, i, home.as_deref()),
                // The label only stands in for the path when it came *from* the
                // path; a name or an agent leaves the location still worth
                // printing.
                named: v.name.as_deref().is_some_and(|n| !n.trim().is_empty()) || v.agent.is_some(),
                path: v
                    .cwd
                    .as_deref()
                    .map(|p| {
                        crate::ui::home::display_path(std::path::Path::new(p), home.as_deref())
                    })
                    .unwrap_or_default(),
                agent: v.agent,
                status: if v.recovery.is_some() { None } else { v.status },
                recovery: v.recovery,
                unread: 0,
                ssh: None,
                active: Some(v.id) == active,
                git: git(v.cwd.as_deref()),
                index: i,
                id: v.id,
            })
            .collect()
    }

    /// Builds one frame's worth of panel: the groups, and the flat left
    /// column the arrow keys walk.
    fn switcher_layout(&self, cx: &mut Context<Self>) -> Layout {
        let groups = self.switcher_groups(cx);
        let query = self
            .switcher
            .as_ref()
            .map(|sw| sw.text(cx))
            .unwrap_or_default();
        let nav = flatten(&groups, &query);
        Layout { groups, nav }
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

    fn link_state(
        &self,
        target: &RemoteTarget,
        supervised: Option<&MachineStatus>,
        cx: &mut App,
    ) -> Link {
        let has_link = remote_connect::HostLinks::get(cx, target.host_id()).is_some();
        link_from(self.connect.as_ref(), target, supervised, has_link)
    }

    fn switcher_open(
        &mut self,
        row: RowRef,
        new_window: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.close_switcher(window, cx);
        match row.adopt {
            Some((target, remote)) => self.open_remote_workspace(target, *remote, window, cx),
            None if new_window => {
                crate::ui::windows::open(cx, Some(row.id));
                false
            }
            None => {
                if !self.switcher_window_conflict(row.id, window, cx) {
                    self.switch_workspace(Some(row.id), window, cx);
                    return self.workspace == row.id;
                }
                false
            }
        }
    }

    fn switcher_window_conflict(
        &self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if workspace != self.workspace
            && crate::ui::windows::WindowRegistry::window_for(cx, workspace).is_some()
        {
            window.push_notification(t(L10nKey::MachineSessionWindowOccupied), cx);
            return true;
        }
        false
    }

    /// Select only the workspace just resolved from fresh target facts.
    /// Client mappings are locators, never permission to take another window.
    pub(crate) fn machine_target(
        &self,
        host: crate::ui::host_ops::HostId,
        cx: &mut App,
    ) -> Option<crate::core::session::RemoteTarget> {
        self.switcher_groups(cx)
            .into_iter()
            .filter_map(|g| g.target)
            .find(|t| t.host_id() == host)
    }

    pub(crate) fn resolve_rejoin_workspace(
        &mut self,
        host: crate::ui::host_ops::HostId,
        target_ws: WorkspaceId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<WorkspaceId> {
        let id = if host.is_local() {
            if WorkspaceStore::all(cx)
                .get(target_ws)
                .is_some_and(|view| !view.host_id().is_local())
            {
                return None;
            }
            target_ws
        } else {
            let target = self
                .switcher_groups(cx)
                .into_iter()
                .filter_map(|group| group.target)
                .find(|target| target.host_id() == host)?;
            let mapped: Vec<_> = WorkspaceStore::all(cx)
                .views
                .iter()
                .filter(|view| {
                    view.host
                        .as_ref()
                        .is_some_and(|r| r.host_id() == host && r.workspace == target_ws)
                })
                .map(|view| view.id)
                .collect();
            if mapped.len() > 1 {
                return None;
            }
            match mapped.first() {
                Some(id) => *id,
                None => {
                    if crate::ui::windows::WindowRegistry::window_for(cx, target_ws).is_some() {
                        return None;
                    }
                    WorkspaceStore::claim_remote(
                        cx,
                        tty7_core::core::session::RemoteRef::new(target, target_ws),
                    )
                }
            }
        };
        if super::remote_workspace::workspace_is_preempted(cx, id) {
            return None;
        }
        if id != self.workspace
            && (crate::ui::windows::WindowRegistry::window_for(cx, id).is_some()
                || WorkspaceStore::all(cx)
                    .get(id)
                    .is_some_and(|view| view.open))
        {
            return None;
        }
        Some(id)
    }

    /// Select a carrier here, never focus another GUI window. The caller has
    /// already validated the target source and its connected Host instance.
    pub(crate) fn activate_history_machine(
        &mut self,
        host: crate::ui::host_ops::HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<WorkspaceId> {
        if self.spawn_host(cx) == host
            && !super::remote_workspace::workspace_is_preempted(cx, self.workspace)
        {
            return Some(self.workspace);
        }
        let group = self.switcher_groups(cx).into_iter().find(|group| {
            group
                .target
                .as_ref()
                .map(RemoteTarget::host_id)
                .unwrap_or(crate::ui::host_ops::HostId::LOCAL)
                == host
        })?;
        let candidate = group.history_carrier(cx);
        let id = match candidate {
            Some(row) => match (&group.target, &row.adopt) {
                (Some(target), Some(remote)) => WorkspaceStore::claim_remote(
                    cx,
                    tty7_core::core::session::RemoteRef::new(target.clone(), remote.id),
                ),
                _ => row.id,
            },
            None => match group.target {
                Some(target) => WorkspaceStore::claim_remote(
                    cx,
                    tty7_core::core::session::RemoteRef::new(target, WorkspaceId::new()),
                ),
                None => WorkspaceStore::claim(cx, None),
            },
        };
        self.switch_workspace(Some(id), window, cx);
        (self.spawn_host(cx) == host).then_some(self.workspace)
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
    }

    /// Arrow keys and Enter for the panel. These run ahead of the text input's
    /// own `MoveUp`/`MoveDown` bindings, so anything handled here must stop
    /// propagating or the cursor jumps inside the search box instead.
    fn on_switcher_key(
        &mut self,
        ev: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sw) = self.switcher.as_ref() else {
            return;
        };
        let (key, mods) = (ev.keystroke.key.as_str(), ev.keystroke.modifiers);
        if key == "escape" {
            cx.stop_propagation();
            self.close_switcher(window, cx);
            return;
        }
        let column = sw.column;
        let layout = self.switcher_layout(cx);
        match key_intent(key, mods) {
            // Escape already returned above; closing again is harmless and
            // beats a panic in the render path if that ever stops being true.
            Key::Close => self.close_switcher(window, cx),
            Key::Pass => {}
            Key::Step(forward) => {
                cx.stop_propagation();
                match column {
                    Column::Left => self.switcher_step_left(&layout, forward, window, cx),
                    Column::Right => self.switcher_step_right(&layout, forward, window, cx),
                }
            }
            // Once there is a query, left and right belong to the caret in the
            // search box; Tab is then the way across.
            Key::ToColumn(Column::Left) if column == Column::Right && sw.text(cx).is_empty() => {
                cx.stop_propagation();
                self.switcher_focus(Column::Left, cx);
            }
            Key::ToColumn(Column::Right) if column == Column::Left && sw.text(cx).is_empty() => {
                let has_tabs = layout
                    .subject_row(sw.left_sel)
                    .is_some_and(|r| !r.tabs.is_empty());
                if has_tabs {
                    cx.stop_propagation();
                    self.switcher_focus(Column::Right, cx);
                }
            }
            Key::ToColumn(_) => {}
            Key::Tab(forward) => {
                cx.stop_propagation();
                self.switcher_step_right(&layout, forward, window, cx);
            }
            Key::Confirm(new_window) => {
                cx.stop_propagation();
                self.switcher_confirm(&layout, new_window, window, cx);
            }
        }
    }

    /// The anchor a row wears while it is the selected one, so stepping the
    /// cursor with the keyboard carries the column to it.
    fn switcher_anchor(&self, column: Column, picked: bool) -> Option<gpui::ScrollAnchor> {
        let sw = self.switcher.as_ref()?;
        picked.then(|| match column {
            Column::Left => sw.left_anchor.clone(),
            Column::Right => sw.right_anchor.clone(),
        })
    }

    /// Moves the left cursor to a clicked row so the tab column follows it.
    /// Deliberately not wired to hover — the tab column swapping out from under
    /// the pointer on the way to somewhere else is noise, not a preview.
    fn switcher_point_at(&mut self, at: usize, cx: &mut Context<Self>) {
        let Some(sw) = self.switcher.as_mut() else {
            return;
        };
        if sw.left_sel == at && sw.column == Column::Left {
            return;
        }
        sw.left_sel = at;
        sw.right_sel = 0;
        sw.column = Column::Left;
        cx.notify();
    }

    /// Aims the tab cursor at one row, without acting on it.
    fn switcher_point_tab(&mut self, nth: usize, cx: &mut Context<Self>) {
        if let Some(sw) = self.switcher.as_mut() {
            sw.column = Column::Right;
            sw.right_sel = nth;
            cx.notify();
        }
    }

    fn switcher_focus(&mut self, column: Column, cx: &mut Context<Self>) {
        if let Some(sw) = self.switcher.as_mut() {
            sw.column = column;
        }
        cx.notify();
    }

    fn switcher_step_left(
        &mut self,
        layout: &Layout,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let n = layout.nav.len();
        let Some(sw) = self.switcher.as_mut() else {
            return;
        };
        if n == 0 {
            return;
        }
        sw.column = Column::Left;
        sw.left_sel = step(sw.left_sel.min(n - 1), n, forward);
        // A different workspace means a different tab column.
        sw.right_sel = 0;
        let anchor = sw.left_anchor.clone();
        anchor.scroll_to(window, cx);
        cx.notify();
    }

    fn switcher_step_right(
        &mut self,
        layout: &Layout,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sel = self.switcher.as_ref().map(|sw| sw.left_sel).unwrap_or(0);
        let query = self
            .switcher
            .as_ref()
            .map(|sw| sw.text(cx))
            .unwrap_or_default();
        let n = layout
            .subject_row(sel)
            .map(|row| visible_tabs(row, &query).len())
            .unwrap_or(0);
        let Some(sw) = self.switcher.as_mut() else {
            return;
        };
        if n == 0 {
            return;
        }
        sw.column = Column::Right;
        sw.right_sel = step(sw.right_sel.min(n - 1), n, forward);
        let anchor = sw.right_anchor.clone();
        anchor.scroll_to(window, cx);
        cx.notify();
    }

    fn switcher_confirm(
        &mut self,
        layout: &Layout,
        new_window: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sw) = self.switcher.as_ref() else {
            return;
        };
        let (sel, column, right_sel) = (sw.left_sel, sw.column, sw.right_sel);
        if column == Column::Right {
            let query = sw.text(cx);
            let Some(row) = layout.subject_row(sel) else {
                return;
            };
            let Some(tab) = visible_tabs(row, &query)
                .get(right_sel)
                .and_then(|i| row.tabs.get(*i))
            else {
                return;
            };
            let (ws, id) = (row.id, tab.id);
            self.switcher_open_tab(ws, id, new_window, window, cx);
            return;
        }
        if let Some(&(g, r)) = layout.nav.get(sel) {
            let group = &layout.groups[g];
            let row = RowRef::of(group, &group.rows[r]);
            self.switcher_open(row, new_window, window, cx);
        }
    }

    /// Normal activation stays here; only an explicit new-window action may reveal elsewhere.
    fn switcher_open_tab(
        &mut self,
        ws: WorkspaceId,
        tab: TabId,
        new_window: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_switcher(window, cx);
        if new_window {
            crate::ui::windows::open_at_tab(cx, ws, tab);
            return;
        }
        if ws == self.workspace {
            // Rendered positions are stale after reorder or close. A resident
            // miss is not hydration: do not park an activation for later.
            if let Some(index) = self.tabs.iter().position(|slot| slot.tree_id.get() == tab) {
                self.activate(index, window, cx);
            }
            return;
        }
        if self.switcher_window_conflict(ws, window, cx) {
            return;
        }
        self.switch_workspace(Some(ws), window, cx);
        self.activate_tree_tab(tab, window, cx);
    }

    pub(crate) fn render_switcher(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.switcher.as_ref()?;
        let card = self.render_list_card(window, cx);
        Some(
            manager_shell::overlay(manager_shell::layout(window), cx)
                .key_context("Switcher")
                .on_action(cx.listener(|this, _: &SwitcherAcross, window, cx| {
                    let layout = this.switcher_layout(cx);
                    this.switcher_step_right(&layout, true, window, cx);
                }))
                .on_action(cx.listener(|this, _: &SwitcherAcrossBack, window, cx| {
                    let layout = this.switcher_layout(cx);
                    this.switcher_step_right(&layout, false, window, cx);
                }))
                .on_key_down(cx.listener(Self::on_switcher_key))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.close_switcher(window, cx)
                    }),
                )
                .child(
                    div()
                        .id("switcher-card")
                        .occlude()
                        .on_hover(cx.listener(|this, hovered: &bool, _window, _cx| {
                            if let Some(sw) = this.switcher.as_mut() {
                                sw.hover_card = *hovered;
                            }
                        }))
                        .child(card),
                )
                .into_any_element(),
        )
    }

    fn render_list_card(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let sw = self.switcher.as_ref().expect("only rendered while up");
        let (sel, column) = (sw.left_sel, sw.column);
        let (left_scroll, right_scroll) = (sw.left_scroll.clone(), sw.right_scroll.clone());
        let layout = self.switcher_layout(cx);

        let theme = cx.theme();
        let border = theme.border;

        let mut list = v_flex().gap(px(1.));
        for (at, &(g, r)) in layout.nav.iter().enumerate() {
            let picked = sel == at && column == Column::Left;
            let group = &layout.groups[g];
            list = list.child(self.render_row(group, &group.rows[r], picked, Some(at), cx));
        }
        if layout.nav.is_empty() {
            list = list.child(
                div()
                    .px(px(ROW_PAD))
                    .py(px(14.))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t(L10nKey::SwitcherNoMatch)),
            );
        }
        // Orphan panes belong to the machine, not to a workspace, so they are
        // not rows and join no navigation — the bottom of the workspace list
        // is simply where a user looking for them finds them (#596). A search
        // narrows the panel to workspaces, and the box steps out of the way
        // for one.
        if !sw.orphans.is_empty() && sw.text(cx).is_empty() {
            list = list.child(self.render_orphan_panes(cx));
        }

        // Fixed height, not fit-to-content: the tab column changes length every
        // time the left cursor moves, and a card that resizes under the pointer
        // is unusable.
        // The card is fixed-size by design — a panel that resizes under the
        // pointer is unusable — but it still has to fit the window it floats
        // over. Below its natural size it takes what there is.
        let shell = manager_shell::layout(window);
        let body = div()
            .flex()
            .flex_row()
            .items_stretch()
            .h(px(shell.body_height))
            // Both columns scroll once the lists outrun the card, and neither
            // said so. The border moves out to the column so it stays put
            // while the rows underneath it move.
            .child(
                v_flex()
                    .w(px(shell.left_width))
                    .flex_shrink_0()
                    .border_r_1()
                    .border_color(border)
                    .child(crate::ui::scrollbar::with_vertical_scrollbar(
                        "switcher-workspaces-scrollbar",
                        div()
                            .id("switcher-workspaces")
                            .track_scroll(&left_scroll)
                            .size_full()
                            .overflow_y_scroll()
                            .p(px(6.))
                            .child(list),
                        &left_scroll,
                    )),
            )
            .child(
                v_flex()
                    .id("switcher-tab-column")
                    .flex_1()
                    .min_w_0()
                    .on_hover(cx.listener(|this, hovered: &bool, _window, _cx| {
                        if let Some(sw) = this.switcher.as_mut() {
                            sw.hover_tabs = *hovered;
                        }
                    }))
                    .child(crate::ui::scrollbar::with_vertical_scrollbar(
                        "switcher-tabs-scrollbar",
                        div()
                            .id("switcher-tabs")
                            .track_scroll(&right_scroll)
                            .size_full()
                            .overflow_y_scroll()
                            .p(px(6.))
                            .child(self.render_tabs(&layout, sel, column, cx)),
                        &right_scroll,
                    )),
            );

        manager_shell::card(shell.width, cx)
            .debug_selector(|| "machine-switcher-card".into())
            .max_h(px(shell.height))
            .child(self.render_search(cx))
            .child(body)
            .children(self.render_banners(&layout, cx))
            .child(self.render_footer(cx))
            .into_any_element()
    }

    fn render_search(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        manager_shell::search(
            &self.switcher.as_ref().expect("rendered while open").query,
            cx,
        )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let holding = self.switcher.as_ref().is_some_and(|sw| sw.hold.is_some());
        // With a query in the box, ← and → belong to the caret and Tab becomes
        // the way across. That remap is deliberate, and until now it was also
        // silent: arrows simply stopped working with nothing said.
        let filtering = self
            .switcher
            .as_ref()
            .is_some_and(|sw| !sw.text(cx).trim().is_empty());
        h_flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(border)
            .p(px(6.))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(6.))
                    .pr(px(ROW_PAD))
                    .text_xs()
                    .text_color(muted)
                    .when(!holding && filtering, |hint| {
                        hint.child(t(L10nKey::SwitcherTabToCrossColumns))
                    })
                    .when(!holding && !filtering, |hint| {
                        hint.child(
                            div()
                                .px(px(5.))
                                .py(px(1.))
                                .rounded(px(4.))
                                .border_1()
                                .border_color(border)
                                .child(crate::ui::keymap::secondary_glyph()),
                        )
                        .child(t(L10nKey::ClickForNewWindow))
                    })
                    .when(holding, |hint| hint.child(t(L10nKey::SwitcherHoldToSwitch))),
            )
    }

    /// The machine-trouble bands between the list and the footer: install
    /// progress, a failed connect with its retry, a parked route (#485), or a
    /// connect still in flight. With the per-machine headers gone this is
    /// where a machine's state gets to speak; a healthy machine says nothing
    /// here — its state dot rides on its workspace rows.
    fn render_banners(&self, layout: &Layout, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = Vec::new();
        for group in &layout.groups {
            if group.target.is_none() {
                continue;
            }
            if let Some(phase) = group.installing {
                out.push(
                    self.render_install_progress(&group.label, phase, cx)
                        .into_any_element(),
                );
            } else if group.parked
                && !self.parked_dismissed.contains(&group.key)
                && !group.rows.is_empty()
            {
                out.push(self.render_parked_notice(group, cx).into_any_element());
            } else if let Some(error) = group.error.clone() {
                out.push(self.render_error_band(group, &error, cx));
            } else if matches!(group.link, Link::Connecting | Link::Reconnecting { .. }) {
                let theme = cx.theme();
                out.push(
                    h_flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(12.))
                        .py(px(6.))
                        .border_t_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .flex_shrink_0()
                                .size(px(6.))
                                .rounded_full()
                                .bg(theme.warning),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(t_fmt(
                                    L10nKey::SwitcherConnectingTo,
                                    &[("machine", &group.label)],
                                )),
                        )
                        .into_any_element(),
                );
            }
        }
        out
    }

    fn render_error_band(&self, group: &Group, error: &str, cx: &mut Context<Self>) -> AnyElement {
        let retry = GroupRef::of(group);
        let replace = retry.clone();
        let retry_key = group.key.clone();
        let replace_key = group.key.clone();
        let dismiss_key = group.key.clone();
        let dismiss_target = group.target.clone();
        // The band no longer sits under a machine header, so plain errors
        // carry the machine's name themselves; the dialect restatement
        // already names it.
        let shown = remote_connect::dialect_complaint(error, &group.label)
            .unwrap_or_else(|| format!("{}: {error}", group.label));
        let replace_action = crate::ui::remote_workspace::mismatch_action_key(error);
        let theme = cx.theme();
        v_flex()
            .gap(px(4.))
            .px(px(12.))
            .py(px(8.))
            .border_t_1()
            .border_color(theme.danger.opacity(0.4))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(shown),
            )
            .child(
                h_flex()
                    .gap(px(4.))
                    .child(
                        Button::new(gpui::SharedString::from(format!(
                            "switcher-retry:{}",
                            group.key
                        )))
                        .label(t(L10nKey::TryAgain))
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(
                            move |this, _, _window, cx| {
                                this.remote_host_errors.remove(&retry_key);
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
                            },
                        )),
                    )
                    .when(
                        crate::daemon::control::is_dialect_refusal(error)
                            // Same gate as the workspace strip's: a machine
                            // whose server is not ours to install cannot be
                            // helped by this button, and a click that can only
                            // fail is worse than no button.
                            && replace
                                .target
                                .as_ref()
                                .is_some_and(|t| t.hosts_our_server()),
                        |row| {
                            row.child(
                                Button::new(gpui::SharedString::from(format!(
                                    "switcher-replace:{}",
                                    group.key
                                )))
                                .label(t(replace_action))
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.remote_host_errors.remove(&replace_key);
                                        if let Some(target) = replace.target.clone() {
                                            this.confirm_replace_remote_server(
                                                target,
                                                replace.label.clone(),
                                                replace_action,
                                                window,
                                                cx,
                                            );
                                        }
                                    },
                                )),
                            )
                        },
                    )
                    .child(
                        Button::new(gpui::SharedString::from(format!(
                            "switcher-dismiss:{}",
                            group.key
                        )))
                        .label(t(L10nKey::Dismiss))
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(
                            move |this, _, _window, cx| {
                                this.remote_host_errors.remove(&dismiss_key);
                                // The other half of this block can come from a
                                // failed connect. Retire that too, but only when
                                // it is this host's failure — a connect to
                                // anywhere else is still in flight.
                                if let Some(ConnectFlow::Failed { choice, .. }) = &this.connect
                                    && Some(&choice.target) == dismiss_target.as_ref()
                                {
                                    this.connect = None;
                                }
                                cx.notify();
                            },
                        )),
                    ),
            )
            .into_any_element()
    }

    /// The orphan block under the local group (#596): one line per pane no
    /// window holds — id, owner, where it runs — and the way to stop it.
    fn render_orphan_panes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let Some(switcher) = self.switcher.as_ref() else {
            return v_flex();
        };
        // Resolved up front, while `cx` can still be borrowed as an `App`: the
        // render loop below needs it mutably for the Close listener.
        let lines: Vec<(u64, String)> = switcher
            .orphans
            .iter()
            .map(|orphan| {
                let mut bits = vec![format!("%{}", orphan.pane_id)];
                if let Some(owner) = &orphan.owner {
                    bits.push(owner_label(owner, workspace_name_of(owner, cx)));
                }
                if let Some(cwd) = &orphan.cwd {
                    bits.push(cwd.clone());
                } else if !orphan.title.is_empty() {
                    bits.push(orphan.title.clone());
                }
                (orphan.pane_id, bits.join(" · "))
            })
            .collect();
        let mut list = v_flex().gap(px(2.));
        for (pane_id, line) in lines {
            list = list.child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.))
                    // The text takes the slack and gives it back: without
                    // `min_w_0` a flex child refuses to shrink below its
                    // content, and one long cwd pushed the Close button past
                    // the edge of the card — the row named a pane the user
                    // then had no way to stop.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme.foreground)
                            .child(line),
                    )
                    .child(
                        div().flex_shrink_0().child(
                            Button::new(gpui::SharedString::from(format!(
                                "switcher-close-orphan:{pane_id}"
                            )))
                            .label(t(L10nKey::Close))
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(
                                move |this, _, _window, cx| {
                                    this.close_orphan_pane(pane_id, cx);
                                },
                            )),
                        ),
                    ),
            );
        }
        v_flex()
            .gap(px(4.))
            .mx(px(4.))
            .mt(px(6.))
            .mb(px(2.))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t(L10nKey::SwitcherOrphanPanes)),
            )
            .child(list)
    }

    /// The parked group's notice (#485): no retry button — nothing it could
    /// try would succeed — just the way back (a fresh profile rediscovers
    /// the session) and the two honest actions.
    fn render_parked_notice(&self, group: &Group, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let forget_key = group.key.clone();
        let dismiss_key = group.key.clone();
        let dismiss_target = group.target.clone();
        let entries: Vec<WorkspaceId> = group.rows.iter().map(|r| r.id).collect();
        v_flex()
            .gap(px(4.))
            .px(px(12.))
            .py(px(8.))
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "{} — {}",
                        group.label,
                        t(L10nKey::RemoteRouteParkedHint)
                    )),
            )
            .child(
                h_flex()
                    .gap(px(4.))
                    .child(
                        Button::new(gpui::SharedString::from(format!(
                            "switcher-forget:{}",
                            group.key
                        )))
                        .label(t(L10nKey::RemoteActionRemoveEntry))
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(
                            move |this, _, _window, cx| {
                                // Forget, never delete: nothing is sent to the
                                // machine, so the remote sessions keep running
                                // (#485). Adopt rows name no local entry and are
                                // skipped by the store.
                                for id in &entries {
                                    crate::ui::windows::forget_workspace(cx, *id);
                                }
                                this.remote_host_errors.remove(&forget_key);
                                this.parked_dismissed.remove(&forget_key);
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        Button::new(gpui::SharedString::from(format!(
                            "switcher-parked-dismiss:{}",
                            group.key
                        )))
                        .label(t(L10nKey::Dismiss))
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(
                            move |this, _, _window, cx| {
                                this.parked_dismissed.insert(dismiss_key.clone());
                                // A parked group can still carry the deterministic
                                // failure that predated the parking — retire it
                                // too, or it pops right back as the error block.
                                this.remote_host_errors.remove(&dismiss_key);
                                if let Some(ConnectFlow::Failed { choice, .. }) = &this.connect
                                    && Some(&choice.target) == dismiss_target.as_ref()
                                {
                                    this.connect = None;
                                }
                                cx.notify();
                            },
                        )),
                    ),
            )
    }

    fn render_install_progress(
        &self,
        label: &str,
        phase: InstallPhase,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let caption = crate::ui::remote_workspace::install_phase_caption(phase);

        v_flex()
            .gap(px(6.))
            .px(px(12.))
            .py(px(8.))
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{label} — {caption}")),
            )
            .child(crate::ui::remote_workspace::install_progress_bar(phase, cx))
    }
    fn render_row(
        &self,
        group: &Group,
        row: &Row,
        picked: bool,
        nav_at: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, warn) = (theme.foreground, theme.muted_foreground, theme.warning);
        let sf = rungs(cx);
        let hover = gpui::rgb(sf.hover);
        let rref = RowRef::of(group, row);
        let click_ref = rref.clone();
        let menu_ref = rref.clone();
        let ctx_ref = rref.clone();
        let gref = GroupRef::of(group);
        let menu_host = gref.clone();
        let ctx_host = gref;
        let app = cx.entity().downgrade();
        let app2 = app.clone();
        let key = row.id.element_key() as usize;
        let holding = self.switcher.as_ref().is_some_and(|sw| sw.hold.is_some());
        // A machine with no link cannot show this row's panes right now; the
        // row stays, muted, and opening it is what asks for the connection.
        let unlit = group.target.is_some() && matches!(group.link, Link::Offline | Link::Failed);

        // "Open" is what this workspace would say either way, and it is the
        // wrong word for one another client is driving.
        let badge = if row.preempted {
            Some((t(L10nKey::SwitcherStatusTakenOver), false))
        } else if row.current {
            Some((t(L10nKey::SwitcherThisWindow), true))
        } else if row.open {
            Some((t(L10nKey::SwitcherOpen), false))
        } else {
            None
        };

        // Two lines rather than one: the left column is only LEFT_W wide, and a
        // workspace name plus path plus badge plus timestamp on one row pushes
        // the trailing pieces straight out over the divider. The second line
        // leads with the machine the workspace lives on — the flat list's only
        // grouping — with its link state as the dot's color.
        let host_dot: Option<gpui::Hsla> = match group.link {
            Link::Local => None,
            Link::Connected if group.preempted => Some(warn),
            Link::Connected => Some(gpui::rgb(crate::ui::tab_strip::LIVE_DOT).into()),
            Link::Connecting | Link::Reconnecting { .. } => Some(warn),
            Link::Failed => Some(theme.danger),
            Link::Offline => Some(gpui::rgb(crate::ui::tab_strip::UNKNOWN_DOT).into()),
        };
        let host_label = match group.target.is_some() {
            true => group.label.clone(),
            false => t(L10nKey::SwitcherLocalHost).to_string(),
        };

        let line = h_flex()
            .id(("switcher-row", key))
            .group("switcher-row")
            .items_center()
            .gap(px(8.))
            .min_h(px(ROW_H))
            .py(px(4.))
            .px(px(ROW_PAD))
            .rounded(px(6.))
            .overflow_hidden()
            .cursor_pointer()
            .when(picked, |r| r.bg(gpui::rgb(sf.cursor)))
            .anchor_scroll(self.switcher_anchor(Column::Left, picked))
            .hover(move |r| r.bg(hover))
            .child(crate::ui::tab_strip::workspace_avatar(
                &row.name, row.live, ROW_AVATAR, cx,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(1.))
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .when(row.current, |d| d.font_weight(gpui::FontWeight::MEDIUM))
                            .text_color(match unlit {
                                true => muted,
                                false => fg,
                            })
                            .child(row.name.clone()),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(5.))
                            .min_w_0()
                            .text_xs()
                            .text_color(muted)
                            .child(match host_dot {
                                Some(color) => div()
                                    .flex_shrink_0()
                                    .size(px(6.))
                                    .rounded_full()
                                    .bg(color)
                                    .into_any_element(),
                                None => gpui::svg()
                                    .path("icons/machine-local.svg")
                                    .flex_shrink_0()
                                    .size(px(10.))
                                    .text_color(muted)
                                    .into_any_element(),
                            })
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .max_w(px(120.))
                                    .truncate()
                                    .text_color(muted)
                                    .child(host_label),
                            )
                            // The path gives way first and the timestamp
                            // never does: with both in one truncating string
                            // the row ended in `~/repo/025/tty7 · …` every
                            // time, a dangling dot where the time had been.
                            .when(!row.path.is_empty(), |line| {
                                line.child(div().flex_shrink_0().child("·"))
                                    .child(div().min_w_0().truncate().child(row.path.clone()))
                            })
                            .when(!row.when.is_empty(), |line| {
                                line.child(div().flex_shrink_0().child("·"))
                                    .child(div().flex_shrink_0().child(row.when.clone()))
                            }),
                    ),
            )
            // A word, not a chip: a filled pill reads as a button, and these
            // are states. Only "taken over" keeps a colour — it is the one
            // that warns.
            .children(badge.map(|(label, _here)| {
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(match row.preempted {
                        true => warn,
                        false => muted,
                    })
                    .child(label)
            }))
            .child(
                div()
                    .invisible()
                    .flex_shrink_0()
                    .group_hover("switcher-row", |x| x.visible())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        crate::ui::tab_strip::hit_target(
                            Button::new(("switcher-row-more", key))
                                .icon(IconName::Ellipsis)
                                .ghost()
                                .xsmall(),
                        )
                        .tooltip(t(L10nKey::TabTooltipMore))
                        .dropdown_menu(move |menu, _window, cx| {
                            row_menu(menu, &menu_ref, &menu_host, app.clone(), cx)
                        }),
                    ),
            )
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                // One click aims the tab column at this workspace; opening it
                // takes a second click, Enter, or the platform modifier.
                if let Some(at) = nav_at {
                    this.switcher_point_at(at, cx);
                }
                let modified = ev.modifiers().secondary();
                if ev.click_count() >= 2 || modified {
                    this.switcher_open(click_ref.clone(), modified, window, cx);
                }
            }));

        // A held Ctrl makes every click a right click on macOS. Drop the menu
        // and take the right-button press as the pick instead, or clicking
        // during the gesture does nothing at all.
        match holding {
            true => line
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                        cx.stop_propagation();
                        if let Some(at) = nav_at {
                            this.switcher_point_at(at, cx);
                        }
                    }),
                )
                .into_any_element(),
            false => line
                .context_menu(move |menu, _window, cx| {
                    row_menu(menu, &ctx_ref, &ctx_host, app2.clone(), cx)
                })
                .into_any_element(),
        }
    }

    /// The right-hand column: the tabs of whichever workspace the left column
    /// is sitting on.
    fn render_tabs(
        &self,
        layout: &Layout,
        sel: usize,
        column: Column,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let added_ink =
            crate::ui::presets::resting_ink(theme.success, theme.muted_foreground, theme.popover);
        let removed_ink =
            crate::ui::presets::resting_ink(theme.danger, theme.muted_foreground, theme.popover);
        let note = |text: String| {
            div()
                .px(px(ROW_PAD))
                .py(px(14.))
                .text_sm()
                .text_color(muted)
                .child(text)
                .into_any_element()
        };

        let Some(row) = layout.subject_row(sel) else {
            return note(t(L10nKey::SwitcherPickAWorkspace).to_string());
        };
        if row.tabs.is_empty() {
            // A remote workspace this client has never opened has no tab
            // mirror to read — "no tabs" would be a claim nobody checked.
            let unseen = row.adopt.is_some() || (row.remote_id.is_some() && !row.open);
            return note(match unseen {
                true => t(L10nKey::SwitcherTabsAfterOpening).to_string(),
                false => t(L10nKey::SwitcherNoTabs).to_string(),
            });
        }

        let query = self
            .switcher
            .as_ref()
            .map(|sw| sw.text(cx))
            .unwrap_or_default();
        let hits = visible_tabs(row, &query);
        if hits.is_empty() {
            return note(t(L10nKey::SwitcherNoTabMatch).to_string());
        }

        let sf = rungs(cx);
        let (hover, picked_bg) = (gpui::rgb(sf.hover), gpui::rgb(sf.cursor));
        let right_sel = self.switcher.as_ref().map(|sw| sw.right_sel).unwrap_or(0);
        let holding = self.switcher.as_ref().is_some_and(|sw| sw.hold.is_some());
        let ws = row.id;

        let mut list = v_flex().gap(px(1.)).child(
            h_flex()
                .items_center()
                .gap(px(6.))
                .h(px(HOST_H))
                .px(px(ROW_PAD))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(muted)
                        .child(row.name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(match row.tabs.len() {
                            1 => t(L10nKey::SwitcherTabCountOne).to_string(),
                            n => t_fmt(L10nKey::SwitcherTabCount, &[("n", &n.to_string())]),
                        }),
                ),
        );

        for (nth, i) in hits.iter().enumerate() {
            let tab = &row.tabs[*i];
            let picked = nth == right_sel && column == Column::Right;
            let (id, index) = (tab.id, tab.index);
            // The second line is what tells two tabs on the same repo apart —
            // the branch, then the diff counts, mirroring the tab sidebar.
            let under = tab.git.as_ref().map(|g| {
                h_flex()
                    .items_center()
                    .gap(px(5.))
                    .text_xs()
                    .text_color(muted)
                    .child(
                        gpui::svg()
                            .path("icons/git-branch.svg")
                            .flex_shrink_0()
                            .size(px(11.))
                            .text_color(muted),
                    )
                    .child(div().min_w_0().truncate().child(g.branch.clone()))
                    .when(g.added > 0, |c| {
                        c.child(
                            div()
                                .flex_shrink_0()
                                .text_color(added_ink)
                                .child(format!("+{}", g.added)),
                        )
                    })
                    .when(g.removed > 0, |c| {
                        c.child(
                            div()
                                .flex_shrink_0()
                                .text_color(removed_ink)
                                .child(format!("−{}", g.removed)),
                        )
                    })
            });
            let subtitle = match under {
                Some(line) => Some(line.into_any_element()),
                None if tab.named && !tab.path.is_empty() => Some(
                    div()
                        .text_xs()
                        .truncate()
                        .text_color(muted)
                        .child(tab.path.clone())
                        .into_any_element(),
                ),
                None => None,
            };

            list = list.child(
                h_flex()
                    .id(("switcher-tab", index))
                    .items_center()
                    .gap(px(8.))
                    .min_h(px(ROW_H))
                    .py(px(4.))
                    .px(px(ROW_PAD))
                    .rounded(px(6.))
                    .overflow_hidden()
                    .cursor_pointer()
                    .when(picked, |r| r.bg(picked_bg))
                    .anchor_scroll(self.switcher_anchor(Column::Right, picked))
                    .hover(move |r| r.bg(hover))
                    .child(self.tab_avatar(
                        ("switcher-avatar", index),
                        tab.agent,
                        tab.status,
                        tab.unread,
                        tab.ssh,
                        picked,
                        ROW_AVATAR,
                        cx,
                    ))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(1.))
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .when(tab.active, |d| d.font_weight(gpui::FontWeight::MEDIUM))
                                    .text_color(fg)
                                    .child(tab.label.clone()),
                            )
                            .children(subtitle),
                    )
                    .children(
                        tab.recovery
                            .as_ref()
                            .map(|recovery| super::session_recovery::badge(recovery, cx)),
                    )
                    .when(tab.active, |r| {
                        r.child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(muted)
                                .child(t(L10nKey::SwitcherActiveTab)),
                        )
                    })
                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                        this.switcher_open_tab(ws, id, ev.modifiers().secondary(), window, cx)
                    }))
                    // Mid-gesture a click arrives as a right press on macOS. It
                    // aims the cursor; releasing Ctrl is what commits.
                    .when(holding, |line| {
                        line.on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                                cx.stop_propagation();
                                this.switcher_point_tab(nth, cx);
                            }),
                        )
                    }),
            );
        }
        list.into_any_element()
    }
}

/// Tab indices of `row` the search leaves visible. A workspace matched by its
/// own name keeps all of them — the search told you which workspace, not which
/// tab.
fn visible_tabs(row: &Row, query: &str) -> Vec<usize> {
    if query.is_empty()
        || row.name.to_lowercase().contains(query)
        || row.path.to_lowercase().contains(query)
    {
        return (0..row.tabs.len()).collect();
    }
    let hits: Vec<usize> = row
        .tabs
        .iter()
        .enumerate()
        .filter(|(_, t)| t.matches(query))
        .map(|(i, _)| i)
        .collect();
    match hits.is_empty() {
        // The host name matched, so the workspace is on screen with nothing of
        // its own to narrow by. Show the lot rather than an empty column.
        true => (0..row.tabs.len()).collect(),
        false => hits,
    }
}

impl Row {
    /// A workspace stays in the list when its own name or path matches, and
    /// also when any of its tabs does — searching "claude" should surface the
    /// workspaces running one.
    fn matches(&self, query: &str) -> bool {
        self.name.to_lowercase().contains(query)
            || self.path.to_lowercase().contains(query)
            || self.tabs.iter().any(|t| t.matches(query))
    }
}

impl TabRow {
    fn matches(&self, query: &str) -> bool {
        self.label.to_lowercase().contains(query) || self.path.to_lowercase().contains(query)
    }
}

/// Names a tab of a workspace this window does not own.
///
/// The two surfaces used to read different sources and had to be talked into
/// agreeing: a local tab was named by its live terminal's title, this one by
/// the tree's copy of it (`PaneRecord::osc_title`). They now go through the one
/// renderer, [`crate::ui::tab_strip::label_of`] — a local tab is turned into
/// the same [`TabView`](crate::ui::machine_mirror::TabView) this one already
/// is, so neither column can rank the evidence its own way.
fn tab_view_label(
    view: &crate::ui::machine_mirror::TabView,
    index: usize,
    home: Option<&std::path::Path>,
) -> String {
    crate::ui::tab_strip::label_of(view, index, home)
}

impl Group {
    fn history_carrier(&self, cx: &mut App) -> Option<&Row> {
        let host = self
            .target
            .as_ref()
            .map(RemoteTarget::host_id)
            .unwrap_or(crate::ui::host_ops::HostId::LOCAL);
        self.rows.iter().find(|row| {
            if row.open
                || row.preempted
                || super::remote_workspace::workspace_is_preempted(cx, row.id)
                || crate::ui::windows::WindowRegistry::window_for(cx, row.id).is_some()
            {
                return false;
            }
            if let Some(remote) = &row.adopt {
                if self.target.is_none() || row.remote_id != Some(remote.id) {
                    return false;
                }
                return super::remote_workspace::remote_workspace_claimable(
                    cx, host, remote.id, None,
                );
            }
            match WorkspaceStore::all(cx).get(row.id) {
                Some(view) => view.host_id() == host && !view.open,
                // CLI-created local workspaces may be known only to the
                // target mirror; absence from the client store alone is not proof.
                None => {
                    host.is_local()
                        && crate::ui::machine_mirror::unclaimed_local_workspaces(cx)
                            .iter()
                            .any(|workspace| workspace.id == row.id)
                }
            }
        })
    }
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
                last_active: r.last_active,
                live: Liveness::Stopped,
                open: false,
                current: false,
                preempted: false,
                adopt: Some(Box::new(r.clone())),
                remote_id: Some(r.id),
                // A workspace this client has never adopted has no local id to
                // hang a machine-tree lookup on. The tab column says so.
                tabs: Vec::new(),
            });
        }
    }
}

#[derive(Clone)]
struct GroupRef {
    label: String,
    target: Option<RemoteTarget>,
    link: Link,
}

impl GroupRef {
    fn of(g: &Group) -> Self {
        Self {
            label: g.label.clone(),
            target: g.target.clone(),
            link: g.link,
        }
    }
}

#[derive(Clone)]
struct RowRef {
    id: WorkspaceId,
    adopt: Option<(RemoteTarget, Box<RemoteWorkspaceRow>)>,
}

impl RowRef {
    fn of(group: &Group, row: &Row) -> Self {
        Self {
            id: row.id,
            adopt: match (&group.target, &row.adopt) {
                (Some(t), Some(r)) => Some((t.clone(), r.clone())),
                _ => None,
            },
        }
    }
}

/// What a host row offering the form says, or `None` for a machine that has no
/// SSH host behind it at all — WSL and the local stdio server are configured
/// nowhere this form could edit.
///
/// Shared with the tab menu, which offers the same row for the connection a tab
/// is on (#438), so the two surfaces cannot drift on which machines are
/// editable or on what the row is called.
pub(crate) fn host_form_label(target: &RemoteTarget) -> Option<&'static str> {
    match target {
        RemoteTarget::Profile { .. } => Some(t(L10nKey::SwitcherEditHost)),
        RemoteTarget::Alias { .. } | RemoteTarget::Direct { .. } => {
            Some(t(L10nKey::SwitcherSaveAsHost))
        }
        RemoteTarget::Wsl { .. } | RemoteTarget::LocalStdio { .. } => None,
    }
}

fn row_menu(
    menu: gpui_component::menu::PopupMenu,
    row: &RowRef,
    host: &GroupRef,
    app: gpui::WeakEntity<Tty7App>,
    cx: &App,
) -> gpui_component::menu::PopupMenu {
    let id = row.id;
    let open_app = app.clone();
    let menu = if row.adopt.is_none() {
        menu.item(
            PopupMenuItem::new(t(L10nKey::SwitcherOpenInNewWindow)).on_click(
                move |_, window, cx| {
                    let _ = open_app.update(cx, |this, cx| {
                        this.close_switcher(window, cx);
                        crate::ui::windows::open(cx, Some(id));
                    });
                },
            ),
        )
    } else {
        menu
    };
    host_menu(menu, host, app, cx)
}

/// The machine verbs that used to live on the group headers, now appended
/// under the machine's own name to every one of its rows. Local rows carry
/// none — the footer's New Workspace covers this computer.
fn host_menu(
    menu: gpui_component::menu::PopupMenu,
    host: &GroupRef,
    app: gpui::WeakEntity<Tty7App>,
    cx: &App,
) -> gpui_component::menu::PopupMenu {
    let Some(target) = host.target.clone() else {
        return menu;
    };
    let (a1, a2, a3, a4) = (app.clone(), app.clone(), app.clone(), app);
    let menu = menu
        .separator()
        .item(PopupMenuItem::label(host.label.clone()));
    let engaged = link_is_engaged(host.link);
    let pin_target = target.clone();
    let pinned = cx
        .global::<crate::core::config::Config>()
        .machine_rail
        .pinned
        .contains(&target);
    let menu = menu.item(
        PopupMenuItem::new(t(if pinned {
            L10nKey::MachineUnpin
        } else {
            L10nKey::MachinePin
        }))
        .on_click(move |_, _, cx| {
            let _ = a1.update(cx, |this, cx| {
                this.toggle_machine_pin(pin_target.clone(), cx);
            });
        }),
    );
    // The host as it is configured, reachable from the one place it is on
    // screen. A machine dialled by address has no profile yet, so the same
    // row offers to make one instead (#438).
    let menu = match host_form_label(&target) {
        Some(label) => {
            let for_edit = target.clone();
            menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let for_edit = for_edit.clone();
                let _ = a4.update(cx, |this, cx| {
                    this.close_switcher(window, cx);
                    this.edit_ssh_host_of_target(&for_edit, window, cx);
                });
            }))
        }
        None => menu,
    };
    let menu = match engaged {
        true => {
            let for_disconnect = target.clone();
            menu.item(PopupMenuItem::new(t(L10nKey::SwitcherDisconnect)).on_click(
                move |_, _window, cx| {
                    let _ = a2.update(cx, |this, cx| this.switcher_disconnect(&for_disconnect, cx));
                },
            ))
        }
        false => {
            let connect_choice = HostChoice {
                target: target.clone(),
                label: host.label.clone(),
                detail: String::new(),
            };
            menu.item(
                PopupMenuItem::new(t(L10nKey::Connect))
                    .disabled(matches!(host.link, Link::Connecting))
                    .on_click(move |_, _window, cx| {
                        let _ = a2.update(cx, |this, cx| {
                            this.connect_to_host(connect_choice.clone(), cx)
                        });
                    }),
            )
        }
    };
    if !target.hosts_our_server() {
        return menu;
    }
    let (label, for_restart) = (host.label.clone(), target);
    menu.item(
        PopupMenuItem::new(t(L10nKey::AppMenuRestartServer)).on_click(move |_, window, cx| {
            let _ = a3.update(cx, |this, cx| {
                this.confirm_restart_remote_server(for_restart.clone(), label.clone(), window, cx);
            });
        }),
    )
}

fn rungs(cx: &App) -> crate::ui::presets::Surface {
    cx.global::<crate::ui::presets::Surfaces>().popover
}

/// What the panel wants to do with a keystroke, before any of the state that
/// only the panel knows (which column has the cursor, whether the search box
/// has text) gets a say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Close,
    Step(bool),
    ToColumn(Column),
    Tab(bool),
    Confirm(bool),
    Pass,
}

fn key_intent(key: &str, mods: gpui::Modifiers) -> Key {
    if key == "escape" {
        return Key::Close;
    }
    // Alt and Fn belong to whoever else wants them, whatever the key.
    if mods.alt || mods.function {
        return Key::Pass;
    }
    // Careful: `control` cannot be lumped in with "some modifier is down", as
    // it *is* the secondary modifier off macOS — Ctrl+Enter has to reach the
    // new-window branch there, not fall through as a stray chord.
    let bare = !mods.control && !mods.secondary();
    match key {
        "up" | "down" if bare => Key::Step(key == "down"),
        "left" if bare => Key::ToColumn(Column::Left),
        "right" if bare => Key::ToColumn(Column::Right),
        // Tab keeps working with Ctrl held — that is the Ctrl+Tab gesture still
        // in progress. Off macOS that chord arrives as the NextTab action
        // instead, which lands in the same place.
        "tab" if !mods.secondary() => Key::Tab(!mods.shift),
        "enter" if bare || mods.secondary() => Key::Confirm(mods.secondary()),
        _ => Key::Pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn machine_identity_never_uses_repo_or_cwd(cx: &mut gpui::TestAppContext) {
        use crate::core::session::{RemoteRef, WindowView, WindowViews};
        use tty7_core::core::machine::{Machine, PaneRecord, Tab, Workspace};
        let (app, _vcx) = crate::ui::app::test_window::harness(cx);
        app.update(cx, |app, cx| {
            let target = RemoteTarget::direct("root", "machine.test", 22);
            let ws = WorkspaceId::new();
            let remote = WindowView::on_remote(RemoteRef::new(target.clone(), ws));
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![remote],
                    active: None,
                },
            );
            let mut expected = None;
            for cwd in [
                "/root",
                "/root/project/subdir",
                "/srv/different-repository",
                "/tmp",
            ] {
                super::super::machine_mirror::MachineMirrors::install(
                    cx,
                    target.host_id(),
                    Machine {
                        workspaces: vec![Workspace {
                            id: ws,
                            tabs: vec![Tab::leaf(1)],
                            ..Workspace::default()
                        }],
                        panes: vec![PaneRecord {
                            cwd: Some(cwd.into()),
                            title: cwd.into(),
                            live: true,
                            ..PaneRecord::new(1)
                        }],
                    },
                );
                let groups = app.switcher_groups(cx);
                let machine = groups
                    .iter()
                    .find(|g| g.target.as_ref() == Some(&target))
                    .unwrap();
                let identity = (
                    machine.key.clone(),
                    machine.label.clone(),
                    machine.target.clone(),
                );
                if let Some(previous) = &expected {
                    assert_eq!(&identity, previous);
                } else {
                    expected = Some(identity);
                }
                assert_eq!(
                    groups
                        .iter()
                        .filter(|g| g.target.as_ref() == Some(&target))
                        .count(),
                    1
                );
                assert!(
                    machine
                        .rows
                        .iter()
                        .flat_map(|r| &r.tabs)
                        .any(|t| t.path.contains(cwd)),
                    "session metadata must track cwd even though the machine identity does not"
                );
            }
        });
    }

    #[gpui::test]
    fn machine_rail_has_one_entry_per_host(cx: &mut gpui::TestAppContext) {
        let (app, _vcx) = crate::ui::app::test_window::harness(cx);
        let target = RemoteTarget::direct("dev", "example.test", 22);
        app.update(cx, |app, cx| {
            let before = app.tabs.len();
            app.toggle_machine_pin(target.clone(), cx);
            assert!(app.connect.is_none(), "pin must not start a connect flow");
            assert!(RemoteLinks::machine_status(cx, target.host_id()).is_none());
            assert!(remote_connect::HostLinks::get(cx, target.host_id()).is_none());
            let groups = app.switcher_groups(cx);
            assert!(
                groups[0].target.is_none(),
                "Local remains first even without tabs"
            );
            let pinned = groups
                .iter()
                .find(|g| g.target.as_ref() == Some(&target))
                .unwrap();
            assert_eq!(pinned.link, Link::Offline);
            assert!(pinned.rows.is_empty());
            app.host_snapshots.insert(
                target.host_id(),
                HostSnapshot {
                    target: target.clone(),
                    rows: vec![],
                },
            );
            let groups = app.switcher_groups(cx);
            assert_eq!(
                groups
                    .iter()
                    .filter(|g| g.target.as_ref() == Some(&target))
                    .count(),
                1
            );
            let weak = cx.entity().downgrade();
            assert_eq!(app.machine_launcher_items(weak, cx).len(), groups.len());
            app.toggle_machine_fold(target.to_string(), cx);
            assert_eq!(
                app.tabs.len(),
                before,
                "fold must not close or create panes"
            );
            assert!(app.connect.is_none());
            app.toggle_machine_pin(target.clone(), cx);
            assert!(
                !cx.global::<crate::core::config::Config>()
                    .machine_rail
                    .pinned
                    .contains(&target)
            );
            assert!(RemoteLinks::machine_status(cx, target.host_id()).is_none());
        });
    }

    #[test]
    fn machine_connecting_is_explicitly_disconnectable() {
        assert!(link_is_engaged(Link::Connecting));
        assert!(link_is_engaged(Link::Reconnecting { attempt: 1 }));
        assert!(!link_is_engaged(Link::Offline));
        assert!(!link_is_engaged(Link::Local));
    }

    /// #485 on the path #645 did not cover. A machine the switcher knows only
    /// from a listing snapshot has no store entry to name it, so its group
    /// used to be labelled by the target's own spelling — and a `Profile`
    /// target spells itself as its config UUID. Delete the profile and every
    /// banner under the list announced a raw UUID.
    #[gpui::test]
    fn a_pending_machine_whose_profile_is_gone_is_not_named_by_its_uuid(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::core::session::RemoteTarget;

        let (app, _vcx) = crate::ui::app::test_window::harness(cx);

        // A profile id that is in no config: the state left behind when the
        // profile a machine was reached through is deleted.
        let id = uuid::Uuid::new_v4();
        let target = RemoteTarget::Profile { id };

        app.update(cx, |app, _| {
            app.host_snapshots.insert(
                target.host_id(),
                super::HostSnapshot {
                    target: target.clone(),
                    rows: Vec::new(),
                },
            );
        });

        app.update(cx, |app, cx| {
            let groups = app.switcher_groups(cx);
            let group = groups
                .iter()
                .find(|g| g.target.as_ref() == Some(&target))
                .expect("the snapshot puts its machine in the list");
            assert!(
                !group.label.contains(&id.to_string()),
                "the switcher named a machine by its raw profile UUID: {}",
                group.label
            );
            assert_eq!(
                group.label,
                t(L10nKey::RemoteProfileGone),
                "a gone profile is named here the way a pane's route names it"
            );
        });
    }

    /// A wrong hostname or a stale password used to be fixable only by
    /// finding the same machine again in Settings (#438). The machine is on
    /// screen here, so its host row is too — worded for what the row can
    /// actually do, since a machine reached by address has no profile to open.
    #[test]
    fn every_ssh_machine_offers_its_host_form_and_nothing_else_does() {
        crate::ui::i18n::set_locale("en");
        assert_eq!(
            host_form_label(&RemoteTarget::Profile {
                id: uuid::Uuid::new_v4()
            }),
            Some("Edit Host…")
        );
        assert_eq!(
            host_form_label(&RemoteTarget::Alias {
                alias: "prod".into()
            }),
            Some("Save as SSH Host…"),
            "an alias lives in ~/.ssh/config, which this form does not write"
        );
        assert_eq!(
            host_form_label(&RemoteTarget::direct("me", "10.0.0.5", 22)),
            Some("Save as SSH Host…")
        );
        assert_eq!(
            host_form_label(&RemoteTarget::Wsl {
                distro: "Ubuntu".into()
            }),
            None,
            "a WSL distro is configured nowhere this form could reach"
        );
        assert_eq!(
            host_form_label(&RemoteTarget::LocalStdio {
                program: "tty7-server".into(),
                args: Vec::new()
            }),
            None
        );
    }

    /// The switcher used to read the `HostLinks` table and nothing else, which
    /// cannot tell "being retried right now" from "never heard of". The group
    /// then collapsed mid-reconnect as if the machine had no workspaces.
    #[test]
    fn a_machine_being_retried_does_not_read_as_one_nobody_ever_connected_to() {
        let target = RemoteTarget::direct("me", "build-box", 22);
        let retrying = MachineStatus::Reconnecting {
            attempt: 2,
            last_error: Some("connection refused".into()),
        };

        assert_eq!(
            link_from(None, &target, Some(&retrying), false),
            Link::Reconnecting { attempt: 2 }
        );
        assert_eq!(link_from(None, &target, None, false), Link::Offline);
        assert_eq!(
            link_from(None, &target, Some(&MachineStatus::Attached), false),
            Link::Connected
        );
        assert_eq!(
            link_from(
                None,
                &target,
                Some(&MachineStatus::Failed("no route to host".into())),
                false
            ),
            Link::Failed,
            "a route the supervisor could not build has to reach the panel too"
        );
        // A link this window knows nothing about is still a link.
        assert_eq!(link_from(None, &target, None, true), Link::Connected);
    }

    #[test]
    fn this_windows_own_attempt_is_only_ever_about_its_own_machine() {
        let build = RemoteTarget::direct("me", "build-box", 22);
        let gpu = RemoteTarget::direct("me", "gpu-lab", 22);
        let flow = ConnectFlow::Connecting {
            choice: HostChoice {
                target: gpu,
                label: "gpu-lab".into(),
                detail: String::new(),
            },
        };

        assert_eq!(
            link_from(Some(&flow), &build, Some(&MachineStatus::Attached), true),
            Link::Connected,
            "a connect to the GPU box says nothing about the build box"
        );
    }

    fn tab(label: &str, path: &str) -> TabRow {
        TabRow {
            recovery: None,
            id: TabId::new(),
            index: 0,
            label: label.to_string(),
            path: path.to_string(),
            named: false,
            agent: None,
            status: None,
            unread: 0,
            ssh: None,
            active: false,
            git: None,
        }
    }

    fn row(name: &str, tabs: Vec<TabRow>) -> Row {
        Row {
            id: WorkspaceId::new(),
            name: name.to_string(),
            path: "~/code".to_string(),
            when: String::new(),
            last_active: 0,
            live: Liveness::Alive,
            open: true,
            current: false,
            preempted: false,
            adopt: None,
            remote_id: None,
            tabs,
        }
    }

    fn aged(mut r: Row, last_active: u64) -> Row {
        r.last_active = last_active;
        r
    }

    fn group(rows: Vec<Row>) -> Group {
        Group {
            key: String::new(),
            label: "This Computer".to_string(),
            endpoint: String::new(),
            target: None,
            link: Link::Local,
            error: None,
            installing: None,
            preempted: false,
            parked: false,
            rows,
        }
    }

    fn named_group(label: &str, rows: Vec<Row>) -> Group {
        Group {
            key: label.to_lowercase(),
            label: label.to_string(),
            target: Some(RemoteTarget::direct("me", label, 22)),
            link: Link::Offline,
            ..group(rows)
        }
    }

    #[gpui::test]
    fn history_carrier_requires_resolved_host_and_unowned_window(cx: &mut gpui::TestAppContext) {
        use crate::core::session::{RemoteRef, WindowView, WindowViews};
        use gpui::VisualContext as _;
        let (app, vcx) = crate::ui::app::test_window::harness(cx);
        let handle = vcx.window_handle();
        let weak = app.downgrade();
        cx.update(|cx| {
            crate::ui::windows::WindowRegistry::init(cx);
            let mut candidate = row("candidate", vec![]);
            candidate.open = false;
            let id = candidate.id;
            let mut group = group(vec![candidate]);
            WorkspaceStore::install_for_test(cx, WindowViews::default());
            assert!(
                group.history_carrier(cx).is_none(),
                "unknown identity is not Local"
            );
            let local = WindowView {
                id,
                open: false,
                ..WindowView::default()
            };
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![local],
                    active: None,
                },
            );
            assert_eq!(group.history_carrier(cx).unwrap().id, id);
            group.rows[0].preempted = true;
            assert!(group.history_carrier(cx).is_none());
            group.rows[0].preempted = false;
            let target = RemoteTarget::direct("dev", "ownership.test", 22);
            let remote_id = WorkspaceId::new();
            let remote = WindowView {
                id,
                open: false,
                ..WindowView::on_remote(RemoteRef::new(target.clone(), remote_id))
            };
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![remote],
                    active: None,
                },
            );
            assert!(
                group.history_carrier(cx).is_none(),
                "Local cannot claim a remote mapping"
            );
            group.target = Some(target);
            assert_eq!(group.history_carrier(cx).unwrap().id, id);
            crate::ui::windows::WindowRegistry::register(cx, id, handle, weak);
            assert!(
                group.history_carrier(cx).is_none(),
                "window ownership outranks stale open=false"
            );
            group.rows[0].id = remote_id;
            group.rows[0].remote_id = Some(remote_id);
            group.rows[0].adopt = Some(Box::new(RemoteWorkspaceRow {
                id: remote_id,
                name: "stale listing".into(),
                panes: 0,
                last_active: 0,
            }));
            assert!(
                group.history_carrier(cx).is_none(),
                "adoptable listing cannot bypass an occupied client mapping"
            );
        });
    }

    #[test]
    fn orphan_panes_of_keeps_only_live_panes_no_workspace_holds() {
        use tty7_core::daemon::protocol::PaneInfo;

        fn info(pane_id: u64, alive: bool) -> PaneInfo {
            PaneInfo {
                pane_id,
                cwd: Some(std::path::PathBuf::from("/tmp/x")),
                title: "zsh".to_string(),
                osc_title: None,
                alive,
                owner: Some("tty7-cli".to_string()),
            }
        }

        let held: HashSet<u64> = [2].into_iter().collect();
        let orphans = orphan_panes_of(vec![info(1, true), info(2, true), info(3, false)], &held);
        assert_eq!(
            orphans,
            vec![OrphanPane {
                pane_id: 1,
                title: "zsh".to_string(),
                cwd: Some("/tmp/x".to_string()),
                owner: Some("tty7-cli".to_string()),
            }],
            "%2 is held by a workspace and %3 is dead — neither is a leak to reap (#596)"
        );
    }

    #[test]
    fn an_orphans_owner_reads_as_a_workspace_never_as_a_whole_uuid() {
        let uuid = "e0d7bebd-2e46-4a8e-abbb-f2f109a9b61d";
        assert_eq!(
            owner_label(uuid, Some("seeg".to_string())),
            "seeg",
            "the workspace is still here, so say which one it is"
        );
        assert_eq!(
            owner_label(uuid, None),
            "e0d7bebd",
            "no workspace left to name: the short id `pane ls` prints, not all 36 \
             characters — the full one shoved the Close button off the card"
        );
        assert_eq!(
            owner_label("tty7-cli", None),
            "tty7-cli",
            "an older client's own label is not an id and reads fine whole"
        );
    }

    #[test]
    fn a_workspace_stays_in_the_list_when_only_one_of_its_tabs_matches() {
        let ws = row(
            "notes",
            vec![tab("zsh", "~/notes"), tab("claude", "~/notes")],
        );
        assert!(ws.matches("claude"));
        assert!(!ws.matches("codex"));
    }

    #[test]
    fn searching_a_tab_name_narrows_the_tab_column_to_the_hits() {
        let ws = row("notes", vec![tab("zsh", "~/a"), tab("claude", "~/b")]);
        assert_eq!(visible_tabs(&ws, "claude"), vec![1]);
    }

    #[test]
    fn searching_the_workspace_name_keeps_all_of_its_tabs() {
        let ws = row("notes", vec![tab("zsh", "~/a"), tab("claude", "~/b")]);
        assert_eq!(visible_tabs(&ws, "notes"), vec![0, 1]);
    }

    #[test]
    fn a_workspace_shown_only_because_its_host_matched_keeps_all_its_tabs() {
        // Nothing about the workspace or its tabs matched — the group header
        // did. An empty column here would read as "this workspace is empty".
        let ws = row("notes", vec![tab("zsh", "~/a")]);
        assert_eq!(visible_tabs(&ws, "dev-box"), vec![0]);
    }

    #[test]
    fn the_flat_list_is_most_recently_used_first_across_machines() {
        // Two machines, interleaved activity: the list must interleave too —
        // grouping by machine is exactly what this design retired.
        let groups = vec![
            group(vec![
                aged(row("old-local", vec![]), 10),
                aged(row("fresh-local", vec![]), 40),
            ]),
            named_group("devbox", vec![aged(row("remote", vec![]), 30)]),
        ];
        let names: Vec<&str> = flatten(&groups, "")
            .into_iter()
            .map(|(g, r)| groups[g].rows[r].name.as_str())
            .collect();
        assert_eq!(names, vec!["fresh-local", "remote", "old-local"]);
    }

    #[test]
    fn this_windows_workspace_outranks_everything_however_stale() {
        let mut here = aged(row("here", vec![]), 1);
        here.current = true;
        let groups = vec![group(vec![aged(row("busy", vec![]), 99), here])];
        let first = flatten(&groups, "")[0];
        assert_eq!(groups[first.0].rows[first.1].name, "here");
    }

    #[test]
    fn a_query_matching_a_machines_name_keeps_all_of_its_rows() {
        // Searching "devbox" is how the retired per-machine grouping is asked
        // for now, so a host-name hit must surface every row of that machine
        // and nothing of the others.
        let groups = vec![
            group(vec![row("local-notes", vec![])]),
            named_group("devbox", vec![row("api", vec![]), row("web", vec![])]),
        ];
        let hits = flatten(&groups, "devbox");
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|&(g, _)| g == 1));
    }

    #[test]
    fn the_tab_column_follows_the_flat_cursor() {
        let layout = Layout {
            groups: vec![group(vec![row("a", vec![]), row("b", vec![])])],
            nav: vec![(0, 1), (0, 0)],
        };
        assert_eq!(layout.subject(0), Some((0, 1)));
        assert_eq!(layout.subject(1), Some((0, 0)));
        assert_eq!(layout.subject(2), None, "past the end is nothing");
    }

    #[test]
    fn the_cursor_wraps_at_both_ends() {
        assert_eq!(step(0, 3, true), 1);
        assert_eq!(step(2, 3, true), 0);
        assert_eq!(step(0, 3, false), 2);
    }

    #[test]
    fn a_tab_of_another_window_is_named_the_way_a_local_one_would_be() {
        // `title` here is the foreground process name the machine tree carries,
        // not a terminal title — it must not outrank the working directory the
        // local tab strip would be showing.
        let mut view = crate::ui::machine_mirror::TabView {
            id: TabId::new(),
            name: Some("  build  ".to_string()),
            title: "zsh".to_string(),
            osc_title: Some("✳ 修复 workspace switcher".to_string()),
            provider_title: None,
            cwd: Some("/Users/x/repo/tty7".to_string()),
            agent: Some(crate::core::cli_agent::CLIAgent::Claude),
            status: None,
            live: true,
            panes: 1,
            recovery: None,
        };
        assert_eq!(tab_view_label(&view, 0, None), "build", "a given name wins");

        view.name = None;
        assert_eq!(
            tab_view_label(&view, 0, None),
            "✳ 修复 workspace switcher",
            "then the title the local strip would be showing, verbatim"
        );

        view.osc_title = Some("user@host:~/repo/025/tty7".to_string());
        assert_eq!(
            tab_view_label(&view, 0, None),
            crate::ui::tab_strip::short_title("user@host:~/repo/025/tty7", None),
            "a shell's title goes through the shortener the strip uses"
        );

        view.osc_title = Some("user@host:".to_string());
        assert_eq!(
            tab_view_label(&view, 0, None),
            "zsh",
            "a title that shortens away to nothing falls through"
        );

        view.osc_title = None;
        assert_eq!(
            tab_view_label(&view, 0, None),
            "Claude Code",
            "an agent names a tab that has told us nothing else"
        );

        view.agent = None;
        assert_eq!(
            tab_view_label(&view, 0, None),
            crate::ui::tab_strip::short_title("/Users/x/repo/tty7", None),
            "otherwise the directory, put through the same shortener as the strip"
        );

        view.cwd = None;
        assert_eq!(
            tab_view_label(&view, 0, None),
            "zsh",
            "process name is last"
        );

        view.title = String::new();
        assert!(tab_view_label(&view, 2, None).contains('3'));
    }
}

#[cfg(test)]
mod gpui_tests {
    use gpui::{Modifiers, TestAppContext};

    use super::Column;
    use crate::ui::app::test_window::harness_with_tabs;

    #[gpui::test]
    fn stale_remote_listing_cannot_claim_another_windows_mapping(cx: &mut TestAppContext) {
        use crate::core::session::{
            RemoteRef, RemoteTarget, WindowView, WindowViews, WorkspaceStore,
        };
        use crate::ui::remote_connect::RemoteWorkspaceRow;
        use crate::ui::windows::WindowRegistry;
        use gpui::VisualContext as _;
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 2);
        let (other, other_cx, _other_streams) = harness_with_tabs(cx, 2);
        let other_ws = other.read_with(cx, |app, _| app.workspace);
        let target = RemoteTarget::Profile {
            id: uuid::Uuid::new_v4(),
        };
        let remote_ws = crate::core::session::WorkspaceId::new();
        cx.update(|cx| {
            WindowRegistry::init(cx);
            WindowRegistry::register(cx, other_ws, other_cx.window_handle(), other.downgrade());
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![WindowView {
                        id: other_ws,
                        open: false,
                        host: Some(RemoteRef::new(target.clone(), remote_ws)),
                        ..Default::default()
                    }],
                    active: None,
                },
            );
        });
        app.update_in(&mut vcx, |app, window, cx| {
            let original = app.workspace;
            let original_tabs: Vec<_> = app.tabs.iter().map(|tab| tab.tree_id.get()).collect();
            let entered = app.switcher_open(
                super::RowRef {
                    id: remote_ws,
                    adopt: Some((
                        target.clone(),
                        Box::new(RemoteWorkspaceRow {
                            id: remote_ws,
                            name: "stale listing".into(),
                            panes: 2,
                            last_active: 0,
                        }),
                    )),
                },
                false,
                window,
                cx,
            );
            assert!(
                !entered,
                "rejected adoption authorized deferred tab activation"
            );
            assert_eq!(
                app.workspace, original,
                "stale listing changed the current machine"
            );
            assert_eq!(
                app.tabs
                    .iter()
                    .map(|tab| tab.tree_id.get())
                    .collect::<Vec<_>>(),
                original_tabs
            );
            assert_eq!(
                WorkspaceStore::all(cx).views.len(),
                1,
                "rejection created a client mapping"
            );
            use crate::ui::remote_workspace::remote_workspace_claimable;
            WindowRegistry::unregister(cx, other_ws);
            assert!(remote_workspace_claimable(
                cx,
                target.host_id(),
                remote_ws,
                None
            ));
            let mut views = WorkspaceStore::all(cx).clone();
            views.views[0].open = true;
            WorkspaceStore::install_for_test(cx, views.clone());
            assert!(!remote_workspace_claimable(
                cx,
                target.host_id(),
                remote_ws,
                None
            ));
            assert!(remote_workspace_claimable(
                cx,
                target.host_id(),
                remote_ws,
                Some(other_ws)
            ));
            let mut duplicate = views.views[0].clone();
            duplicate.id = crate::core::session::WorkspaceId::new();
            duplicate.open = false;
            views.views.push(duplicate);
            WorkspaceStore::install_for_test(cx, views);
            assert!(
                !remote_workspace_claimable(cx, target.host_id(), remote_ws, Some(other_ws)),
                "duplicate target mappings are ambiguous"
            );
        });
    }

    #[gpui::test]
    fn normal_session_click_does_not_change_another_window(cx: &mut TestAppContext) {
        use crate::ui::windows::WindowRegistry;
        use gpui::VisualContext as _;
        let (first, mut first_cx, _first_streams) = harness_with_tabs(cx, 2);
        let (other, mut other_cx, _other_streams) = harness_with_tabs(cx, 2);
        let first_handle = first_cx.window_handle();
        let other_handle = other_cx.window_handle();
        let first_weak = first.downgrade();
        let other_weak = other.downgrade();
        let first_ws = first.read_with(cx, |app, _| app.workspace);
        let (other_ws, target) =
            other.read_with(cx, |app, _| (app.workspace, app.tabs[1].tree_id.get()));
        cx.update(|cx| {
            WindowRegistry::init(cx);
            WindowRegistry::register(cx, first_ws, first_handle, first_weak);
            WindowRegistry::register(cx, other_ws, other_handle, other_weak);
        });
        other.update_in(&mut other_cx, |app, _, _| app.active = 0);
        first.update_in(&mut first_cx, |app, window, cx| {
            app.switcher_open_tab(other_ws, target, false, window, cx);
            assert_eq!(app.workspace, first_ws);
            assert_eq!(app.tabs.len(), 2);
        });
        other.update_in(&mut other_cx, |app, _, _| {
            assert_eq!(
                app.active, 0,
                "normal click changed the foreign window's selection"
            );
            assert_eq!(app.workspace, other_ws);
        });
        first.update_in(&mut first_cx, |app, window, cx| {
            app.switcher_open(
                super::RowRef {
                    id: other_ws,
                    adopt: None,
                },
                false,
                window,
                cx,
            );
            assert_eq!(app.workspace, first_ws);
            app.switcher_open_tab(other_ws, target, true, window, cx);
        });
        other.update_in(&mut other_cx, |app, _, _| {
            assert_eq!(
                app.tabs[app.active].tree_id.get(),
                target,
                "explicit new-window action retains its existing behavior"
            );
        });
    }

    #[gpui::test]
    fn resident_selection_keeps_identity_after_reorder_and_removal(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        app.update_in(&mut vcx, |app, window, cx| {
            let target = app.tabs[0].tree_id.get();
            app.tabs.swap(0, 2);
            let order: Vec<_> = app.tabs.iter().map(|tab| tab.tree_id.get()).collect();
            app.switcher_open_tab(app.workspace, target, false, window, cx);
            assert_eq!(app.tabs[app.active].tree_id.get(), target);
            assert_eq!(
                app.tabs
                    .iter()
                    .map(|tab| tab.tree_id.get())
                    .collect::<Vec<_>>(),
                order
            );
            // A stale click after removal must not focus the old index's neighbor
            // or park a request which could unexpectedly fire during hydration.
            let removed = app.tabs.remove(2);
            app.active = 1;
            app.switcher_open_tab(app.workspace, target, false, window, cx);
            assert_eq!(app.active, 1);
            assert_eq!(app.tabs.len(), 2);
            drop(removed);
        });
    }

    #[gpui::test]
    fn ctrl_tab_raises_the_panel_on_the_previously_used_tab(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        vcx.simulate_modifiers_change(Modifiers::control());

        app.update_in(&mut vcx, |app, window, cx| app.tab_switch(true, window, cx));

        app.update(cx, |app, _| {
            let sw = app.switcher.as_ref().expect("Ctrl+Tab raises the panel");
            assert_eq!(sw.column, Column::Right, "the tab column takes the cursor");
            assert_eq!(sw.right_sel, 1, "the cursor lands on the previous tab");
            assert!(
                sw.mru,
                "Ctrl+Tab orders the column most-recently-used first"
            );
            assert!(sw.hold.is_some(), "the held modifier is what commits later");
        });
    }

    #[gpui::test]
    fn holding_ctrl_and_pressing_tab_again_walks_further_down(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        vcx.simulate_modifiers_change(Modifiers::control());

        app.update_in(&mut vcx, |app, window, cx| {
            app.tab_switch(true, window, cx);
            app.tab_switch(true, window, cx);
        });

        app.update(cx, |app, _| {
            assert_eq!(app.switcher.as_ref().expect("still up").right_sel, 2);
            assert_eq!(app.active, 0, "nothing is committed while the key is held");
        });
    }

    #[gpui::test]
    fn releasing_ctrl_commits_the_highlighted_tab_and_closes_the_panel(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        vcx.simulate_modifiers_change(Modifiers::control());
        app.update_in(&mut vcx, |app, window, cx| app.tab_switch(true, window, cx));

        vcx.simulate_modifiers_change(Modifiers::none());

        app.update(cx, |app, _| {
            assert!(app.switcher.is_none(), "the panel comes down on release");
            assert_eq!(app.active, 1, "the highlighted tab is now the active one");
        });
    }

    #[gpui::test]
    fn a_second_ctrl_tab_goes_back_to_where_it_came_from(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);

        vcx.simulate_modifiers_change(Modifiers::control());
        app.update_in(&mut vcx, |app, window, cx| app.tab_switch(true, window, cx));
        vcx.simulate_modifiers_change(Modifiers::none());
        vcx.run_until_parked();
        app.update(cx, |app, _| assert_eq!(app.active, 1));

        vcx.simulate_modifiers_change(Modifiers::control());
        app.update_in(&mut vcx, |app, window, cx| app.tab_switch(true, window, cx));
        vcx.simulate_modifiers_change(Modifiers::none());

        app.update(cx, |app, _| {
            assert_eq!(
                app.active, 0,
                "most-recently-used ordering makes the gesture a toggle"
            );
        });
    }

    #[gpui::test]
    fn a_lone_tab_still_opens_the_panel_but_does_not_hold(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 1);
        vcx.simulate_modifiers_change(Modifiers::control());

        app.update_in(&mut vcx, |app, window, cx| app.tab_switch(true, window, cx));

        app.update(cx, |app, _| {
            let sw = app
                .switcher
                .as_ref()
                .expect("with nothing to cycle it still opens the switcher");
            assert_eq!(
                sw.column,
                Column::Left,
                "the workspace column is the useful one"
            );
            assert!(sw.hold.is_none(), "nothing to commit, so nothing to hold");
        });
    }

    #[gpui::test]
    fn a_lone_tabs_panel_survives_letting_go_of_ctrl(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 1);
        vcx.simulate_modifiers_change(Modifiers::control());
        app.update_in(&mut vcx, |app, window, cx| app.tab_switch(true, window, cx));

        vcx.simulate_modifiers_change(Modifiers::none());

        app.update(cx, |app, _| {
            assert!(
                app.switcher.is_some(),
                "a panel opened without a hold must not close on release"
            );
        });
    }

    #[gpui::test]
    fn picking_a_tab_mid_gesture_commits_it_on_release(cx: &mut TestAppContext) {
        // What a click during the hold amounts to: aim the cursor, then let go.
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        vcx.simulate_modifiers_change(Modifiers::control());
        app.update_in(&mut vcx, |app, window, cx| app.tab_switch(true, window, cx));

        app.update(cx, |app, cx| app.switcher_point_tab(2, cx));
        vcx.simulate_modifiers_change(Modifiers::none());

        app.update(cx, |app, _| {
            assert!(app.switcher.is_none());
            assert_eq!(app.active, 2, "the tab the pointer picked is now active");
        });
    }

    #[gpui::test]
    fn a_held_gesture_hides_the_context_menus_it_would_trip_over(cx: &mut TestAppContext) {
        // macOS reports Ctrl+click as a right click, which is exactly what the
        // context menu listens for. Nothing to assert on the element tree from
        // here, so this pins the flag the render path branches on.
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        vcx.simulate_modifiers_change(Modifiers::control());
        app.update_in(&mut vcx, |app, window, cx| app.tab_switch(true, window, cx));

        app.update(cx, |app, _| {
            assert!(
                app.switcher.as_ref().is_some_and(|sw| sw.hold.is_some()),
                "the render path drops the menus while this is set"
            );
        });
    }

    #[gpui::test]
    fn machine_switcher_has_no_workspace_create_footer(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 1);
        app.update_in(&mut vcx, |app, window, cx| app.open_switcher(window, cx));
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("machine-switcher-card").is_some());
        app.update(cx, |app, _| assert!(app.switcher.is_some()));
        assert!(
            vcx.debug_bounds("switcher-new-workspace").is_none(),
            "the rendered switcher still offers named workspace creation"
        );
    }

    /// Creating a workspace switches this window over to it, and the pull that
    /// follows used to read the window back out of the registry to see what it
    /// was showing — the very entity `switch_workspace` is holding. gpui
    /// aborts the process for that, so the window had to be registered here for
    /// the lookup to find anything and the crash to reproduce (#617).
    #[gpui::test]
    fn creating_a_workspace_does_not_read_the_window_making_it(cx: &mut TestAppContext) {
        use gpui::VisualContext;

        let (app, mut vcx, _streams) = harness_with_tabs(cx, 1);
        let handle = vcx.window_handle();
        let weak = app.downgrade();
        app.update(cx, |app, cx| {
            crate::ui::windows::WindowRegistry::init(cx);
            crate::ui::windows::WindowRegistry::register(cx, app.workspace, handle, weak);
        });

        app.update_in(&mut vcx, |app, window, cx| {
            app.switch_workspace(None, window, cx)
        });

        app.update(cx, |app, cx| {
            assert!(
                crate::ui::windows::WindowRegistry::app_for(cx, app.workspace).is_some(),
                "the window followed its new workspace into the registry"
            );
        });
    }

    /// One of the three places on the card a test wants to put the pointer.
    #[derive(Clone, Copy)]
    enum Spot {
        Workspaces,
        Tabs,
        Search,
    }

    /// Where that part of the card lands on screen. The card is centred and
    /// its columns are laid out from `CARD_W` / `LEFT_W`, so the geometry is
    /// worth recomputing here rather than hard-coding pixels that move with
    /// the window size.
    fn card_point(vcx: &mut gpui::VisualTestContext, spot: Spot) -> gpui::Point<gpui::Pixels> {
        use gpui::{point, px};

        let viewport = vcx.update(|window, _| window.viewport_size());
        let card_w = super::CARD_W
            .min(viewport.width.as_f32() - 2. * super::CARD_MARGIN)
            .max(320.);
        let left_w = super::LEFT_W.min(card_w * 0.5);
        let card_left = (viewport.width.as_f32() - card_w) / 2.;
        let (dx, dy) = match spot {
            // The search row is the first thing in the card; both columns
            // start below it.
            Spot::Search => (100., 20.),
            Spot::Workspaces => (20., 60.),
            // Past the tab column's own header row, onto its first tab.
            Spot::Tabs => (left_w + 40., 42. + 6. + super::HOST_H + super::ROW_H / 2.),
        };
        point(px(card_left + dx), px(super::CARD_TOP + dy))
    }

    /// Ctrl+Tab, then reach for the mouse: the pointer leaves the tab column
    /// for the workspace list, and letting go of Ctrl there must not slam the
    /// panel shut — switching workspaces by hand is exactly what the user is
    /// in the middle of doing.
    #[gpui::test]
    fn releasing_ctrl_over_the_workspace_list_keeps_the_panel_up(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        vcx.simulate_modifiers_change(Modifiers::control());
        app.update_in(&mut vcx, |app, window, cx| app.tab_switch(true, window, cx));
        vcx.run_until_parked();

        let at = card_point(&mut vcx, Spot::Workspaces);
        vcx.simulate_mouse_move(at, None, Modifiers::control());
        vcx.simulate_modifiers_change(Modifiers::none());

        app.update(cx, |app, _| {
            let sw = app
                .switcher
                .as_ref()
                .expect("the panel stays up for the mouse to finish in");
            assert!(sw.hold.is_none(), "the hold is spent, not re-armed");
            assert_eq!(app.active, 0, "the release picked nothing");
        });
    }

    /// The pointer over the tab column is the ordinary gesture: release still
    /// commits.
    #[gpui::test]
    fn releasing_ctrl_over_the_tab_column_still_commits(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        vcx.simulate_modifiers_change(Modifiers::control());
        app.update_in(&mut vcx, |app, window, cx| app.tab_switch(true, window, cx));
        vcx.run_until_parked();

        let at = card_point(&mut vcx, Spot::Tabs);
        vcx.simulate_mouse_move(at, None, Modifiers::control());
        vcx.simulate_modifiers_change(Modifiers::none());

        app.update(cx, |app, _| {
            assert!(app.switcher.is_none(), "the panel comes down on release");
            assert_eq!(app.active, 1, "the highlighted tab is now the active one");
        });
    }

    /// macOS reports Ctrl+click as a right click, so a tab row picked with
    /// the mouse mid-gesture arrives on the right button. The row takes that
    /// press as the pick; nothing between it and the window may swallow it
    /// first.
    #[gpui::test]
    fn ctrl_clicking_a_tab_row_mid_gesture_picks_it(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        vcx.simulate_modifiers_change(Modifiers::control());
        app.update_in(&mut vcx, |app, window, cx| {
            app.tab_switch(true, window, cx);
            // Two steps down, so the row the pointer lands on below is not
            // the one the keyboard had already reached.
            app.tab_switch(true, window, cx);
        });
        vcx.run_until_parked();
        app.update(cx, |app, _| {
            assert_eq!(app.switcher.as_ref().expect("up").right_sel, 2);
        });

        let at = card_point(&mut vcx, Spot::Tabs);
        vcx.simulate_mouse_move(at, None, Modifiers::control());
        vcx.simulate_mouse_down(at, gpui::MouseButton::Right, Modifiers::control());

        app.update(cx, |app, _| {
            let sw = app.switcher.as_ref().expect("the panel stays up");
            assert_eq!(
                sw.right_sel, 0,
                "the row under the pointer took the press, not the keyboard's row 2"
            );
            assert!(
                sw.hold.is_some(),
                "the gesture is still on until Ctrl is up"
            );
        });

        vcx.simulate_modifiers_change(Modifiers::none());
        app.update(cx, |app, _| {
            assert!(app.switcher.is_none(), "release commits and closes");
            assert_eq!(
                app.active, 0,
                "the first row of a most-recently-used column is this very tab"
            );
        });
    }

    #[gpui::test]
    fn losing_focus_drops_the_hold_so_the_panel_cannot_hang(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        vcx.simulate_modifiers_change(Modifiers::control());
        app.update_in(&mut vcx, |app, window, cx| app.tab_switch(true, window, cx));

        vcx.deactivate_window();

        app.update(cx, |app, _| {
            let sw = app.switcher.as_ref().expect("the panel stays up");
            assert!(
                sw.hold.is_none(),
                "a release over another window never reaches us"
            );
        });
    }
}

#[cfg(test)]
mod key_tests {
    use gpui::Modifiers;

    use super::{Column, Key, key_intent};

    #[test]
    fn bare_arrows_and_enter_drive_the_panel() {
        let none = Modifiers::none();
        assert_eq!(key_intent("down", none), Key::Step(true));
        assert_eq!(key_intent("up", none), Key::Step(false));
        assert_eq!(key_intent("left", none), Key::ToColumn(Column::Left));
        assert_eq!(key_intent("right", none), Key::ToColumn(Column::Right));
        assert_eq!(key_intent("enter", none), Key::Confirm(false));
        assert_eq!(key_intent("escape", none), Key::Close);
    }

    #[test]
    fn the_secondary_modifier_turns_enter_into_a_new_window() {
        // ⌘ on macOS, Ctrl everywhere else. Off macOS this is the case that a
        // blanket "control means not ours" check would have swallowed.
        assert_eq!(
            key_intent("enter", Modifiers::secondary_key()),
            Key::Confirm(true)
        );
    }

    #[test]
    fn tab_walks_the_tab_column_in_both_directions() {
        assert_eq!(key_intent("tab", Modifiers::none()), Key::Tab(true));
        assert_eq!(key_intent("tab", Modifiers::shift()), Key::Tab(false));
    }

    #[test]
    fn a_held_control_keeps_tab_working_but_parks_the_arrows() {
        // Mid Ctrl+Tab gesture: Tab still steps, but an arrow key is somebody
        // else's chord. Only macOS sees the raw key here — everywhere else
        // Ctrl is the secondary modifier, the chord arrives as the NextTab
        // action instead, and the raw key must fall through untouched.
        let ctrl = Modifiers::control();
        match cfg!(target_os = "macos") {
            true => assert_eq!(key_intent("tab", ctrl), Key::Tab(true)),
            false => assert_eq!(key_intent("tab", ctrl), Key::Pass),
        }
        assert_eq!(key_intent("up", ctrl), Key::Pass);
    }

    #[test]
    fn alt_and_fn_chords_are_left_alone() {
        assert_eq!(key_intent("down", Modifiers::alt()), Key::Pass);
        assert_eq!(key_intent("enter", Modifiers::alt()), Key::Pass);
    }

    #[test]
    fn escape_closes_even_mid_chord() {
        assert_eq!(key_intent("escape", Modifiers::alt()), Key::Close);
        assert_eq!(key_intent("escape", Modifiers::secondary_key()), Key::Close);
    }

    #[test]
    fn the_secondary_glyph_matches_the_platform() {
        let glyph = crate::ui::keymap::secondary_glyph();
        match cfg!(target_os = "macos") {
            true => assert_eq!(glyph, "⌘"),
            false => assert_eq!(glyph, "Ctrl"),
        }
    }
}
