use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::sync::Arc;

use agentty_core::core::machine::{
    AgentFacts, Axis as TreeAxis, LayoutDelta, Machine, PaneNode, PaneRecord, PaneSeed,
    PaneTitleIdentity, Side, Tab as TreeTab, TabId,
};
use agentty_core::daemon::control::{ControlClient, ControlRequest, ReplyOk};
use agentty_core::host::HostId;
use gpui::{App, Global};

use crate::core::session::{Session, SessionPane, SessionTab, WorkspaceId, WorkspaceStore};
use crate::ui::app::AgenttyApp;
use crate::ui::pane::{Pane, PaneSlot};

pub(crate) fn control_for(cx: &mut App, host: HostId) -> Option<Arc<ControlClient>> {
    if host.is_local() {
        crate::ui::local_link::LocalLink::client(cx)
    } else {
        crate::ui::remote_connect::HostLinks::get(cx, host)
            .map(|h| Arc::clone(h.client()))
            .filter(|c| c.is_connected())
    }
}

pub(crate) enum TreeLink {
    Ready(Arc<ControlClient>),
    Unserved,
    Down,
}

pub(crate) fn tree_control_for(cx: &mut App, host: HostId) -> TreeLink {
    classify_tree_link(control_for(cx, host))
}

fn classify_tree_link(client: Option<Arc<ControlClient>>) -> TreeLink {
    match client {
        Some(client)
            if client
                .hello()
                .has_feature(agentty_core::daemon::control::feature::MACHINE_TREE) =>
        {
            TreeLink::Ready(client)
        }
        Some(_) => TreeLink::Unserved,
        None => TreeLink::Down,
    }
}

fn tree_workspace_id(cx: &App, client_ws: WorkspaceId) -> WorkspaceId {
    WorkspaceStore::all(cx)
        .get(client_ws)
        .and_then(|w| w.remote_workspace)
        .unwrap_or(client_ws)
}

pub(crate) struct TabTransferDispatch {
    pub(crate) client: Arc<ControlClient>,
    pub(crate) request: ControlRequest,
}

pub(crate) fn transfer_tab_to_workspace(
    cx: &mut App,
    from_client_workspace: WorkspaceId,
    tab: TabId,
    to_client_workspace: WorkspaceId,
) -> Result<TabTransferDispatch, String> {
    let from_host = WorkspaceStore::host_of(cx, from_client_workspace);
    let to_host = WorkspaceStore::host_of(cx, to_client_workspace);
    if from_host != to_host {
        return Err("source and destination windows belong to different machines".into());
    }
    let client = match tree_control_for(cx, from_host) {
        TreeLink::Ready(client) => client,
        TreeLink::Unserved => {
            return Err("this machine does not support persisted workspace transfer".into());
        }
        TreeLink::Down => return Err("the environment control link is unavailable".into()),
    };
    Ok(TabTransferDispatch {
        client,
        request: ControlRequest::TabTransfer {
            from_workspace: tree_workspace_id(cx, from_client_workspace),
            tab,
            to_workspace: tree_workspace_id(cx, to_client_workspace),
            to: 0,
        },
    })
}

#[derive(Debug, Clone)]
pub(crate) struct DesiredTab {
    pub id: TabId,
    pub name: Option<String>,
    pub group: Option<String>,
    pub root: DesiredNode,
}

#[derive(Debug, Clone)]
pub(crate) enum DesiredNode {
    Leaf {
        pane: u64,
        seed: PaneSeed,
    },
    Split {
        axis: TreeAxis,
        ratio: f32,
        a: Box<DesiredNode>,
        b: Box<DesiredNode>,
    },
}

impl DesiredNode {
    fn first_leaf(&self) -> (&u64, &PaneSeed) {
        match self {
            DesiredNode::Leaf { pane, seed } => (pane, seed),
            DesiredNode::Split { a, .. } => a.first_leaf(),
        }
    }

    fn to_pane_node(&self) -> PaneNode {
        match self {
            DesiredNode::Leaf { pane, .. } => PaneNode::Leaf { pane: *pane },
            DesiredNode::Split { axis, ratio, a, b } => PaneNode::Split {
                axis: *axis,
                ratio: *ratio,
                a: Box::new(a.to_pane_node()),
                b: Box::new(b.to_pane_node()),
            },
        }
    }

    fn seed_of(&self, pane: u64) -> Option<&PaneSeed> {
        match self {
            DesiredNode::Leaf { pane: p, seed } => (*p == pane).then_some(seed),
            DesiredNode::Split { a, b, .. } => a.seed_of(pane).or_else(|| b.seed_of(pane)),
        }
    }

    fn first_user_titles(&self, out: &mut Vec<(u64, String)>) {
        match self {
            DesiredNode::Leaf { pane, seed } => {
                if let Some(title) = seed
                    .agent
                    .as_ref()
                    .and_then(|agent| agent.first_user_title.as_deref())
                    .and_then(agentty_core::agent_runtime::normalize_title_candidate)
                {
                    out.push((*pane, title));
                }
            }
            DesiredNode::Split { a, b, .. } => {
                a.first_user_titles(out);
                b.first_user_titles(out);
            }
        }
    }

    fn provider_titles(&self, out: &mut Vec<(u64, String, PaneTitleIdentity)>) {
        match self {
            DesiredNode::Leaf { pane, seed } => {
                let Some(agent) = seed.agent.as_ref() else {
                    return;
                };
                let Some(title) = agent
                    .provider_title
                    .as_deref()
                    .and_then(agentty_core::agent_runtime::normalize_title_candidate)
                else {
                    return;
                };
                out.push((*pane, title, PaneTitleIdentity::from_facts(agent)));
            }
            DesiredNode::Split { a, b, .. } => {
                a.provider_titles(out);
                b.provider_titles(out);
            }
        }
    }
}

pub(crate) fn desired_tabs(
    app: &AgenttyApp,
    cx: &App,
) -> (Vec<DesiredTab>, Option<TabId>, Vec<TabId>) {
    let remote = WorkspaceStore::all(cx)
        .get(app.workspace)
        .is_some_and(|w| w.is_remote());
    let mut out = Vec::new();
    let mut active = None;
    let mut held = Vec::new();
    for (index, tab) in app.tabs.iter().enumerate() {
        let Some(root) = desired_node(&tab.pane, remote, cx) else {
            if !(remote && every_leaf_is_native_ssh(&tab.pane, cx)) {
                held.push(tab.tree_id.get());
            }
            continue;
        };
        let id = tab.tree_id.get();
        if index == app.active {
            active = Some(id);
        }
        out.push(DesiredTab {
            id,
            name: tab.name.clone(),
            group: tab
                .sidebar_group
                .borrow()
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            root,
        });
    }
    (out, active, held)
}

fn every_leaf_is_native_ssh(pane: &Pane, cx: &App) -> bool {
    match pane {
        Pane::Leaf(PaneSlot::Ready(view)) => view.read(cx).ssh_spec().is_some(),
        Pane::Leaf(PaneSlot::Connecting(_)) | Pane::Empty => false,
        Pane::Split { a, b, .. } => {
            every_leaf_is_native_ssh(a, cx) && every_leaf_is_native_ssh(b, cx)
        }
    }
}

/// Project a ready terminal's durable binding into the machine-tree seed.
///
/// `TerminalView::agent` and `agent_session` are live observations. They may
/// disappear as soon as the foreground process exits, while the
/// `LiveContainerBinding` is the persisted carrier identity that must survive
/// that transition. Prefer every binding-owned identity field and consult the
/// runtime observations only when the corresponding binding field is absent.
fn desired_agent_facts(
    binding: &agentty_core::core::session::LiveContainerBinding,
    runtime_agent: Option<crate::core::cli_agent::CLIAgent>,
    runtime_session: Option<&crate::core::cli_agent::AgentSessionState>,
) -> Option<AgentFacts> {
    let agent = binding.agent.or(runtime_agent)?;
    let session_id = binding
        .session_id
        .clone()
        .or_else(|| runtime_session.and_then(|session| session.session_id.clone()));
    let launch_argv = if binding.launch_argv.is_empty() {
        runtime_session.and_then(|session| session.launch_argv.clone())
    } else {
        Some(binding.launch_argv.clone())
    };
    let first_user_title = binding
        .first_user_title()
        .and_then(agentty_core::agent_runtime::normalize_title_candidate);
    let provider_title = binding
        .provider_title()
        .and_then(agentty_core::agent_runtime::normalize_title_candidate);

    Some(AgentFacts {
        agent,
        container_id: Some(binding.container_id.clone()),
        session_id,
        launch_argv,
        status: None,
        provider_title,
        first_user_title,
    })
}

fn desired_node(pane: &Pane, remote_window: bool, cx: &App) -> Option<DesiredNode> {
    match pane {
        Pane::Leaf(PaneSlot::Ready(view)) => {
            let view = view.read(cx);
            let ssh_spec = view.ssh_spec();
            if remote_window && ssh_spec.is_some() {
                return None;
            }
            let runtime_agent = view.agent();
            let runtime_session = view.agent_session();
            let agent =
                desired_agent_facts(view.live_binding(), runtime_agent, runtime_session.as_ref());
            Some(DesiredNode::Leaf {
                pane: view.pane_id,
                seed: PaneSeed {
                    pane: view.pane_id,
                    cwd: view
                        .spawnable_cwd()
                        .map(|p| p.to_string_lossy().into_owned()),
                    ssh_spec,
                    agent,
                },
            })
        }
        Pane::Leaf(PaneSlot::Connecting(pending)) => {
            let spawn = &pending.read(cx).spawn;
            let pane = spawn.restore_pane?;
            let agent = spawn.live_binding.agent.map(|agent| AgentFacts {
                agent,
                container_id: Some(spawn.live_binding.container_id.clone()),
                session_id: spawn.live_binding.session_id.clone(),
                launch_argv: Some(spawn.live_binding.launch_argv.clone()),
                status: None,
                provider_title: spawn.live_binding.provider_title().map(str::to_owned),
                first_user_title: spawn.live_binding.first_user_title().map(str::to_owned),
            });
            Some(DesiredNode::Leaf {
                pane,
                seed: PaneSeed {
                    pane,
                    cwd: spawn
                        .working_directory
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned()),
                    ssh_spec: None,
                    agent,
                },
            })
        }
        Pane::Split {
            axis, a, b, ratio, ..
        } => {
            let left = desired_node(a, remote_window, cx);
            let right = desired_node(b, remote_window, cx);
            match (left, right) {
                (Some(a), Some(b)) => Some(DesiredNode::Split {
                    axis: match axis {
                        gpui::Axis::Horizontal => TreeAxis::Horizontal,
                        gpui::Axis::Vertical => TreeAxis::Vertical,
                    },
                    ratio: ratio.get(),
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (one, other) => one.or(other),
            }
        }
        Pane::Empty => None,
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct WsMirror {
    pub tabs: Vec<TreeTab>,
    pub active: Option<TabId>,
    /// Write-once title candidates already known by this window's machine
    /// mirror.  Keep only non-empty values so a late hydrate/delta can still
    /// enqueue the first real title exactly once.
    pub pane_first_user_titles: HashMap<u64, String>,
    /// Provider/legacy title evidence already known by this window's machine
    /// mirror.  The map prevents a later runtime observation from erasing it
    /// during hydrate/reconnect and is advanced only after machine authority
    /// is known (or a structural seed carries the value atomically).
    pub pane_provider_titles: HashMap<u64, String>,
    /// Identity snapshots fetched from MachineGet/PaneFacts.  Provider-title
    /// migrations are not queued for a pane whose machine identity is absent;
    /// that fail-closed rule avoids optimistic bookkeeping for bare panes.
    pub pane_agent_identities: HashMap<u64, PaneTitleIdentity>,
}

fn mirror_from_machine(
    machine: &Machine,
    workspace: &agentty_core::core::machine::Workspace,
) -> WsMirror {
    let mut mirror = WsMirror {
        tabs: workspace.tabs.clone(),
        active: workspace.active_tab,
        ..WsMirror::default()
    };
    for pane in &machine.panes {
        // The machine snapshot is authoritative for both identity and typed
        // evidence.  Keeping this projection in one helper prevents prime
        // and hydrate from drifting into different title semantics.
        remember_pane_title(&mut mirror, pane);
    }
    mirror
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SyncScope {
    Full,
    Additive,
}

pub(crate) fn diff(
    workspace: WorkspaceId,
    mirror: &mut WsMirror,
    desired: &[DesiredTab],
    desired_active: Option<TabId>,
    scope: SyncScope,
    held: &[TabId],
) -> Vec<ControlRequest> {
    let mut ops = Vec::new();
    let mut blocked_orphans = HashSet::new();

    // Keep orphan tabs in the mirror until after desired tabs have been
    // reconciled.  A tab-strip merge presents the destination as a tab with
    // one additional pane while the source tab disappears at the same time;
    // retaining the source here lets the reconciliation turn that pair into
    // the machine's atomic PaneMove operation instead of a duplicate
    // PaneSplit followed by TabClose.
    let orphan_sources: HashMap<u64, TabId> = if scope == SyncScope::Full {
        mirror
            .tabs
            .iter()
            .filter(|tab| !desired.iter().any(|want| want.id == tab.id) && !held.contains(&tab.id))
            .filter_map(|tab| match &tab.root {
                PaneNode::Leaf { pane } => Some((*pane, tab.id)),
                PaneNode::Split { .. } => None,
            })
            .collect()
    } else {
        HashMap::new()
    };

    for (index, want) in desired.iter().enumerate() {
        match mirror.tabs.iter().position(|t| t.id == want.id) {
            None => {
                let at = match scope {
                    SyncScope::Full => index,
                    SyncScope::Additive => mirror.tabs.len(),
                };
                create_tab(workspace, mirror, at, want, &mut ops);
            }
            Some(at) => reconcile_tab(
                workspace,
                mirror,
                at,
                want,
                &orphan_sources,
                &mut blocked_orphans,
                &mut ops,
            ),
        }
    }

    // Any orphan not consumed by a PaneMove is an ordinary closed tab.  Do
    // this only after reconciliation so a merge can remove its source from
    // the mirror without emitting a second, racing TabClose.
    if scope == SyncScope::Full {
        let mut index = 0;
        while index < mirror.tabs.len() {
            let id = mirror.tabs[index].id;
            if desired.iter().any(|want| want.id == id)
                || held.contains(&id)
                || blocked_orphans.contains(&id)
            {
                index += 1;
                continue;
            }
            let closed = mirror.tabs.remove(index);
            forget_tab_titles(mirror, &closed.root);
            ops.push(ControlRequest::TabClose {
                workspace,
                tab: closed.id,
            });
            heal_active(mirror, index);
        }
    }

    // Title writes must follow all topology operations above: a freshly
    // created/split/replaced pane does not exist on the machine until its
    // corresponding structural request has been applied. Provider evidence
    // is queued first and carries an identity precondition; first-user
    // evidence remains a separate write-once request.
    queue_provider_title_updates(workspace, mirror, desired, &mut ops);
    queue_first_user_title_updates(workspace, mirror, desired, &mut ops);

    if scope == SyncScope::Additive || !held.is_empty() {
        return ops;
    }

    for (index, want) in desired.iter().enumerate() {
        let at = mirror
            .tabs
            .iter()
            .position(|t| t.id == want.id)
            .expect("every desired tab exists after the passes above");
        if at != index {
            let tab = mirror.tabs.remove(at);
            mirror.tabs.insert(index, tab);
            ops.push(ControlRequest::TabMove {
                workspace,
                tab: want.id,
                to: index as u64,
            });
        }
    }

    if let Some(active) = desired_active
        && mirror.active != Some(active)
        && mirror.tabs.iter().any(|t| t.id == active)
    {
        mirror.active = Some(active);
        ops.push(ControlRequest::WorkspaceSetActiveTab {
            workspace,
            tab: active,
        });
    }

    ops
}

fn heal_active(mirror: &mut WsMirror, removed: usize) {
    let named = mirror
        .active
        .is_some_and(|active| mirror.tabs.iter().any(|t| t.id == active));
    if named {
        return;
    }
    if mirror.tabs.is_empty() {
        mirror.active = None;
        return;
    }
    mirror.active = Some(mirror.tabs[removed.min(mirror.tabs.len() - 1)].id);
}

fn forget_pane_title_state(mirror: &mut WsMirror, pane: u64) {
    mirror.pane_first_user_titles.remove(&pane);
    mirror.pane_provider_titles.remove(&pane);
    mirror.pane_agent_identities.remove(&pane);
}

fn forget_tab_titles(mirror: &mut WsMirror, root: &PaneNode) {
    for pane in root.pane_ids() {
        forget_pane_title_state(mirror, pane);
    }
}

fn queue_provider_title_updates(
    workspace: WorkspaceId,
    mirror: &mut WsMirror,
    desired: &[DesiredTab],
    ops: &mut Vec<ControlRequest>,
) {
    let mut wanted = Vec::new();
    for tab in desired {
        tab.root.provider_titles(&mut wanted);
    }
    for (pane, title, expected_identity) in wanted {
        if mirror.pane_provider_titles.contains_key(&pane) {
            if mirror
                .pane_agent_identities
                .get(&pane)
                .is_some_and(|identity| identity == &expected_identity)
            {
                continue;
            }
            // A pane id was reused but the stale delta has not reached this
            // mirror yet.  Drop the old evidence and wait for an authoritative
            // identity rather than suppressing the replacement migration.
            mirror.pane_provider_titles.remove(&pane);
        }

        // A structural seed persists provider evidence atomically with the
        // pane create/split/replace.  Mark both typed slots only after
        // recognizing that exact seed; this is safe even when the pane was
        // absent from the mirror before this diff.
        if ops.iter().any(|op| seeded_provider_title(op, pane, &title)) {
            mirror.pane_provider_titles.insert(pane, title);
            mirror.pane_agent_identities.insert(pane, expected_identity);
            continue;
        }

        // Do not optimistically mark a bare/unknown pane.  Prime/hydrate must
        // first fetch MachineGet and populate the identity map; otherwise a
        // no-agent machine response could make the migration look committed
        // forever.
        let Some(machine_identity) = mirror.pane_agent_identities.get(&pane).cloned() else {
            continue;
        };
        if machine_identity != expected_identity {
            // The pane id may have been reused for another agent.  Waiting for
            // the next authoritative delta/re-pull is safer than stamping a
            // stale title onto the replacement.
            continue;
        }

        mirror.pane_provider_titles.insert(pane, title.clone());
        ops.push(ControlRequest::PaneSetProviderTitle {
            workspace,
            pane,
            title,
            expected_identity: Some(expected_identity),
        });
    }
}

fn queue_first_user_title_updates(
    workspace: WorkspaceId,
    mirror: &mut WsMirror,
    desired: &[DesiredTab],
    ops: &mut Vec<ControlRequest>,
) {
    let mut wanted = Vec::new();
    for tab in desired {
        tab.root.first_user_titles(&mut wanted);
    }
    for (pane, title) in wanted {
        // A known value is write-once.  In particular, a stale live binding
        // must not replace the title already restored from the machine tree.
        if mirror.pane_first_user_titles.contains_key(&pane) {
            continue;
        }
        // Structural requests already carry the complete PaneSeed. Mark a
        // title included in TabCreate/PaneSplit/PaneReplace as committed in
        // the optimistic mirror instead of sending a redundant follow-up
        // request (the machine stores the seed atomically with that op).
        if ops.iter().any(|op| seeded_title(op, pane, &title)) {
            mirror.pane_first_user_titles.insert(pane, title);
            continue;
        }
        mirror.pane_first_user_titles.insert(pane, title.clone());
        ops.push(ControlRequest::PaneSetFirstUserTitle {
            workspace,
            pane,
            title,
        });
    }
}

fn seeded_title(op: &ControlRequest, pane: u64, title: &str) -> bool {
    let seed = match op {
        ControlRequest::TabCreate { pane: seed, .. }
        | ControlRequest::PaneSplit { new: seed, .. }
        | ControlRequest::PaneReplace { new: seed, .. }
            if seed.pane == pane =>
        {
            seed
        }
        _ => return false,
    };
    seed.agent
        .as_ref()
        .and_then(|agent| agent.first_user_title.as_deref())
        .and_then(agentty_core::agent_runtime::normalize_title_candidate)
        .is_some_and(|candidate| candidate == title)
}

fn seeded_provider_title(op: &ControlRequest, pane: u64, title: &str) -> bool {
    let seed = match op {
        ControlRequest::TabCreate { pane: seed, .. }
        | ControlRequest::PaneSplit { new: seed, .. }
        | ControlRequest::PaneReplace { new: seed, .. }
            if seed.pane == pane =>
        {
            seed
        }
        _ => return false,
    };
    seed.agent
        .as_ref()
        .and_then(|agent| agent.provider_title.as_deref())
        .and_then(agentty_core::agent_runtime::normalize_title_candidate)
        .is_some_and(|candidate| candidate == title)
}

fn create_tab(
    workspace: WorkspaceId,
    mirror: &mut WsMirror,
    index: usize,
    want: &DesiredTab,
    ops: &mut Vec<ControlRequest>,
) {
    let (first, seed) = want.root.first_leaf();
    ops.push(ControlRequest::TabCreate {
        workspace,
        at: Some(index as u64),
        pane: seed.clone(),
        tab: Some(want.id),
    });
    let mut root = PaneNode::Leaf { pane: *first };
    materialize_splits(workspace, &want.root, &mut root, ops);
    if want.name.is_some() {
        ops.push(ControlRequest::TabRename {
            workspace,
            tab: want.id,
            name: want.name.clone(),
        });
    }
    if want.group.is_some() {
        ops.push(ControlRequest::TabSetGroup {
            workspace,
            tab: want.id,
            group: want.group.clone(),
        });
    }
    mirror.tabs.insert(
        index.min(mirror.tabs.len()),
        TreeTab {
            id: want.id,
            name: want.name.clone(),
            sidebar_group: want.group.clone(),
            root,
        },
    );
    mirror.active = Some(want.id);
}

fn materialize_splits(
    workspace: WorkspaceId,
    want: &DesiredNode,
    root: &mut PaneNode,
    ops: &mut Vec<ControlRequest>,
) {
    let DesiredNode::Split { axis, ratio, a, b } = want else {
        return;
    };
    let (anchor, _) = a.first_leaf();
    let (new, seed) = b.first_leaf();
    ops.push(ControlRequest::PaneSplit {
        workspace,
        pane: *anchor,
        axis: *axis,
        ratio: *ratio,
        new: seed.clone(),
        first: false,
    });
    root.split_leaf(*anchor, *new, *axis, *ratio, false);
    materialize_splits(workspace, a, root, ops);
    materialize_splits(workspace, b, root, ops);
}

fn reconcile_tab(
    workspace: WorkspaceId,
    mirror: &mut WsMirror,
    at: usize,
    want: &DesiredTab,
    orphan_sources: &HashMap<u64, TabId>,
    blocked_orphans: &mut HashSet<TabId>,
    ops: &mut Vec<ControlRequest>,
) {
    {
        let tab = &mut mirror.tabs[at];
        if tab.name != want.name {
            tab.name = want.name.clone();
            ops.push(ControlRequest::TabRename {
                workspace,
                tab: want.id,
                name: want.name.clone(),
            });
        }
        if tab.sidebar_group != want.group {
            tab.sidebar_group = want.group.clone();
            ops.push(ControlRequest::TabSetGroup {
                workspace,
                tab: want.id,
                group: want.group.clone(),
            });
        }
    }

    let desired_root = want.root.to_pane_node();
    if mirror.tabs[at].root == desired_root {
        return;
    }
    if same_shape_and_panes(&mirror.tabs[at].root, &desired_root) {
        fix_ratios(
            workspace,
            want.id,
            &mut mirror.tabs[at].root,
            &desired_root,
            ops,
        );
        return;
    }

    let have = mirror.tabs[at].root.pane_ids();
    let wanted = desired_root.pane_ids();
    let added: Vec<u64> = wanted
        .iter()
        .copied()
        .filter(|p| !have.contains(p))
        .collect();
    let removed: Vec<u64> = have
        .iter()
        .copied()
        .filter(|p| !wanted.contains(p))
        .collect();

    let mut moved_source = None;
    let done = match (added.as_slice(), removed.as_slice()) {
        ([new], []) => {
            if let Some(source) = try_single_pane_move(
                workspace,
                mirror,
                at,
                &desired_root,
                *new,
                orphan_sources,
                ops,
            ) {
                moved_source = Some(source);
                true
            } else {
                try_single_split(workspace, mirror, at, want, &desired_root, *new, ops)
            }
        }
        ([], gone) if !gone.is_empty() => {
            for pane in gone {
                mirror.tabs[at].root.remove_leaf(*pane);
                forget_pane_title_state(mirror, *pane);
                ops.push(ControlRequest::PaneClose {
                    workspace,
                    pane: *pane,
                });
            }
            same_shape_and_panes(&mirror.tabs[at].root, &desired_root)
        }
        ([new], [old]) => {
            let elsewhere = mirror
                .tabs
                .iter()
                .enumerate()
                .any(|(i, t)| i != at && t.root.contains(*new));
            let mut predicted = mirror.tabs[at].root.clone();
            predicted.replace_leaf(*old, *new);
            if !elsewhere && same_shape_and_panes(&predicted, &desired_root) {
                let seed = want
                    .root
                    .seed_of(*new)
                    .expect("the added pane is a desired leaf")
                    .clone();
                forget_pane_title_state(mirror, *old);
                mirror.tabs[at].root = predicted;
                ops.push(ControlRequest::PaneReplace {
                    workspace,
                    old: *old,
                    new: seed,
                });
                true
            } else {
                false
            }
        }
        _ => false,
    };

    if done {
        if let Some(source) = moved_source {
            // The source may precede the destination in the mirror.  Remove
            // it by stable tab id, then re-find the destination before fixing
            // ratios; the old `at` index is no longer reliable.
            if let Some(source_at) = mirror.tabs.iter().position(|tab| tab.id == source) {
                mirror.tabs.remove(source_at);
                heal_active(mirror, source_at);
            }
        }
        let target_at = mirror
            .tabs
            .iter()
            .position(|tab| tab.id == want.id)
            .expect("the reconciled destination tab remains in the mirror");
        fix_ratios(
            workspace,
            want.id,
            &mut mirror.tabs[target_at].root,
            &desired_root,
            ops,
        );
        return;
    }

    // A pane id already owned by another tab cannot be materialized with
    // PaneSplit/TabCreate.  If the constrained PaneMove shape did not match,
    // fail closed and leave the current tree intact for a later, valid drag
    // snapshot.  Keep an orphan source in the mirror as well; the deferred
    // cleanup below must not turn this refusal into a destructive TabClose.
    if added.iter().any(|pane| {
        mirror
            .tabs
            .iter()
            .enumerate()
            .any(|(index, tab)| index != at && tab.root.contains(*pane))
    }) {
        for pane in &added {
            if let Some(source) = orphan_sources.get(pane) {
                blocked_orphans.insert(*source);
            }
        }
        return;
    }

    let closed = mirror.tabs.remove(at);
    forget_tab_titles(mirror, &closed.root);
    ops.push(ControlRequest::TabClose {
        workspace,
        tab: closed.id,
    });
    heal_active(mirror, at);
    create_tab(workspace, mirror, at, want, ops);
}

/// Recognize the constrained 1+1 tab-strip merge shape and express it using
/// the machine's atomic pane-move primitive.  The source tab must still be a
/// single orphan leaf; held tabs and desired tabs are never eligible sources.
fn try_single_pane_move(
    workspace: WorkspaceId,
    mirror: &mut WsMirror,
    target_at: usize,
    desired_root: &PaneNode,
    new: u64,
    orphan_sources: &HashMap<u64, TabId>,
    ops: &mut Vec<ControlRequest>,
) -> Option<TabId> {
    let PaneNode::Split { axis, a, b, .. } = desired_root else {
        return None;
    };
    let (sibling, first) = match (&**a, &**b) {
        (PaneNode::Leaf { pane }, PaneNode::Leaf { pane: added }) if *added == new => {
            (*pane, false)
        }
        (PaneNode::Leaf { pane: added }, PaneNode::Leaf { pane }) if *added == new => (*pane, true),
        _ => return None,
    };

    let PaneNode::Leaf { pane: current } = &mirror.tabs[target_at].root else {
        return None;
    };
    if *current != sibling {
        return None;
    }

    let source = *orphan_sources.get(&new)?;
    if source == mirror.tabs[target_at].id {
        return None;
    }
    let source_at = mirror.tabs.iter().position(|tab| tab.id == source)?;
    if !matches!(&mirror.tabs[source_at].root, PaneNode::Leaf { pane } if *pane == new) {
        return None;
    }
    // A pane id must have one owner.  Refuse the move if a malformed/stale
    // mirror also references it from another tab.
    if mirror
        .tabs
        .iter()
        .enumerate()
        .any(|(index, tab)| index != source_at && tab.root.contains(new))
    {
        return None;
    }

    // The machine's PaneMove primitive always starts the new split at ratio
    // 0.5.  Keep that fact in the optimistic mirror so the normal ratio pass
    // can emit PaneSetRatio when the UI snapshot requested a different ratio.
    let mut predicted = mirror.tabs[target_at].root.clone();
    if !predicted.split_leaf(sibling, new, *axis, 0.5, first) {
        return None;
    }
    mirror.tabs[target_at].root = predicted;
    ops.push(ControlRequest::PaneMove {
        workspace,
        pane: new,
        to: sibling,
        axis: *axis,
        first,
    });
    Some(source)
}

fn try_single_split(
    workspace: WorkspaceId,
    mirror: &mut WsMirror,
    at: usize,
    want: &DesiredTab,
    desired_root: &PaneNode,
    new: u64,
    ops: &mut Vec<ControlRequest>,
) -> bool {
    let Some((sibling, axis, ratio, first)) = split_site(desired_root, new) else {
        return false;
    };
    // A PaneSplit materializes a fresh pane record.  If the id is already
    // owned by another mirror tab, sending it would be refused by the
    // machine (and is precisely the race avoided by PaneMove for merges).
    if mirror
        .tabs
        .iter()
        .enumerate()
        .any(|(index, tab)| index != at && tab.root.contains(new))
    {
        return false;
    }
    let mut predicted = mirror.tabs[at].root.clone();
    if !predicted.split_leaf(sibling, new, axis, ratio, first) {
        return false;
    }
    if !same_shape_and_panes(&predicted, desired_root) {
        return false;
    }
    let seed = want
        .root
        .seed_of(new)
        .expect("the added pane is a desired leaf")
        .clone();
    mirror.tabs[at].root = predicted;
    ops.push(ControlRequest::PaneSplit {
        workspace,
        pane: sibling,
        axis,
        ratio,
        new: seed,
        first,
    });
    true
}

fn split_site(node: &PaneNode, new: u64) -> Option<(u64, TreeAxis, f32, bool)> {
    let PaneNode::Split { axis, ratio, a, b } = node else {
        return None;
    };
    match (&**a, &**b) {
        (PaneNode::Leaf { pane }, sibling) if *pane == new => {
            if let PaneNode::Leaf { pane: s } = sibling {
                return Some((*s, *axis, *ratio, true));
            }
            return None;
        }
        (sibling, PaneNode::Leaf { pane }) if *pane == new => {
            if let PaneNode::Leaf { pane: s } = sibling {
                return Some((*s, *axis, *ratio, false));
            }
            return None;
        }
        _ => {}
    }
    if a.contains(new) {
        split_site(a, new)
    } else if b.contains(new) {
        split_site(b, new)
    } else {
        None
    }
}

fn same_shape_and_panes(a: &PaneNode, b: &PaneNode) -> bool {
    match (a, b) {
        (PaneNode::Leaf { pane: pa }, PaneNode::Leaf { pane: pb }) => pa == pb,
        (
            PaneNode::Split {
                axis: ax,
                a: aa,
                b: ab,
                ..
            },
            PaneNode::Split {
                axis: bx,
                a: ba,
                b: bb,
                ..
            },
        ) => ax == bx && same_shape_and_panes(aa, ba) && same_shape_and_panes(ab, bb),
        _ => false,
    }
}

fn fix_ratios(
    workspace: WorkspaceId,
    tab: TabId,
    mirror: &mut PaneNode,
    desired: &PaneNode,
    ops: &mut Vec<ControlRequest>,
) {
    fn walk(
        workspace: WorkspaceId,
        tab: TabId,
        mirror: &mut PaneNode,
        desired: &PaneNode,
        path: &mut Vec<Side>,
        ops: &mut Vec<ControlRequest>,
    ) {
        let (
            PaneNode::Split {
                ratio: mr,
                a: ma,
                b: mb,
                ..
            },
            PaneNode::Split {
                ratio: dr,
                a: da,
                b: db,
                ..
            },
        ) = (mirror, desired)
        else {
            return;
        };
        if (*mr - *dr).abs() > 1e-4 {
            *mr = *dr;
            ops.push(ControlRequest::PaneSetRatio {
                workspace,
                tab,
                path: path.clone(),
                ratio: *dr,
            });
        }
        path.push(Side::A);
        walk(workspace, tab, ma, da, path, ops);
        path.pop();
        path.push(Side::B);
        walk(workspace, tab, mb, db, path, ops);
        path.pop();
    }
    let mut path = Vec::new();
    walk(workspace, tab, mirror, desired, &mut path, ops);
}

enum SyncPhase {
    Unprimed { dirty: bool, priming: bool },
    Primed(WsMirror),
}

struct WsState {
    sync: SyncPhase,
    queue: VecDeque<ControlRequest>,
    inflight: bool,
    informed: bool,
    epoch: u64,
    /// A hydration that failed and still owes this window its layout.
    ///
    /// The window is sitting empty because of it, so nothing may be pushed
    /// from it until the pull is retried — an empty window diffs into
    /// "close every tab" and would wipe the layout off the machine.
    rehydrate: Option<Adopt>,
}

impl Default for WsState {
    fn default() -> Self {
        WsState {
            sync: SyncPhase::Unprimed {
                dirty: false,
                priming: false,
            },
            queue: VecDeque::new(),
            inflight: false,
            informed: false,
            epoch: 0,
            rehydrate: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct TreeSync {
    windows: HashMap<WorkspaceId, WsState>,
}

impl Global for TreeSync {}

pub(crate) fn sync_window(app: &AgenttyApp, cx: &mut App) {
    let client_ws = app.workspace;
    if !cx.has_global::<crate::core::session::WorkspaceStore>() {
        return;
    }
    if crate::ui::remote_workspace::workspace_is_preempted(cx, client_ws) {
        return;
    }
    if let Some(adopt) = take_rehydrate(cx, client_ws, app.tabs.is_empty()) {
        hydrate(cx, client_ws, adopt);
        return;
    }
    adopt_tab_ids(app, cx);
    let (desired, desired_active, held) = desired_tabs(app, cx);
    let machine_ws = tree_workspace_id(cx, client_ws);

    let state = cx
        .default_global::<TreeSync>()
        .windows
        .entry(client_ws)
        .or_default();
    match &mut state.sync {
        SyncPhase::Unprimed { dirty, priming } => {
            *dirty = true;
            if !*priming {
                *priming = true;
                start_prime(cx, client_ws);
            }
        }
        SyncPhase::Primed(mirror) => {
            let scope = if state.informed {
                SyncScope::Full
            } else {
                SyncScope::Additive
            };
            let ops = diff(machine_ws, mirror, &desired, desired_active, scope, &held);
            if !ops.is_empty() {
                let (tabs, active) = (mirror.tabs.clone(), mirror.active);
                state.queue.extend(ops);
                let host = WorkspaceStore::host_of(cx, client_ws);
                crate::ui::machine_mirror::MachineMirrors::note_synced_workspace(
                    cx, host, machine_ws, tabs, active,
                );
                pump(cx, client_ws);
            }
        }
    }
}

pub(crate) fn on_link_up(cx: &mut App, host: HostId) {
    for (workspace, app) in crate::ui::windows::WindowRegistry::open_windows(cx) {
        if WorkspaceStore::host_of(cx, workspace) != host {
            continue;
        }
        if let Some(app) = app.upgrade() {
            app.update(cx, |app, cx| sync_window(app, cx));
        }
    }
}

/// Claims a hydration owed to `client_ws`, if one is still outstanding.
///
/// A `Replace` retry is dropped once the window has tabs again: the user moved
/// on without us, and replaying the machine's older layout over their work
/// would be worse than never retrying at all.
fn take_rehydrate(cx: &mut App, client_ws: WorkspaceId, window_is_empty: bool) -> Option<Adopt> {
    let state = cx
        .default_global::<TreeSync>()
        .windows
        .get_mut(&client_ws)?;
    let adopt = state.rehydrate.take()?;
    (window_is_empty || adopt == Adopt::IfEmpty).then_some(adopt)
}

pub(crate) fn window_is_informed(cx: &App, client_ws: WorkspaceId) -> bool {
    cx.try_global::<TreeSync>()
        .and_then(|t| t.windows.get(&client_ws))
        .is_some_and(|s| s.informed)
}

pub(crate) fn mark_window_informed(cx: &mut App, client_ws: WorkspaceId) {
    cx.default_global::<TreeSync>()
        .windows
        .entry(client_ws)
        .or_default()
        .informed = true;
}

fn adopt_tab_ids(app: &AgenttyApp, cx: &App) {
    let Some(TreeSync { windows }) = cx.try_global::<TreeSync>() else {
        return;
    };
    let Some(WsState {
        sync: SyncPhase::Primed(mirror),
        ..
    }) = windows.get(&app.workspace)
    else {
        return;
    };
    let known: Vec<TabId> = app.tabs.iter().map(|t| t.tree_id.get()).collect();
    for tab in &app.tabs {
        let id = tab.tree_id.get();
        if mirror.tabs.iter().any(|m| m.id == id) {
            continue;
        }
        let panes: Vec<u64> = tab
            .pane
            .terminals()
            .iter()
            .map(|v| v.read(cx).pane_id)
            .collect();
        if panes.is_empty() {
            continue;
        }
        let Some(matched) = mirror
            .tabs
            .iter()
            .find(|m| !known.contains(&m.id) && panes.iter().any(|p| m.root.contains(*p)))
        else {
            continue;
        };
        tab.tree_id.set(matched.id);
    }
}

pub(crate) fn on_preempted(cx: &mut App, client_ws: WorkspaceId) {
    let Some(state) = cx.default_global::<TreeSync>().windows.get_mut(&client_ws) else {
        return;
    };
    state.sync = SyncPhase::Unprimed {
        dirty: false,
        priming: false,
    };
    state.queue.clear();
    state.informed = false;
    state.epoch += 1;
}

pub(crate) fn forget(cx: &mut App, client_ws: WorkspaceId) {
    if let Some(state) = cx.try_global::<TreeSync>() {
        let _ = state;
        cx.default_global::<TreeSync>().windows.remove(&client_ws);
    }
}

pub(crate) fn fire_workspace_op(
    cx: &mut App,
    client_ws: WorkspaceId,
    op: impl FnOnce(WorkspaceId) -> ControlRequest,
) {
    if !cx.has_global::<crate::core::session::WorkspaceStore>() {
        return;
    }
    let host = WorkspaceStore::host_of(cx, client_ws);
    let machine_ws = tree_workspace_id(cx, client_ws);
    let request = op(machine_ws);
    crate::ui::machine_mirror::MachineMirrors::note_workspace_op(cx, host, &request);
    let client = match tree_control_for(cx, host) {
        TreeLink::Ready(client) => client,
        TreeLink::Unserved => {
            unsendable(
                &request,
                "this machine's server does not serve the workspace tree",
            );
            return;
        }
        TreeLink::Down => {
            unsendable(&request, "there is no control link to its machine");
            return;
        }
    };
    cx.background_executor()
        .spawn(async move {
            if let Err(e) = client.call(request.clone()) {
                unsendable(&request, &format!("the machine refused it: {e}"));
            }
        })
        .detach();
}

fn unsendable(request: &ControlRequest, why: &str) {
    match request {
        ControlRequest::WorkspaceRemove { workspace } => log::warn!(
            "workspace {workspace} was deleted here but not on its machine ({why}); \
             its entry stays in that machine's tree, where another client will still \
             see it — delete it again from a client that can reach the machine"
        ),
        other => log::debug!("{other:?} not sent ({why}); the next edit carries it"),
    }
}

pub(crate) fn rename_workspace(cx: &mut App, client_ws: WorkspaceId, name: Option<String>) {
    fire_workspace_op(cx, client_ws, move |ws| ControlRequest::WorkspaceRename {
        workspace: ws,
        name,
    });
}

fn start_prime(cx: &mut App, client_ws: WorkspaceId) {
    let host = WorkspaceStore::host_of(cx, client_ws);
    let machine_ws = tree_workspace_id(cx, client_ws);
    let client = match tree_control_for(cx, host) {
        TreeLink::Ready(client) => client,
        unavailable => {
            if matches!(unavailable, TreeLink::Unserved) {
                log::warn!(
                    "workspace {client_ws}: its machine's server does not serve the tree; \
                     the layout will not be synced"
                );
            }
            if let Some(state) = cx.default_global::<TreeSync>().windows.get_mut(&client_ws)
                && let SyncPhase::Unprimed { priming, .. } = &mut state.sync
            {
                *priming = false;
            }
            return;
        }
    };
    let epoch = cx
        .default_global::<TreeSync>()
        .windows
        .get(&client_ws)
        .map(|s| s.epoch)
        .unwrap_or(0);
    cx.spawn(async move |cx| {
        let outcome = cx
            .background_executor()
            .spawn(async move { pull_or_create(&client, machine_ws) })
            .await;
        cx.update(|cx| finish_prime(cx, client_ws, epoch, outcome));
    })
    .detach();
}

fn pull_or_create(client: &ControlClient, machine_ws: WorkspaceId) -> io::Result<WsMirror> {
    // Prime reads the complete machine snapshot before any optimistic diff.
    // WorkspaceTree alone has no PaneRecord identity, which would make a
    // provider-title migration either unsafe or permanently invisible.
    let machine: Machine = match client.call(ControlRequest::MachineGet)? {
        ReplyOk::MachineTree(machine) => *machine,
        other => return Err(io::Error::other(format!("MachineGet answered {other:?}"))),
    };
    match client.call(ControlRequest::WorkspaceTree {
        workspace: machine_ws,
    }) {
        Ok(ReplyOk::WorkspaceTree(ws)) => Ok(mirror_from_machine(&machine, &ws)),
        Ok(other) => Err(io::Error::other(format!(
            "WorkspaceTree answered {other:?}"
        ))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            match client.call(ControlRequest::WorkspaceCreate {
                name: None,
                workspace: Some(machine_ws),
            })? {
                ReplyOk::WorkspaceTree(ws) => Ok(mirror_from_machine(&machine, &ws)),
                other => Err(io::Error::other(format!(
                    "WorkspaceCreate answered {other:?}"
                ))),
            }
        }
        Err(e) => Err(e),
    }
}

fn finish_prime(cx: &mut App, client_ws: WorkspaceId, epoch: u64, outcome: io::Result<WsMirror>) {
    let Some(state) = cx.default_global::<TreeSync>().windows.get_mut(&client_ws) else {
        return;
    };
    if state.epoch != epoch || !matches!(state.sync, SyncPhase::Unprimed { priming: true, .. }) {
        log::debug!("workspace {client_ws}: dropping a superseded tree pull");
        return;
    }
    let was_dirty = matches!(state.sync, SyncPhase::Unprimed { dirty: true, .. });
    let landed = match outcome {
        Ok(mirror) => {
            state.informed |= mirror.tabs.is_empty();
            let landed = (mirror.tabs.clone(), mirror.active);
            state.sync = SyncPhase::Primed(mirror);
            landed
        }
        Err(e) => {
            log::warn!("could not pull the tree for workspace {client_ws}: {e}");
            state.sync = SyncPhase::Unprimed {
                dirty: was_dirty,
                priming: false,
            };
            return;
        }
    };
    let host = WorkspaceStore::host_of(cx, client_ws);
    let machine_ws = tree_workspace_id(cx, client_ws);
    crate::ui::machine_mirror::MachineMirrors::note_synced_workspace(
        cx, host, machine_ws, landed.0, landed.1,
    );
    if !was_dirty {
        return;
    }
    let Some(app) =
        crate::ui::windows::WindowRegistry::app_for(cx, client_ws).and_then(|app| app.upgrade())
    else {
        return;
    };
    app.update(cx, |app, cx| sync_window(app, cx));
}

fn pump(cx: &mut App, client_ws: WorkspaceId) {
    let host = WorkspaceStore::host_of(cx, client_ws);
    let client = tree_control_for(cx, host);
    let state = cx
        .default_global::<TreeSync>()
        .windows
        .entry(client_ws)
        .or_default();
    if state.inflight || state.queue.is_empty() {
        return;
    }
    let client = match client {
        TreeLink::Ready(client) => client,
        TreeLink::Unserved => {
            desync(cx, client_ws, "the server does not serve the machine tree");
            return;
        }
        TreeLink::Down => {
            desync(cx, client_ws, "the control link is down");
            return;
        }
    };
    let batch: Vec<ControlRequest> = state.queue.drain(..).collect();
    state.inflight = true;
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                let mut committed = Vec::new();
                for op in batch {
                    if let Err(e) = client.call(op.clone()) {
                        return (committed, Err((op, e)));
                    }
                    committed.push(op);
                }
                (committed, Ok(()))
            })
            .await;
        cx.update(|cx| {
            if let Some(state) = cx.default_global::<TreeSync>().windows.get_mut(&client_ws) {
                state.inflight = false;
            }
            let (committed, result) = result;
            settle_remote_materializations(cx, client_ws, &committed);
            match result {
                Ok(()) => pump(cx, client_ws),
                Err((op, e)) => {
                    log::warn!("tree operation {op:?} failed: {e}; re-pulling the tree");
                    fail_remote_materialization(cx, client_ws, &op, e.to_string());
                    desync(cx, client_ws, "an operation was refused");
                }
            }
        });
    })
    .detach();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RemoteMaterializationFailure {
    pane: u64,
    kill_orphan: bool,
    retryable: bool,
}

fn materialized_pane(op: &ControlRequest) -> Option<u64> {
    match op {
        ControlRequest::TabCreate { pane, .. } => Some(pane.pane),
        ControlRequest::PaneSplit { new, .. } | ControlRequest::PaneReplace { new, .. } => {
            Some(new.pane)
        }
        _ => None,
    }
}

fn remote_materialization_failure(op: &ControlRequest) -> Option<RemoteMaterializationFailure> {
    materialized_pane(op).map(|pane| RemoteMaterializationFailure {
        pane,
        kill_orphan: true,
        retryable: true,
    })
}

fn settle_remote_materializations(
    cx: &mut App,
    client_ws: WorkspaceId,
    committed: &[ControlRequest],
) {
    let panes: Vec<u64> = committed.iter().filter_map(materialized_pane).collect();
    if panes.is_empty() {
        return;
    }
    let Some(app) =
        crate::ui::windows::WindowRegistry::app_for(cx, client_ws).and_then(|app| app.upgrade())
    else {
        return;
    };
    app.update(cx, |app, _cx| {
        for pane in panes {
            app.finish_remote_tree_materialization(pane);
        }
    });
}

fn fail_remote_materialization(
    cx: &mut App,
    client_ws: WorkspaceId,
    op: &ControlRequest,
    reason: String,
) {
    let Some(failure) = remote_materialization_failure(op) else {
        return;
    };
    let handled = crate::ui::windows::WindowRegistry::app_for(cx, client_ws)
        .and_then(|app| app.upgrade())
        .is_some_and(|app| {
            app.update(cx, |app, cx| {
                failure.retryable && app.fail_remote_tree_materialization(failure.pane, reason, cx)
            })
        });
    if !handled && failure.kill_orphan {
        let route = crate::ui::remote_workspace::pane_route_for(cx, client_ws);
        let pane = failure.pane;
        cx.background_executor()
            .spawn(async move {
                crate::terminal::RemoteTerminal::kill_pane_on(&route, pane);
            })
            .detach();
    }
}

fn desync(cx: &mut App, client_ws: WorkspaceId, why: &str) {
    log::info!("resynchronizing workspace {client_ws} with its machine ({why})");
    let Some(state) = cx.default_global::<TreeSync>().windows.get_mut(&client_ws) else {
        return;
    };
    state.queue.clear();
    state.inflight = false;
    state.sync = SyncPhase::Unprimed {
        dirty: true,
        priming: true,
    };
    state.epoch += 1;
    start_prime(cx, client_ws);
}

pub(crate) fn session_from_tree(
    ws: &agentty_core::core::machine::Workspace,
    panes: &[PaneRecord],
) -> Session {
    let tabs: Vec<SessionTab> = ws
        .tabs
        .iter()
        .map(|tab| SessionTab {
            name: tab.name.clone(),
            tree_id: Some(tab.id),
            sidebar_group: tab.sidebar_group.clone().map(std::path::PathBuf::from),
            pane: session_pane_from_node(&tab.root, panes),
        })
        .collect();
    let active = ws
        .active_tab
        .and_then(|id| ws.tabs.iter().position(|t| t.id == id))
        .unwrap_or(0);
    Session { active, tabs }
}

fn session_pane_from_node(node: &PaneNode, panes: &[PaneRecord]) -> SessionPane {
    match node {
        PaneNode::Leaf { pane } => {
            let record = panes.iter().find(|p| p.id == *pane);
            let live = record.is_some_and(|r| r.live);
            let (cwd, ssh_spec, agent) = match record {
                Some(r) => (
                    r.cwd.clone().map(std::path::PathBuf::from),
                    r.ssh_spec.clone(),
                    r.agent.clone(),
                ),
                None => (None, None, None),
            };
            SessionPane::Leaf {
                cwd,
                pane_id: live.then_some(*pane),
                ssh_spec,
                live_binding: agent.map_or_else(Default::default, |agent| {
                    let candidates = agentty_core::agent_runtime::SessionTitleCandidates::from_raw(
                        agent.provider_title.as_deref(),
                        agent.first_user_title.as_deref(),
                    );
                    agentty_core::core::session::LiveContainerBinding::restored_with_title_candidates(
                        agent
                            .container_id
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        Some(agent.agent),
                        agent.session_id,
                        agent.launch_argv.unwrap_or_default(),
                        candidates,
                    )
                }),
            }
        }
        PaneNode::Split { axis, ratio, a, b } => SessionPane::Split {
            axis: match axis {
                TreeAxis::Horizontal => crate::core::session::SessionAxis::Horizontal,
                TreeAxis::Vertical => crate::core::session::SessionAxis::Vertical,
            },
            ratio: *ratio,
            a: Box::new(session_pane_from_node(a, panes)),
            b: Box::new(session_pane_from_node(b, panes)),
        },
    }
}

const HYDRATE_LINK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);
const HYDRATE_LINK_POLL: std::time::Duration = std::time::Duration::from_millis(200);

pub(crate) fn hydrate_window_from_tree(cx: &mut App, client_ws: WorkspaceId) {
    hydrate(cx, client_ws, Adopt::IfEmpty);
}

#[derive(Clone, Copy, PartialEq)]
enum Adopt {
    IfEmpty,
    Replace,
}

fn begin_hydration(state: &mut WsState, adopt: Adopt) -> u64 {
    let dirty = matches!(state.sync, SyncPhase::Unprimed { dirty: true, .. });
    state.sync = SyncPhase::Unprimed {
        dirty,
        priming: true,
    };
    state.queue.clear();
    state.epoch += 1;
    // This attempt takes over the debt; it re-records it if it fails too.
    state.rehydrate = None;
    let _ = adopt;
    state.epoch
}

fn hydrate(cx: &mut App, client_ws: WorkspaceId, adopt: Adopt) {
    let host = WorkspaceStore::host_of(cx, client_ws);
    let machine_ws = tree_workspace_id(cx, client_ws);
    let epoch = begin_hydration(
        cx.default_global::<TreeSync>()
            .windows
            .entry(client_ws)
            .or_default(),
        adopt,
    );
    cx.spawn(async move |cx| {
        let deadline = std::time::Instant::now() + HYDRATE_LINK_DEADLINE;
        let client = loop {
            match cx.update(|cx| tree_control_for(cx, host)) {
                TreeLink::Ready(client) => break Some(client),
                TreeLink::Unserved => {
                    log::warn!(
                        "workspace {client_ws}: its machine's server does not serve the \
                         machine tree; opening empty"
                    );
                    break None;
                }
                TreeLink::Down if std::time::Instant::now() > deadline => {
                    log::warn!("workspace {client_ws}: no link to its machine; opening empty");
                    break None;
                }
                TreeLink::Down => cx.background_executor().timer(HYDRATE_LINK_POLL).await,
            }
        };
        let Some(client) = client else {
            cx.update(|cx| owe_rehydration(cx, client_ws, epoch, adopt));
            return;
        };
        let outcome = cx
            .background_executor()
            .spawn(async move { pull_workspace(&client, machine_ws) })
            .await;
        cx.update(|cx| finish_hydration(cx, client_ws, epoch, adopt, outcome));
    })
    .detach();
}

/// Records that a hydration failed and still owes `client_ws` its layout.
///
/// Nothing else recovers on its own: the window stays empty, and without this
/// the next `sync_window` would push that emptiness to the machine as "close
/// every tab". Instead the pull is retried the next time the window syncs —
/// which is what a reconnect does through `on_link_up`.
fn owe_rehydration(cx: &mut App, client_ws: WorkspaceId, epoch: u64, adopt: Adopt) {
    let Some(state) = cx.default_global::<TreeSync>().windows.get_mut(&client_ws) else {
        return;
    };
    if state.epoch != epoch {
        return;
    }
    if let SyncPhase::Unprimed { priming, .. } = &mut state.sync {
        *priming = false;
    }
    state.rehydrate = Some(adopt);
    log::info!("workspace {client_ws}: will pull its layout again once its machine answers");
    // While waiting to retry, do not strand an empty local window on welcome.
    if let Some(handle) = crate::ui::windows::WindowRegistry::window_for(cx, client_ws)
        && let Some(app) =
            crate::ui::windows::WindowRegistry::app_for(cx, client_ws).and_then(|a| a.upgrade())
    {
        let _ = handle.update(cx, move |_, window, cx| {
            app.update(cx, |app, cx| app.ensure_default_terminal(window, cx));
        });
    }
}

/// True while this window's machine tree hydrate is in flight (UI-STARTUP-TERMINAL-55).
pub(crate) fn window_is_tree_hydrating(cx: &App, client_ws: WorkspaceId) -> bool {
    cx.try_global::<TreeSync>()
        .and_then(|sync| sync.windows.get(&client_ws))
        .is_some_and(|state| matches!(state.sync, SyncPhase::Unprimed { priming: true, .. }))
}

fn pull_workspace(
    client: &ControlClient,
    machine_ws: WorkspaceId,
) -> io::Result<(Machine, WsMirror, Session)> {
    let machine: Machine = match client.call(ControlRequest::MachineGet)? {
        ReplyOk::MachineTree(m) => *m,
        other => return Err(io::Error::other(format!("MachineGet answered {other:?}"))),
    };
    match machine.workspaces.iter().find(|w| w.id == machine_ws) {
        Some(ws) => {
            let mirror = mirror_from_machine(&machine, ws);
            let session = session_from_tree(ws, &machine.panes);
            Ok((machine, mirror, session))
        }
        None => {
            client.call(ControlRequest::WorkspaceCreate {
                name: None,
                workspace: Some(machine_ws),
            })?;
            Ok((machine, WsMirror::default(), Session::default()))
        }
    }
}

fn finish_hydration(
    cx: &mut App,
    client_ws: WorkspaceId,
    epoch: u64,
    adopt: Adopt,
    outcome: io::Result<(Machine, WsMirror, Session)>,
) {
    let current = cx
        .default_global::<TreeSync>()
        .windows
        .get(&client_ws)
        .map(|s| s.epoch);
    if current != Some(epoch) {
        log::debug!("workspace {client_ws}: dropping a superseded hydration");
        return;
    }
    let (machine, mirror, session) = match outcome {
        Ok(pulled) => pulled,
        Err(e) => {
            log::warn!("could not hydrate workspace {client_ws} from its machine: {e}");
            owe_rehydration(cx, client_ws, epoch, adopt);
            return;
        }
    };
    let host = WorkspaceStore::host_of(cx, client_ws);
    crate::ui::machine_mirror::MachineMirrors::install(cx, host, machine);
    let was_dirty = {
        let Some(state) = cx.default_global::<TreeSync>().windows.get_mut(&client_ws) else {
            return;
        };
        let dirty = matches!(state.sync, SyncPhase::Unprimed { dirty: true, .. });
        state.informed |= mirror.tabs.is_empty();
        state.sync = SyncPhase::Primed(mirror);
        dirty
    };
    let Some(app) =
        crate::ui::windows::WindowRegistry::app_for(cx, client_ws).and_then(|app| app.upgrade())
    else {
        return;
    };
    if adopt == Adopt::IfEmpty && !app.read(cx).tabs.is_empty() {
        if was_dirty {
            app.update(cx, |app, cx| sync_window(app, cx));
        }
        return;
    }
    if session.tabs.is_empty() && adopt == Adopt::IfEmpty {
        let Some(handle) = crate::ui::windows::WindowRegistry::window_for(cx, client_ws) else {
            return;
        };
        mark_window_informed(cx, client_ws);
        let _ = handle.update(cx, move |_, window, cx| {
            app.update(cx, |app, cx| {
                if was_dirty {
                    sync_window(app, cx);
                }
                // UI-STARTUP-TERMINAL-55: empty tree must not strand the user on welcome.
                app.ensure_default_terminal(window, cx);
            });
        });
        return;
    }
    let Some(handle) = crate::ui::windows::WindowRegistry::window_for(cx, client_ws) else {
        return;
    };
    log::info!(
        "rebuilding {} tab(s) of workspace {client_ws} from its machine's tree",
        session.tabs.len()
    );
    mark_window_informed(cx, client_ws);
    let _ = handle.update(cx, move |_, window, cx| {
        app.update(cx, |app, cx| {
            app.adopt_workspace(client_ws, session, window, cx)
        });
    });
}

pub(crate) fn on_layout_delta(cx: &mut App, host: HostId, key: &str, delta: LayoutDelta) {
    crate::ui::machine_mirror::MachineMirrors::apply_delta(cx, host, key, &delta);
    let client_ws = if host.is_local() {
        key.parse::<WorkspaceId>().ok()
    } else {
        WorkspaceStore::all(cx)
            .windows
            .iter()
            .find(|w| {
                w.remote_ref()
                    .is_some_and(|r| r.host_id() == host && r.workspace.to_string() == key)
            })
            .map(|w| w.workspace)
    };
    let Some(client_ws) = client_ws else {
        return;
    };

    if crate::ui::remote_workspace::workspace_is_preempted(cx, client_ws) {
        on_preempted(cx, client_ws);
        return;
    }

    let mirror_ok = match cx
        .default_global::<TreeSync>()
        .windows
        .get_mut(&client_ws)
        .map(|s| &mut s.sync)
    {
        Some(SyncPhase::Primed(mirror)) => apply_to_mirror(mirror, &delta),
        _ => return,
    };

    let Some(app) =
        crate::ui::windows::WindowRegistry::app_for(cx, client_ws).and_then(|a| a.upgrade())
    else {
        return;
    };
    let Some(handle) = crate::ui::windows::WindowRegistry::window_for(cx, client_ws) else {
        return;
    };
    let window_ok = handle
        .update(cx, |_, window, cx| {
            app.update(cx, |app, cx| app.apply_layout_delta(&delta, window, cx))
        })
        .unwrap_or(true);
    if !mirror_ok || !window_ok {
        log::info!(
            "workspace {client_ws}: delta {delta:?} did not apply cleanly; re-pulling the tree"
        );
        resync_window_from_tree(cx, client_ws);
        return;
    }
    app.update(cx, |app, cx| sync_window(app, cx));
}

fn apply_to_mirror(mirror: &mut WsMirror, delta: &LayoutDelta) -> bool {
    match delta {
        LayoutDelta::WorkspaceCreated { .. }
        | LayoutDelta::WorkspaceRenamed { .. }
        | LayoutDelta::WorkspaceTouched { .. }
        | LayoutDelta::WorkspaceDeleted => true,
        LayoutDelta::PaneFacts { pane } => {
            remember_pane_title(mirror, pane);
            true
        }
        LayoutDelta::ActiveTabChanged { tab } => {
            mirror.active = Some(*tab);
            true
        }
        LayoutDelta::TabCreated { at, tab } => {
            if let Some(previous) = mirror.tabs.iter().find(|t| t.id == tab.id) {
                let previous = previous.root.clone();
                forget_tab_titles(mirror, &previous);
            }
            mirror.tabs.retain(|t| t.id != tab.id);
            let at = (*at).min(mirror.tabs.len());
            mirror.tabs.insert(at, tab.clone());
            true
        }
        LayoutDelta::TabClosed { tab } => {
            let before = mirror.tabs.len();
            if let Some(closed) = mirror.tabs.iter().find(|t| t.id == *tab) {
                let root = closed.root.clone();
                forget_tab_titles(mirror, &root);
            }
            mirror.tabs.retain(|t| t.id != *tab);
            if mirror.tabs.is_empty() {
                mirror.active = None;
            }
            mirror.tabs.len() != before
        }
        LayoutDelta::TabRenamed { tab, name } => {
            let Some(t) = mirror.tabs.iter_mut().find(|t| t.id == *tab) else {
                return false;
            };
            t.name = name.clone();
            true
        }
        LayoutDelta::TabRegrouped { tab, group } => {
            let Some(t) = mirror.tabs.iter_mut().find(|t| t.id == *tab) else {
                return false;
            };
            t.sidebar_group = group.clone();
            true
        }
        LayoutDelta::TabMoved { tab, to } => {
            let Some(from) = mirror.tabs.iter().position(|t| t.id == *tab) else {
                return false;
            };
            let moved = mirror.tabs.remove(from);
            mirror.tabs.insert((*to).min(mirror.tabs.len()), moved);
            true
        }
        LayoutDelta::TabRestructured { tab, pane } => {
            let Some(index) = mirror.tabs.iter().position(|t| t.id == tab.id) else {
                return false;
            };
            let old_ids = mirror.tabs[index].root.pane_ids();
            let new_ids = tab.root.pane_ids();
            for pane in old_ids {
                if !new_ids.contains(&pane) {
                    forget_pane_title_state(mirror, pane);
                }
            }
            mirror.tabs[index] = tab.clone();
            if let Some(record) = pane {
                remember_pane_title(mirror, record);
            }
            true
        }
        LayoutDelta::RatioChanged { tab, path, ratio } => {
            let Some(t) = mirror.tabs.iter_mut().find(|t| t.id == *tab) else {
                return false;
            };
            match t.root.descend_mut(path) {
                Some(PaneNode::Split { ratio: r, .. }) => {
                    *r = *ratio;
                    true
                }
                _ => false,
            }
        }
    }
}

fn remember_pane_title(mirror: &mut WsMirror, pane: &PaneRecord) {
    let Some(agent) = pane.agent.as_ref() else {
        // A PaneFacts snapshot without AgentFacts is authoritative negative
        // identity evidence.  Do not let a pane number reuse the previous
        // agent's identity to authorize a provider-title request.
        forget_pane_title_state(mirror, pane.id);
        return;
    };
    let identity = PaneTitleIdentity::from_facts(agent);
    if mirror
        .pane_agent_identities
        .get(&pane.id)
        .is_some_and(|previous| previous != &identity)
    {
        // A pane id was reused.  Typed evidence belongs to the old identity
        // and must not be sent to the replacement.
        forget_pane_title_state(mirror, pane.id);
    }
    mirror.pane_agent_identities.insert(pane.id, identity);
    if let Some(title) = agent
        .provider_title
        .as_deref()
        .and_then(agentty_core::agent_runtime::normalize_title_candidate)
    {
        mirror.pane_provider_titles.entry(pane.id).or_insert(title);
    }
    if let Some(title) = agent
        .first_user_title
        .as_deref()
        .and_then(agentty_core::agent_runtime::normalize_title_candidate)
    {
        mirror
            .pane_first_user_titles
            .entry(pane.id)
            .or_insert(title);
    }
}

pub(crate) fn resync_window_from_tree(cx: &mut App, client_ws: WorkspaceId) {
    hydrate(cx, client_ws, Adopt::Replace);
}

impl AgenttyApp {
    pub(crate) fn apply_layout_delta(
        &mut self,
        delta: &LayoutDelta,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let index_of = |tabs: &[crate::ui::app::Tab], id: TabId| {
            tabs.iter().position(|t| t.tree_id.get() == id)
        };
        let applied = match delta {
            LayoutDelta::WorkspaceCreated { .. }
            | LayoutDelta::WorkspaceTouched { .. }
            | LayoutDelta::WorkspaceRenamed { .. }
            | LayoutDelta::PaneFacts { .. } => true,
            LayoutDelta::WorkspaceDeleted => {
                log::info!(
                    "workspace {} was deleted on its machine; keeping the window",
                    self.workspace
                );
                true
            }
            LayoutDelta::ActiveTabChanged { tab } => {
                if let Some(index) = index_of(&self.tabs, *tab) {
                    self.activate_from_delta(index, window, cx);
                }
                true
            }
            LayoutDelta::TabCreated { at, tab } => {
                self.insert_tab_from_tree((*at).min(self.tabs.len()), tab, window, cx)
            }
            LayoutDelta::TabClosed { tab } => {
                if let Some(index) = index_of(&self.tabs, *tab) {
                    let active_id = self.tabs.get(self.active).map(|t| t.tree_id.get());
                    self.tabs.remove(index);
                    self.active = active_id
                        .and_then(|id| index_of(&self.tabs, id))
                        .unwrap_or_else(|| index.min(self.tabs.len().saturating_sub(1)));
                    self.maximized = None;
                    self.focus_active(window, cx);
                }
                true
            }
            LayoutDelta::TabRenamed { tab, name } => {
                if let Some(index) = index_of(&self.tabs, *tab) {
                    self.tabs[index].name = name.clone();
                }
                true
            }
            LayoutDelta::TabRegrouped { tab, group } => {
                if let Some(index) = index_of(&self.tabs, *tab) {
                    *self.tabs[index].sidebar_group.borrow_mut() =
                        group.clone().map(std::path::PathBuf::from);
                }
                true
            }
            LayoutDelta::TabMoved { tab, to } => {
                if let Some(from) = index_of(&self.tabs, *tab) {
                    let active_id = self.tabs.get(self.active).map(|t| t.tree_id.get());
                    let moved = self.tabs.remove(from);
                    self.tabs.insert((*to).min(self.tabs.len()), moved);
                    if let Some(id) = active_id
                        && let Some(index) = index_of(&self.tabs, id)
                    {
                        self.active = index;
                    }
                }
                true
            }
            LayoutDelta::TabRestructured { tab, .. } => match index_of(&self.tabs, tab.id) {
                Some(index) => self.rebuild_tab_from_tree(index, tab, window, cx),
                None => false,
            },
            LayoutDelta::RatioChanged { tab, path, ratio } => {
                if let Some(index) = index_of(&self.tabs, *tab) {
                    set_gui_ratio(&mut self.tabs[index].pane, path, *ratio)
                } else {
                    true
                }
            }
        };
        cx.notify();
        applied
    }

    fn activate_from_delta(
        &mut self,
        index: usize,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.active == index {
            return;
        }
        self.maximized = None;
        self.active = index;
        self.focus_active(window, cx);
    }

    fn insert_tab_from_tree(
        &mut self,
        at: usize,
        tab: &TreeTab,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if self.tabs.iter().any(|t| t.tree_id.get() == tab.id) {
            return true;
        }
        let mut existing = HashMap::new();
        let Some(pane) = self.build_pane_from_tree(&tab.root, &mut existing, window, cx) else {
            return false;
        };
        let gui = crate::ui::app::Tab::from_tree(tab, pane);
        self.tabs.insert(at, gui);
        if self.active >= at && self.tabs.len() > 1 {
            self.active += 1;
        }
        true
    }

    fn rebuild_tab_from_tree(
        &mut self,
        index: usize,
        tab: &TreeTab,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let remote = WorkspaceStore::all(cx)
            .get(self.workspace)
            .is_some_and(|w| w.is_remote());
        let mut existing: HashMap<u64, PaneSlot> = HashMap::new();
        let mut ssh_slots: Vec<PaneSlot> = Vec::new();
        for slot in self.tabs[index].pane.leaves() {
            let id = match &slot {
                PaneSlot::Ready(view) if remote && view.read(cx).ssh_spec().is_some() => {
                    ssh_slots.push(slot);
                    continue;
                }
                PaneSlot::Ready(view) => Some(view.read(cx).pane_id),
                PaneSlot::Connecting(pending) => pending.read(cx).spawn.restore_pane,
            };
            if let Some(id) = id {
                existing.insert(id, slot);
            }
        }
        let Some(pane) = self.build_pane_from_tree(&tab.root, &mut existing, window, cx) else {
            return false;
        };
        let pane = ssh_slots.into_iter().fold(pane, |tree, slot| {
            Pane::split_node(gpui::Axis::Horizontal, 0.5, tree, Pane::Leaf(slot))
        });
        let gui = &mut self.tabs[index];
        gui.pane = pane;
        gui.name = tab.name.clone();
        *gui.sidebar_group.borrow_mut() = tab.sidebar_group.clone().map(std::path::PathBuf::from);
        self.maximized = None;
        true
    }

    fn build_pane_from_tree(
        &self,
        node: &PaneNode,
        existing: &mut HashMap<u64, PaneSlot>,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> Option<Pane> {
        match node {
            PaneNode::Leaf { pane } => {
                if let Some(slot) = existing.remove(pane) {
                    return Some(Pane::Leaf(slot));
                }
                match crate::ui::app::new_terminal(
                    self.window_workspace(cx),
                    Some(self.workspace),
                    self.font_size,
                    None,
                    Some(*pane),
                    None,
                    window,
                    cx,
                ) {
                    Ok(slot) => Some(Pane::Leaf(slot)),
                    Err(e) => {
                        log::warn!("could not attach pane {pane} from a delta: {e}");
                        None
                    }
                }
            }
            PaneNode::Split { axis, ratio, a, b } => {
                let left = self.build_pane_from_tree(a, existing, window, cx);
                let right = self.build_pane_from_tree(b, existing, window, cx);
                match (left, right) {
                    (Some(a), Some(b)) => Some(Pane::split_node(
                        match axis {
                            TreeAxis::Horizontal => gpui::Axis::Horizontal,
                            TreeAxis::Vertical => gpui::Axis::Vertical,
                        },
                        *ratio,
                        a,
                        b,
                    )),
                    (one, other) => one.or(other),
                }
            }
        }
    }
}

fn set_gui_ratio(pane: &mut Pane, path: &[Side], ratio: f32) -> bool {
    match path.split_first() {
        None => match pane {
            Pane::Split { ratio: cell, .. } => {
                cell.set(ratio.clamp(0.05, 0.95));
                true
            }
            _ => false,
        },
        Some((side, rest)) => match pane {
            Pane::Split { a, b, .. } => match side {
                Side::A => set_gui_ratio(a, rest, ratio),
                Side::B => set_gui_ratio(b, rest, ratio),
            },
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_peer_without_the_machine_tree_bit_classifies_as_unserved() {
        use agentty_core::daemon::control::ControlHello;
        use agentty_core::host::local::LocalHost;
        use agentty_core::host::server::{Services, serve_with};

        let connect = |services: Services| {
            let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
            std::thread::spawn(move || {
                let _ = serve_with(server, LocalHost::new(), services);
            });
            let hello = ControlHello::host_rpc("test-token", "test-host");
            Arc::new(
                agentty_core::daemon::control::ControlClient::over_unix(
                    client,
                    &hello,
                    Box::new(|_| {}),
                )
                .unwrap(),
            )
        };

        let treeless = connect(Services::none());
        assert!(matches!(
            classify_tree_link(Some(treeless)),
            TreeLink::Unserved
        ));

        let dir = std::env::temp_dir().join(format!("agentty-treelink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = agentty_core::core::machine::MachineStore::open(
            dir.join(agentty_core::core::machine::MACHINE_FILE),
        );
        let serving = connect(Services::with_machine(store));
        assert!(matches!(
            classify_tree_link(Some(serving)),
            TreeLink::Ready(_)
        ));

        assert!(matches!(classify_tree_link(None), TreeLink::Down));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui::test]
    fn preemption_drops_the_mirror_the_queue_and_the_informed_licence(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            let ws = WorkspaceId::new();
            {
                let state = cx
                    .default_global::<TreeSync>()
                    .windows
                    .entry(ws)
                    .or_default();
                state.sync = SyncPhase::Primed(WsMirror::default());
                state.informed = true;
                state.queue.push_back(ControlRequest::Ping);
            }
            on_preempted(cx, ws);
            let state = &cx.default_global::<TreeSync>().windows[&ws];
            assert!(matches!(
                state.sync,
                SyncPhase::Unprimed {
                    dirty: false,
                    priming: false,
                }
            ));
            assert!(
                state.queue.is_empty(),
                "queued ops belong to the lost session"
            );
            assert!(
                !state.informed,
                "the licence to prune must not survive a takeover"
            );
        });
    }

    #[gpui::test]
    fn a_hydration_that_died_on_a_stale_link_is_owed_back(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let ws = WorkspaceId::new();
            let epoch = {
                let state = cx
                    .default_global::<TreeSync>()
                    .windows
                    .entry(ws)
                    .or_default();
                state.sync = SyncPhase::Unprimed {
                    dirty: false,
                    priming: true,
                };
                state.epoch
            };
            owe_rehydration(cx, ws, epoch, Adopt::Replace);
            let state = &cx.default_global::<TreeSync>().windows[&ws];
            assert!(
                matches!(state.sync, SyncPhase::Unprimed { priming: false, .. }),
                "the attempt is over; another one must be able to start"
            );
            assert!(
                state.rehydrate.is_some(),
                "dropping the failure here is what left the window on the home page"
            );

            // A newer attempt has already taken over — the loser must not
            // re-arm a retry behind its back.
            {
                let state = cx
                    .default_global::<TreeSync>()
                    .windows
                    .get_mut(&ws)
                    .unwrap();
                state.rehydrate = None;
                state.epoch += 1;
            }
            owe_rehydration(cx, ws, epoch, Adopt::Replace);
            assert!(
                cx.default_global::<TreeSync>().windows[&ws]
                    .rehydrate
                    .is_none()
            );
        });
    }

    #[test]
    fn link_up_hydration_preserves_pending_local_tree_commit() {
        let mut state = WsState::default();
        state.sync = SyncPhase::Unprimed {
            dirty: true,
            priming: false,
        };

        begin_hydration(&mut state, Adopt::IfEmpty);

        assert!(matches!(
            state.sync,
            SyncPhase::Unprimed {
                dirty: true,
                priming: true,
            }
        ));
        assert!(state.rehydrate.is_none());
    }

    #[test]
    fn failed_remote_tree_commit_cleans_orphan_and_surfaces_retryable_error() {
        let pane = 41;
        let failure = remote_materialization_failure(&ControlRequest::TabCreate {
            workspace: WorkspaceId::new(),
            at: Some(0),
            pane: PaneSeed::bare(pane),
            tab: Some(TabId::new()),
        })
        .expect("a refused pane-adopting operation must have an explicit cleanup plan");

        assert_eq!(failure.pane, pane);
        assert!(failure.kill_orphan);
        assert!(failure.retryable);
    }

    #[gpui::test]
    fn a_window_that_filled_up_while_owed_keeps_what_it_has(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let ws = WorkspaceId::new();
            let arm = |cx: &mut App, adopt| {
                cx.default_global::<TreeSync>()
                    .windows
                    .entry(ws)
                    .or_default()
                    .rehydrate = Some(adopt);
            };

            arm(cx, Adopt::Replace);
            assert!(
                take_rehydrate(cx, ws, true).is_some(),
                "an empty window is exactly the one that still needs its layout"
            );
            assert!(
                take_rehydrate(cx, ws, true).is_none(),
                "the debt is claimed once"
            );

            arm(cx, Adopt::Replace);
            assert!(
                take_rehydrate(cx, ws, false).is_none(),
                "replaying an older layout over the user's new tabs is worse than not retrying"
            );
            assert!(
                cx.default_global::<TreeSync>().windows[&ws]
                    .rehydrate
                    .is_none(),
                "and the dropped retry must not linger"
            );

            arm(cx, Adopt::IfEmpty);
            assert!(
                take_rehydrate(cx, ws, false).is_some(),
                "IfEmpty polices that itself, and still owes the mirror a pull"
            );
        });
    }

    #[test]
    fn a_ratio_delta_is_clamped_to_the_servers_band_not_a_narrower_one() {
        let mut pane = Pane::split_node(gpui::Axis::Horizontal, 0.5, Pane::Empty, Pane::Empty);
        assert!(set_gui_ratio(&mut pane, &[], 0.07));
        match &pane {
            Pane::Split { ratio, .. } => assert_eq!(ratio.get(), 0.07),
            _ => unreachable!("built as a split"),
        }
        assert!(set_gui_ratio(&mut pane, &[], 0.01));
        match &pane {
            Pane::Split { ratio, .. } => assert_eq!(ratio.get(), 0.05),
            _ => unreachable!("built as a split"),
        }
    }

    #[test]
    fn a_tab_created_delta_that_straddled_a_repull_lands_once_in_the_window_mirror() {
        let mut mirror = WsMirror::default();
        let delta = LayoutDelta::TabCreated {
            at: 0,
            tab: TreeTab::leaf(1),
        };
        assert!(apply_to_mirror(&mut mirror, &delta));
        assert!(apply_to_mirror(&mut mirror, &delta));
        assert_eq!(mirror.tabs.len(), 1);
    }

    #[test]
    fn pane_facts_delta_primes_the_title_mirror_without_overwriting_it() {
        use agentty_core::core::cli_agent::CLIAgent;

        let mut mirror = WsMirror::default();
        let pane = PaneRecord {
            id: 7,
            agent: Some(AgentFacts {
                agent: CLIAgent::Codex,
                container_id: None,
                session_id: None,
                launch_argv: None,
                status: None,
                provider_title: None,
                first_user_title: Some("Draw a fox".into()),
            }),
            ..PaneRecord::new(7)
        };
        assert!(apply_to_mirror(
            &mut mirror,
            &LayoutDelta::PaneFacts { pane: pane.clone() },
        ));
        assert_eq!(
            mirror.pane_first_user_titles.get(&7).map(String::as_str),
            Some("Draw a fox")
        );

        let mut later = pane;
        later.agent.as_mut().unwrap().first_user_title = Some("Later".into());
        assert!(apply_to_mirror(
            &mut mirror,
            &LayoutDelta::PaneFacts { pane: later },
        ));
        assert_eq!(
            mirror.pane_first_user_titles.get(&7).map(String::as_str),
            Some("Draw a fox"),
            "the mirror follows the machine's write-once policy"
        );
    }

    #[test]
    fn pane_facts_delta_preserves_provider_title_evidence() {
        use agentty_core::core::cli_agent::CLIAgent;

        let mut mirror = WsMirror::default();
        let pane = PaneRecord {
            id: 8,
            agent: Some(AgentFacts {
                agent: CLIAgent::Codex,
                container_id: None,
                session_id: None,
                launch_argv: None,
                status: None,
                provider_title: Some("Historical rollout".into()),
                first_user_title: Some("Draw a fox".into()),
            }),
            ..PaneRecord::new(8)
        };
        assert!(apply_to_mirror(
            &mut mirror,
            &LayoutDelta::PaneFacts { pane: pane.clone() },
        ));
        assert_eq!(
            mirror.pane_provider_titles.get(&8).map(String::as_str),
            Some("Historical rollout")
        );

        let mut later = pane;
        later.agent.as_mut().unwrap().provider_title = Some("Newer".into());
        assert!(apply_to_mirror(
            &mut mirror,
            &LayoutDelta::PaneFacts { pane: later },
        ));
        assert_eq!(
            mirror.pane_provider_titles.get(&8).map(String::as_str),
            Some("Historical rollout"),
            "provider evidence is retained write-once in the mirror"
        );
    }

    #[test]
    fn tab_restructure_clears_removed_pane_identity() {
        let tab_id = TabId::new();
        let mut mirror = WsMirror {
            tabs: vec![TreeTab {
                id: tab_id,
                name: None,
                sidebar_group: None,
                root: PaneNode::Leaf { pane: 31 },
            }],
            active: Some(tab_id),
            ..WsMirror::default()
        };
        let pane = machine_provider_record(
            31,
            Some("Historical rollout"),
            Some("container-31"),
            Some("session-31"),
        );
        assert!(apply_to_mirror(
            &mut mirror,
            &LayoutDelta::PaneFacts { pane },
        ));
        assert!(mirror.pane_agent_identities.contains_key(&31));

        assert!(apply_to_mirror(
            &mut mirror,
            &LayoutDelta::TabRestructured {
                tab: TreeTab {
                    id: tab_id,
                    name: None,
                    sidebar_group: None,
                    root: PaneNode::Leaf { pane: 32 },
                },
                pane: None,
            },
        ));
        assert!(!mirror.pane_first_user_titles.contains_key(&31));
        assert!(!mirror.pane_provider_titles.contains_key(&31));
        assert!(
            !mirror.pane_agent_identities.contains_key(&31),
            "a removed pane id must not retain authorization for a future title write"
        );
    }

    #[test]
    fn agentless_pane_facts_clear_all_title_identity_slots() {
        let mut mirror = WsMirror {
            tabs: vec![TreeTab::leaf(32)],
            ..WsMirror::default()
        };
        let pane = machine_provider_record(
            32,
            Some("Historical rollout"),
            Some("container-32"),
            Some("session-32"),
        );
        assert!(apply_to_mirror(
            &mut mirror,
            &LayoutDelta::PaneFacts { pane },
        ));
        assert!(apply_to_mirror(
            &mut mirror,
            &LayoutDelta::PaneFacts {
                pane: PaneRecord::new(32),
            },
        ));
        assert!(!mirror.pane_first_user_titles.contains_key(&32));
        assert!(!mirror.pane_provider_titles.contains_key(&32));
        assert!(
            !mirror.pane_agent_identities.contains_key(&32),
            "agent-less facts revoke the previous pane identity"
        );
    }

    #[test]
    fn reused_pane_after_agentless_facts_cannot_queue_provider_title() {
        let tab_id = TabId::new();
        let mut mirror = WsMirror {
            tabs: vec![TreeTab {
                id: tab_id,
                name: None,
                sidebar_group: None,
                root: PaneNode::Leaf { pane: 33 },
            }],
            active: Some(tab_id),
            ..WsMirror::default()
        };
        assert!(apply_to_mirror(
            &mut mirror,
            &LayoutDelta::PaneFacts {
                pane: machine_provider_record(
                    33,
                    Some("Historical rollout"),
                    Some("container-33"),
                    Some("session-33"),
                ),
            },
        ));
        assert!(apply_to_mirror(
            &mut mirror,
            &LayoutDelta::TabRestructured {
                tab: TreeTab {
                    id: tab_id,
                    name: None,
                    sidebar_group: None,
                    root: PaneNode::Leaf { pane: 34 },
                },
                pane: None,
            },
        ));
        assert!(apply_to_mirror(
            &mut mirror,
            &LayoutDelta::TabRestructured {
                tab: TreeTab {
                    id: tab_id,
                    name: None,
                    sidebar_group: None,
                    root: PaneNode::Leaf { pane: 33 },
                },
                pane: Some(PaneRecord::new(33)),
            },
        ));

        let ops = diff(
            WorkspaceId::new(),
            &mut mirror,
            &[tab(
                tab_id,
                DesiredNode::Leaf {
                    pane: 33,
                    seed: seed_with_provider_title(33, "Replacement rollout"),
                },
            )],
            Some(tab_id),
            SyncScope::Full,
            &[],
        );
        assert!(
            !ops.iter()
                .any(|op| matches!(op, ControlRequest::PaneSetProviderTitle { .. })),
            "pane-number reuse without fresh AgentFacts must not authorize a title write: {ops:?}"
        );
        assert!(!mirror.pane_agent_identities.contains_key(&33));
    }

    #[test]
    fn ready_desired_agent_facts_prefer_live_binding_over_runtime_fallback() {
        use agentty_core::core::cli_agent::{AgentSessionState, AgentStatus, CLIAgent};
        use agentty_core::core::session::LiveContainerBinding;

        let binding = LiveContainerBinding::restored_with_title_candidates(
            "container-persisted".into(),
            Some(CLIAgent::Codex),
            Some("persisted-session".into()),
            vec!["codex".into(), "--resume".into()],
            agentty_core::agent_runtime::SessionTitleCandidates::from_raw(
                Some("Persisted provider"),
                Some("Persisted title"),
            ),
        );
        let runtime = AgentSessionState {
            status: AgentStatus::Working,
            session_id: Some("runtime-session".into()),
            launch_argv: Some(vec!["runtime-agent".into()]),
            ..Default::default()
        };

        let facts = desired_agent_facts(&binding, None, Some(&runtime))
            .expect("a persisted binding supplies the agent facts after runtime exit");
        assert_eq!(facts.agent, CLIAgent::Codex);
        assert_eq!(facts.container_id.as_deref(), Some("container-persisted"));
        assert_eq!(facts.session_id.as_deref(), Some("persisted-session"));
        assert_eq!(
            facts.launch_argv.as_deref(),
            Some(["codex".to_string(), "--resume".to_string()].as_slice())
        );
        assert_eq!(facts.provider_title.as_deref(), Some("Persisted provider"));
        assert_eq!(facts.first_user_title.as_deref(), Some("Persisted title"));

        let conflicting_runtime =
            desired_agent_facts(&binding, Some(CLIAgent::Claude), Some(&runtime)).unwrap();
        assert_eq!(conflicting_runtime.agent, CLIAgent::Codex);
    }

    #[test]
    fn ready_desired_agent_facts_use_runtime_identity_only_when_binding_is_empty() {
        use agentty_core::core::cli_agent::{AgentSessionState, AgentStatus, CLIAgent};
        use agentty_core::core::session::LiveContainerBinding;

        let binding =
            LiveContainerBinding::restored("container-runtime".into(), None, None, Vec::new());
        let runtime = AgentSessionState {
            status: AgentStatus::Working,
            session_id: Some("runtime-session".into()),
            launch_argv: Some(vec!["runtime-agent".into()]),
            ..Default::default()
        };

        let facts = desired_agent_facts(&binding, Some(CLIAgent::Claude), Some(&runtime))
            .expect("runtime identity remains a fallback for an unstamped binding");
        assert_eq!(facts.agent, CLIAgent::Claude);
        assert_eq!(facts.container_id.as_deref(), Some("container-runtime"));
        assert_eq!(facts.session_id.as_deref(), Some("runtime-session"));
        assert_eq!(
            facts.launch_argv.as_deref(),
            Some(["runtime-agent".to_string()].as_slice())
        );
        assert_eq!(facts.first_user_title, None);
    }

    #[gpui::test]
    fn a_superseded_prime_result_does_not_roll_the_mirror_back(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let ws = WorkspaceId::new();
            let stale_epoch = {
                let state = cx
                    .default_global::<TreeSync>()
                    .windows
                    .entry(ws)
                    .or_default();
                state.sync = SyncPhase::Unprimed {
                    dirty: false,
                    priming: true,
                };
                state.epoch
            };
            let advanced = WsMirror {
                tabs: vec![TreeTab::leaf(7)],
                active: None,
                ..WsMirror::default()
            };
            {
                let state = cx
                    .default_global::<TreeSync>()
                    .windows
                    .get_mut(&ws)
                    .unwrap();
                state.epoch += 1;
                state.sync = SyncPhase::Primed(advanced.clone());
            }

            finish_prime(cx, ws, stale_epoch, Ok(WsMirror::default()));

            match &cx.default_global::<TreeSync>().windows[&ws].sync {
                SyncPhase::Primed(mirror) => assert_eq!(
                    *mirror, advanced,
                    "the stale pull's empty answer must not replace the advanced mirror"
                ),
                _ => panic!("the mirror was dropped entirely"),
            }
        });
    }

    fn seed(pane: u64) -> PaneSeed {
        PaneSeed {
            pane,
            cwd: Some(format!("/work/{pane}")),
            ssh_spec: None,
            agent: None,
        }
    }

    fn seed_with_first_user_title(pane: u64, title: &str) -> PaneSeed {
        let mut seed = seed(pane);
        seed.agent = Some(AgentFacts {
            agent: agentty_core::core::cli_agent::CLIAgent::Codex,
            container_id: Some(format!("container-{pane}")),
            session_id: Some(format!("session-{pane}")),
            launch_argv: Some(vec!["codex".into()]),
            status: None,
            provider_title: None,
            first_user_title: Some(title.into()),
        });
        seed
    }

    fn seed_with_provider_title(pane: u64, provider: &str) -> PaneSeed {
        let mut seed = seed(pane);
        seed.agent = Some(AgentFacts {
            agent: agentty_core::core::cli_agent::CLIAgent::Codex,
            container_id: Some(format!("container-{pane}")),
            session_id: Some(format!("session-{pane}")),
            launch_argv: Some(vec!["codex".into()]),
            status: None,
            provider_title: Some(provider.into()),
            first_user_title: None,
        });
        seed
    }

    fn provider_identity(pane: u64) -> PaneTitleIdentity {
        PaneTitleIdentity {
            agent: agentty_core::core::cli_agent::CLIAgent::Codex,
            container_id: Some(format!("container-{pane}")),
            session_id: Some(format!("session-{pane}")),
        }
    }

    fn leaf(pane: u64) -> DesiredNode {
        DesiredNode::Leaf {
            pane,
            seed: seed(pane),
        }
    }

    fn split(axis: TreeAxis, ratio: f32, a: DesiredNode, b: DesiredNode) -> DesiredNode {
        DesiredNode::Split {
            axis,
            ratio,
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    fn tab(id: TabId, root: DesiredNode) -> DesiredTab {
        DesiredTab {
            id,
            name: None,
            group: None,
            root,
        }
    }

    fn assert_converged(mirror: &WsMirror, desired: &[DesiredTab]) {
        assert_eq!(mirror.tabs.len(), desired.len());
        for (m, d) in mirror.tabs.iter().zip(desired) {
            assert_eq!(m.id, d.id);
            assert_eq!(m.name, d.name);
            assert_eq!(m.sidebar_group, d.group);
            assert_eq!(m.root, d.root.to_pane_node());
        }
    }

    #[test]
    fn opening_the_first_tab_emits_a_create_carrying_the_client_identity() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        let desired = vec![tab(id, leaf(7))];

        let ops = diff(ws, &mut mirror, &desired, Some(id), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![ControlRequest::TabCreate {
                workspace: ws,
                at: Some(0),
                pane: seed(7),
                tab: Some(id),
            }],
            "a created tab is active on the server, so no separate active op"
        );
        assert_converged(&mirror, &desired);
        assert_eq!(mirror.active, Some(id));
    }

    #[test]
    fn a_first_user_title_is_persisted_after_pane_materialization_and_only_once() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        let desired = vec![tab(
            id,
            DesiredNode::Leaf {
                pane: 7,
                seed: seed_with_first_user_title(7, "Draw a fox"),
            },
        )];

        let ops = diff(ws, &mut mirror, &desired, Some(id), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![ControlRequest::TabCreate {
                workspace: ws,
                at: Some(0),
                pane: seed_with_first_user_title(7, "Draw a fox"),
                tab: Some(id),
            },],
            "the complete seed persists the title atomically with the create"
        );
        assert_eq!(
            mirror.pane_first_user_titles.get(&7).map(String::as_str),
            Some("Draw a fox")
        );
        assert!(
            diff(ws, &mut mirror, &desired, Some(id), SyncScope::Full, &[]).is_empty(),
            "a title request is write-once in the optimistic mirror"
        );
    }

    #[test]
    fn an_existing_pane_gets_a_typed_first_user_title_request() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        diff(
            ws,
            &mut mirror,
            &[tab(id, leaf(7))],
            Some(id),
            SyncScope::Full,
            &[],
        );

        let desired = vec![tab(
            id,
            DesiredNode::Leaf {
                pane: 7,
                seed: seed_with_first_user_title(7, "Draw a fox"),
            },
        )];
        assert_eq!(
            diff(ws, &mut mirror, &desired, Some(id), SyncScope::Full, &[]),
            vec![ControlRequest::PaneSetFirstUserTitle {
                workspace: ws,
                pane: 7,
                title: "Draw a fox".into(),
            }]
        );
    }

    #[test]
    fn an_existing_pane_gets_a_typed_provider_title_request() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        diff(
            ws,
            &mut mirror,
            &[tab(id, leaf(7))],
            Some(id),
            SyncScope::Full,
            &[],
        );
        mirror.pane_agent_identities.insert(7, provider_identity(7));

        let desired = vec![tab(
            id,
            DesiredNode::Leaf {
                pane: 7,
                seed: seed_with_provider_title(7, "Historical rollout"),
            },
        )];
        assert_eq!(
            diff(ws, &mut mirror, &desired, Some(id), SyncScope::Full, &[]),
            vec![ControlRequest::PaneSetProviderTitle {
                workspace: ws,
                pane: 7,
                title: "Historical rollout".into(),
                expected_identity: Some(provider_identity(7)),
            }]
        );
    }

    #[test]
    fn provider_title_seed_is_persisted_after_pane_materialization_and_only_once() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        let desired = vec![tab(
            id,
            DesiredNode::Leaf {
                pane: 8,
                seed: seed_with_provider_title(8, "Historical rollout"),
            },
        )];

        let ops = diff(ws, &mut mirror, &desired, Some(id), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![ControlRequest::TabCreate {
                workspace: ws,
                at: Some(0),
                pane: seed_with_provider_title(8, "Historical rollout"),
                tab: Some(id),
            }]
        );
        assert_eq!(
            mirror.pane_provider_titles.get(&8).map(String::as_str),
            Some("Historical rollout")
        );
        assert_eq!(
            mirror.pane_agent_identities.get(&8),
            Some(&provider_identity(8))
        );
        assert!(
            diff(ws, &mut mirror, &desired, Some(id), SyncScope::Full, &[]).is_empty(),
            "a seeded provider title is write-once in the optimistic mirror"
        );
    }

    #[test]
    fn provider_title_requests_precede_first_user_title_requests() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        diff(
            ws,
            &mut mirror,
            &[tab(id, leaf(9))],
            Some(id),
            SyncScope::Full,
            &[],
        );
        mirror.pane_agent_identities.insert(9, provider_identity(9));
        let mut seed = seed_with_provider_title(9, "Historical rollout");
        seed.agent.as_mut().unwrap().first_user_title = Some("Draw a fox".into());

        let ops = diff(
            ws,
            &mut mirror,
            &[tab(id, DesiredNode::Leaf { pane: 9, seed })],
            Some(id),
            SyncScope::Full,
            &[],
        );
        assert!(matches!(
            ops.as_slice(),
            [
                ControlRequest::PaneSetProviderTitle { .. },
                ControlRequest::PaneSetFirstUserTitle { .. }
            ]
        ));
    }

    #[test]
    fn no_agent_pane_is_not_optimistically_marked_for_provider_title() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        diff(
            ws,
            &mut mirror,
            &[tab(id, leaf(10))],
            Some(id),
            SyncScope::Full,
            &[],
        );
        let desired = vec![tab(
            id,
            DesiredNode::Leaf {
                pane: 10,
                seed: seed_with_provider_title(10, "Historical rollout"),
            },
        )];
        assert!(
            diff(ws, &mut mirror, &desired, Some(id), SyncScope::Full, &[]).is_empty(),
            "without a MachineGet identity, provider migration waits instead of guessing"
        );
        assert!(!mirror.pane_provider_titles.contains_key(&10));
    }

    #[test]
    fn prime_mirror_reads_machine_provider_title_before_queueing() {
        let ws = agentty_core::core::machine::Workspace {
            id: WorkspaceId::new(),
            name: None,
            last_active: 0,
            tabs: vec![TreeTab::leaf(11)],
            active_tab: None,
            attachment: None,
        };
        let machine = Machine {
            workspaces: vec![ws.clone()],
            panes: vec![PaneRecord {
                id: 11,
                agent: Some(AgentFacts {
                    agent: agentty_core::core::cli_agent::CLIAgent::Codex,
                    container_id: Some("container-11".into()),
                    session_id: Some("session-11".into()),
                    launch_argv: Some(vec!["codex".into()]),
                    status: None,
                    provider_title: Some("Historical rollout".into()),
                    first_user_title: None,
                }),
                ..PaneRecord::new(11)
            }],
        };
        let mirror = mirror_from_machine(&machine, &ws);
        assert_eq!(
            mirror.pane_provider_titles.get(&11).map(String::as_str),
            Some("Historical rollout")
        );
        assert_eq!(
            mirror.pane_agent_identities.get(&11),
            Some(&provider_identity(11))
        );
    }

    #[test]
    fn existing_pane_provider_title_request_is_idempotent_after_authoritative_delta() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror {
            tabs: vec![TreeTab {
                id,
                name: None,
                sidebar_group: None,
                root: PaneNode::Leaf { pane: 12 },
            }],
            active: Some(id),
            ..WsMirror::default()
        };
        mirror
            .pane_agent_identities
            .insert(12, provider_identity(12));
        let desired = vec![tab(
            id,
            DesiredNode::Leaf {
                pane: 12,
                seed: seed_with_provider_title(12, "Historical rollout"),
            },
        )];
        let first = diff(ws, &mut mirror, &desired, Some(id), SyncScope::Full, &[]);
        assert!(matches!(
            first.as_slice(),
            [ControlRequest::PaneSetProviderTitle { .. }]
        ));

        // A subsequent authoritative PaneFacts delta is idempotent and does
        // not enqueue a second migration after reconnect/hydrate.
        let pane = PaneRecord {
            id: 12,
            agent: Some(AgentFacts {
                agent: agentty_core::core::cli_agent::CLIAgent::Codex,
                container_id: Some("container-12".into()),
                session_id: Some("session-12".into()),
                launch_argv: Some(vec!["codex".into()]),
                status: None,
                provider_title: Some("Historical rollout".into()),
                first_user_title: None,
            }),
            ..PaneRecord::new(12)
        };
        apply_to_mirror(&mut mirror, &LayoutDelta::PaneFacts { pane });
        assert!(diff(ws, &mut mirror, &desired, Some(id), SyncScope::Full, &[]).is_empty());
    }

    #[test]
    fn replace_hydrate_machine_missing_provider_title_fails_closed() {
        let workspace = agentty_core::core::machine::Workspace {
            id: WorkspaceId::new(),
            name: None,
            last_active: 0,
            tabs: vec![TreeTab::leaf(21)],
            active_tab: None,
            attachment: None,
        };
        let machine = Machine {
            workspaces: vec![workspace.clone()],
            panes: vec![machine_provider_record(
                21,
                None,
                Some("machine-container"),
                Some("machine-session"),
            )],
        };
        let session = session_from_tree(&workspace, &machine.panes);
        let SessionPane::Leaf { live_binding, .. } = &session.tabs[0].pane else {
            panic!("single-pane hydration remains a leaf");
        };
        assert_eq!(
            live_binding.provider_title(),
            None,
            "Replace must preserve machine-empty provider slot, not infer a local title"
        );

        let mut mirror = mirror_from_machine(&machine, &workspace);
        let desired = vec![tab(TabId::new(), leaf(21))];
        let ops = diff(
            workspace.id,
            &mut mirror,
            &desired,
            Some(desired[0].id),
            SyncScope::Full,
            &[],
        );
        assert!(
            !ops.iter()
                .any(|op| matches!(op, ControlRequest::PaneSetProviderTitle { .. }))
        );
        assert!(!mirror.pane_provider_titles.contains_key(&21));
    }

    #[test]
    fn replace_hydrate_machine_provider_title_wins() {
        let workspace = agentty_core::core::machine::Workspace {
            id: WorkspaceId::new(),
            name: None,
            last_active: 0,
            tabs: vec![TreeTab::leaf(22)],
            active_tab: None,
            attachment: None,
        };
        let machine = Machine {
            workspaces: vec![workspace.clone()],
            panes: vec![machine_provider_record(
                22,
                Some("Machine authority"),
                Some("machine-container"),
                Some("machine-session"),
            )],
        };
        let session = session_from_tree(&workspace, &machine.panes);
        let SessionPane::Leaf { live_binding, .. } = &session.tabs[0].pane else {
            panic!("single-pane hydration remains a leaf");
        };
        assert_eq!(
            live_binding.provider_title(),
            Some("Machine authority"),
            "Replace must project meaningful machine evidence"
        );
    }

    #[test]
    fn replace_hydrate_no_agent_does_not_fabricate_provider_title() {
        let workspace = agentty_core::core::machine::Workspace {
            id: WorkspaceId::new(),
            name: None,
            last_active: 0,
            tabs: vec![TreeTab::leaf(23)],
            active_tab: None,
            attachment: None,
        };
        let machine = Machine {
            workspaces: vec![workspace.clone()],
            panes: vec![PaneRecord::new(23)],
        };
        let session = session_from_tree(&workspace, &machine.panes);
        let SessionPane::Leaf {
            pane_id,
            live_binding,
            ..
        } = &session.tabs[0].pane
        else {
            panic!("single-pane hydration remains a leaf");
        };
        assert_eq!(*pane_id, None);
        assert_eq!(live_binding.provider_title(), None);
    }

    fn machine_provider_record(
        pane: u64,
        provider_title: Option<&str>,
        container_id: Option<&str>,
        session_id: Option<&str>,
    ) -> PaneRecord {
        PaneRecord {
            id: pane,
            agent: Some(AgentFacts {
                agent: agentty_core::core::cli_agent::CLIAgent::Codex,
                container_id: container_id.map(str::to_owned),
                session_id: session_id.map(str::to_owned),
                launch_argv: Some(vec!["codex".into()]),
                status: None,
                provider_title: provider_title.map(str::to_owned),
                first_user_title: None,
            }),
            ..PaneRecord::new(pane)
        }
    }

    #[test]
    fn a_split_emits_one_pane_split_against_its_sibling() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        let one = vec![tab(id, leaf(1))];
        diff(ws, &mut mirror, &one, Some(id), SyncScope::Full, &[]);

        let two = vec![tab(id, split(TreeAxis::Vertical, 0.5, leaf(1), leaf(2)))];
        let ops = diff(ws, &mut mirror, &two, Some(id), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![ControlRequest::PaneSplit {
                workspace: ws,
                pane: 1,
                axis: TreeAxis::Vertical,
                ratio: 0.5,
                new: seed(2),
                first: false,
            }]
        );
        assert_converged(&mirror, &two);
    }

    #[test]
    fn a_new_pane_on_the_upper_side_splits_with_first_set() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        diff(
            ws,
            &mut mirror,
            &[tab(id, leaf(1))],
            Some(id),
            SyncScope::Full,
            &[],
        );

        let want = vec![tab(id, split(TreeAxis::Horizontal, 0.4, leaf(2), leaf(1)))];
        let ops = diff(ws, &mut mirror, &want, Some(id), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![ControlRequest::PaneSplit {
                workspace: ws,
                pane: 1,
                axis: TreeAxis::Horizontal,
                ratio: 0.4,
                new: seed(2),
                first: true,
            }]
        );
        assert_converged(&mirror, &want);
    }

    #[test]
    fn closing_a_pane_emits_pane_close_and_the_split_collapses() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        diff(
            ws,
            &mut mirror,
            &[tab(id, split(TreeAxis::Vertical, 0.5, leaf(1), leaf(2)))],
            Some(id),
            SyncScope::Full,
            &[],
        );

        let want = vec![tab(id, leaf(1))];
        let ops = diff(ws, &mut mirror, &want, Some(id), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![ControlRequest::PaneClose {
                workspace: ws,
                pane: 2
            }]
        );
        assert_converged(&mirror, &want);
    }

    #[test]
    fn a_revived_leaf_emits_pane_replace_with_the_successors_seed() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        diff(
            ws,
            &mut mirror,
            &[tab(id, split(TreeAxis::Vertical, 0.5, leaf(1), leaf(2)))],
            Some(id),
            SyncScope::Full,
            &[],
        );

        let want = vec![tab(id, split(TreeAxis::Vertical, 0.5, leaf(1), leaf(9)))];
        let ops = diff(ws, &mut mirror, &want, Some(id), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![ControlRequest::PaneReplace {
                workspace: ws,
                old: 2,
                new: seed(9),
            }]
        );
        assert_converged(&mirror, &want);
    }

    #[test]
    fn a_ratio_drag_emits_set_ratio_with_the_splits_path() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        let nested = |r| {
            split(
                TreeAxis::Vertical,
                0.5,
                leaf(1),
                split(TreeAxis::Horizontal, r, leaf(2), leaf(3)),
            )
        };
        diff(
            ws,
            &mut mirror,
            &[tab(id, nested(0.5))],
            Some(id),
            SyncScope::Full,
            &[],
        );

        let want = vec![tab(id, nested(0.7))];
        let ops = diff(ws, &mut mirror, &want, Some(id), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![ControlRequest::PaneSetRatio {
                workspace: ws,
                tab: id,
                path: vec![Side::B],
                ratio: 0.7,
            }]
        );
        assert_converged(&mirror, &want);
    }

    #[test]
    fn closing_a_tab_emits_tab_close_and_heals_the_active_tab() {
        let ws = WorkspaceId::new();
        let (a, b) = (TabId::new(), TabId::new());
        let mut mirror = WsMirror::default();
        diff(
            ws,
            &mut mirror,
            &[tab(a, leaf(1)), tab(b, leaf(2))],
            Some(b),
            SyncScope::Full,
            &[],
        );

        let want = vec![tab(a, leaf(1))];
        let ops = diff(ws, &mut mirror, &want, None, SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![ControlRequest::TabClose {
                workspace: ws,
                tab: b
            }],
            "the heal is the server's own rule, so no active op crosses"
        );
        assert_converged(&mirror, &want);
        assert_eq!(mirror.active, Some(a));
    }

    #[test]
    fn tab_drag_merge_tree_diff_emits_one_pane_move_not_split_and_tab_close() {
        let ws = WorkspaceId::new();
        let (target, source) = (TabId::new(), TabId::new());
        let mut mirror = WsMirror::default();
        let before = vec![tab(target, leaf(1)), tab(source, leaf(2))];
        diff(ws, &mut mirror, &before, Some(target), SyncScope::Full, &[]);

        let want = vec![tab(
            target,
            split(TreeAxis::Horizontal, 0.5, leaf(1), leaf(2)),
        )];
        let ops = diff(ws, &mut mirror, &want, Some(target), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![ControlRequest::PaneMove {
                workspace: ws,
                pane: 2,
                to: 1,
                axis: TreeAxis::Horizontal,
                first: false,
            }],
            "a 1+1 drag merge must use the machine's atomic PaneMove primitive"
        );
        assert!(
            !ops.iter().any(|op| matches!(
                op,
                ControlRequest::PaneSplit { .. } | ControlRequest::TabClose { .. }
            )),
            "PaneSplit + TabClose can race and reject the duplicate pane"
        );
        assert_converged(&mirror, &want);
    }

    #[test]
    fn tab_drag_merge_removes_source_by_id_when_source_precedes_target() {
        let ws = WorkspaceId::new();
        let (target, source) = (TabId::new(), TabId::new());
        let mut mirror = WsMirror::default();
        let before = vec![tab(source, leaf(2)), tab(target, leaf(1))];
        diff(ws, &mut mirror, &before, Some(target), SyncScope::Full, &[]);

        let want = vec![tab(
            target,
            split(TreeAxis::Horizontal, 0.5, leaf(1), leaf(2)),
        )];
        let ops = diff(ws, &mut mirror, &want, Some(target), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![ControlRequest::PaneMove {
                workspace: ws,
                pane: 2,
                to: 1,
                axis: TreeAxis::Horizontal,
                first: false,
            }]
        );
        assert_converged(&mirror, &want);
    }

    #[test]
    fn an_invalid_duplicate_pane_merge_is_left_for_a_later_snapshot() {
        let ws = WorkspaceId::new();
        let (target, source) = (TabId::new(), TabId::new());
        let mut mirror = WsMirror::default();
        let before = vec![tab(target, leaf(1)), tab(source, leaf(2))];
        diff(ws, &mut mirror, &before, Some(target), SyncScope::Full, &[]);

        // A 1+2 target is outside the constrained merge contract, but it
        // still references the orphan's pane.  Do not rebuild the target (or
        // close the source) with a duplicate PaneSplit/TabCreate.
        let want = vec![tab(
            target,
            split(
                TreeAxis::Horizontal,
                0.5,
                leaf(1),
                split(TreeAxis::Vertical, 0.5, leaf(2), leaf(3)),
            ),
        )];
        let ops = diff(ws, &mut mirror, &want, Some(target), SyncScope::Full, &[]);
        assert!(
            ops.is_empty(),
            "invalid duplicate merge must fail closed: {ops:?}"
        );
        assert_eq!(
            mirror.tabs,
            before
                .iter()
                .map(|tab| TreeTab {
                    id: tab.id,
                    name: tab.name.clone(),
                    sidebar_group: tab.group.clone(),
                    root: tab.root.to_pane_node(),
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn tab_drag_merge_reconciles_a_non_default_split_ratio_after_pane_move() {
        let ws = WorkspaceId::new();
        let (target, source) = (TabId::new(), TabId::new());
        let mut mirror = WsMirror::default();
        let before = vec![tab(target, leaf(1)), tab(source, leaf(2))];
        diff(ws, &mut mirror, &before, Some(target), SyncScope::Full, &[]);

        let want = vec![tab(
            target,
            split(TreeAxis::Horizontal, 0.4, leaf(1), leaf(2)),
        )];
        let ops = diff(ws, &mut mirror, &want, Some(target), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![
                ControlRequest::PaneMove {
                    workspace: ws,
                    pane: 2,
                    to: 1,
                    axis: TreeAxis::Horizontal,
                    first: false,
                },
                ControlRequest::PaneSetRatio {
                    workspace: ws,
                    tab: target,
                    path: Vec::new(),
                    ratio: 0.4,
                },
            ]
        );
        assert_converged(&mirror, &want);
    }

    #[test]
    fn a_tab_reorder_emits_moves_that_land_the_windows_order() {
        let ws = WorkspaceId::new();
        let (a, b, c) = (TabId::new(), TabId::new(), TabId::new());
        let mut mirror = WsMirror::default();
        let before = [tab(a, leaf(1)), tab(b, leaf(2)), tab(c, leaf(3))];
        diff(ws, &mut mirror, &before, Some(c), SyncScope::Full, &[]);

        let want = vec![tab(c, leaf(3)), tab(a, leaf(1)), tab(b, leaf(2))];
        let ops = diff(ws, &mut mirror, &want, Some(c), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![ControlRequest::TabMove {
                workspace: ws,
                tab: c,
                to: 0
            }]
        );
        assert_converged(&mirror, &want);
    }

    #[test]
    fn renaming_and_regrouping_emit_their_label_ops() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        diff(
            ws,
            &mut mirror,
            &[tab(id, leaf(1))],
            Some(id),
            SyncScope::Full,
            &[],
        );

        let mut named = tab(id, leaf(1));
        named.name = Some("build".into());
        named.group = Some("/repo".into());
        let want = vec![named];
        let ops = diff(ws, &mut mirror, &want, Some(id), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![
                ControlRequest::TabRename {
                    workspace: ws,
                    tab: id,
                    name: Some("build".into()),
                },
                ControlRequest::TabSetGroup {
                    workspace: ws,
                    tab: id,
                    group: Some("/repo".into()),
                },
            ]
        );
        assert_converged(&mirror, &want);
    }

    #[test]
    fn switching_tabs_emits_only_set_active_tab() {
        let ws = WorkspaceId::new();
        let (a, b) = (TabId::new(), TabId::new());
        let mut mirror = WsMirror::default();
        let both = [tab(a, leaf(1)), tab(b, leaf(2))];
        diff(ws, &mut mirror, &both, Some(b), SyncScope::Full, &[]);

        let ops = diff(ws, &mut mirror, &both, Some(a), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![ControlRequest::WorkspaceSetActiveTab {
                workspace: ws,
                tab: a
            }]
        );
        assert_eq!(mirror.active, Some(a));
    }

    #[test]
    fn a_swap_no_single_op_expresses_rebuilds_the_tab_whole() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        diff(
            ws,
            &mut mirror,
            &[tab(id, split(TreeAxis::Vertical, 0.5, leaf(1), leaf(2)))],
            Some(id),
            SyncScope::Full,
            &[],
        );

        let want = vec![tab(id, split(TreeAxis::Vertical, 0.5, leaf(2), leaf(1)))];
        let ops = diff(ws, &mut mirror, &want, Some(id), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![
                ControlRequest::TabClose {
                    workspace: ws,
                    tab: id
                },
                ControlRequest::TabCreate {
                    workspace: ws,
                    at: Some(0),
                    pane: seed(2),
                    tab: Some(id),
                },
                ControlRequest::PaneSplit {
                    workspace: ws,
                    pane: 2,
                    axis: TreeAxis::Vertical,
                    ratio: 0.5,
                    new: seed(1),
                    first: false,
                },
            ]
        );
        assert_converged(&mirror, &want);
    }

    #[test]
    fn a_deep_tree_materializes_top_split_first_and_converges() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        let want = vec![tab(
            id,
            split(
                TreeAxis::Horizontal,
                0.6,
                split(TreeAxis::Vertical, 0.3, leaf(1), leaf(2)),
                split(TreeAxis::Vertical, 0.7, leaf(3), leaf(4)),
            ),
        )];
        let ops = diff(ws, &mut mirror, &want, Some(id), SyncScope::Full, &[]);
        assert_eq!(
            ops,
            vec![
                ControlRequest::TabCreate {
                    workspace: ws,
                    at: Some(0),
                    pane: seed(1),
                    tab: Some(id),
                },
                ControlRequest::PaneSplit {
                    workspace: ws,
                    pane: 1,
                    axis: TreeAxis::Horizontal,
                    ratio: 0.6,
                    new: seed(3),
                    first: false,
                },
                ControlRequest::PaneSplit {
                    workspace: ws,
                    pane: 1,
                    axis: TreeAxis::Vertical,
                    ratio: 0.3,
                    new: seed(2),
                    first: false,
                },
                ControlRequest::PaneSplit {
                    workspace: ws,
                    pane: 3,
                    axis: TreeAxis::Vertical,
                    ratio: 0.7,
                    new: seed(4),
                    first: false,
                },
            ]
        );
        assert_converged(&mirror, &want);
    }

    #[test]
    fn an_unchanged_window_emits_nothing() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        let want = vec![tab(id, split(TreeAxis::Vertical, 0.5, leaf(1), leaf(2)))];
        diff(ws, &mut mirror, &want, Some(id), SyncScope::Full, &[]);
        assert_eq!(
            diff(ws, &mut mirror, &want, Some(id), SyncScope::Full, &[]),
            Vec::new()
        );
    }

    #[test]
    fn a_tab_whose_panes_are_all_still_spawning_is_held_not_closed() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut mirror = WsMirror::default();
        diff(
            ws,
            &mut mirror,
            &[tab(id, leaf(1))],
            Some(id),
            SyncScope::Full,
            &[],
        );

        let ops = diff(ws, &mut mirror, &[], None, SyncScope::Full, &[id]);
        assert_eq!(ops, Vec::new());
        assert_eq!(mirror.tabs.len(), 1, "the daemon tab survives the wait");
    }

    #[test]
    fn an_additive_diff_never_closes_tabs_the_window_has_not_seen() {
        let ws = WorkspaceId::new();
        let (a, b) = (TabId::new(), TabId::new());
        let mut mirror = WsMirror::default();
        diff(
            ws,
            &mut mirror,
            &[tab(a, leaf(1)), tab(b, leaf(2))],
            Some(b),
            SyncScope::Full,
            &[],
        );

        let fresh = TabId::new();
        let ops = diff(
            ws,
            &mut mirror,
            &[tab(fresh, leaf(9))],
            Some(fresh),
            SyncScope::Additive,
            &[],
        );
        assert_eq!(
            ops,
            vec![ControlRequest::TabCreate {
                workspace: ws,
                at: Some(2),
                pane: seed(9),
                tab: Some(fresh),
            }],
            "appended after the tabs it has not seen; nothing closed or moved"
        );
        assert_eq!(mirror.tabs.len(), 3);
    }

    #[test]
    fn deltas_advance_the_mirror_exactly_as_the_writers_operations_did() {
        let ws = WorkspaceId::new();
        let id = TabId::new();
        let mut watcher = WsMirror::default();

        let tree_tab = TreeTab {
            id,
            name: None,
            sidebar_group: None,
            root: PaneNode::Leaf { pane: 1 },
        };
        assert!(apply_to_mirror(
            &mut watcher,
            &LayoutDelta::TabCreated {
                at: 0,
                tab: tree_tab,
            },
        ));
        assert!(apply_to_mirror(
            &mut watcher,
            &LayoutDelta::ActiveTabChanged { tab: id },
        ));
        assert!(apply_to_mirror(
            &mut watcher,
            &LayoutDelta::TabRestructured {
                tab: TreeTab {
                    id,
                    name: None,
                    sidebar_group: None,
                    root: PaneNode::Split {
                        axis: TreeAxis::Vertical,
                        ratio: 0.5,
                        a: Box::new(PaneNode::Leaf { pane: 1 }),
                        b: Box::new(PaneNode::Leaf { pane: 2 }),
                    },
                },
                pane: None,
            },
        ));
        assert!(apply_to_mirror(
            &mut watcher,
            &LayoutDelta::RatioChanged {
                tab: id,
                path: Vec::new(),
                ratio: 0.7,
            },
        ));

        let mut writer = WsMirror::default();
        diff(
            ws,
            &mut writer,
            &[tab(id, leaf(1))],
            Some(id),
            SyncScope::Full,
            &[],
        );
        let final_state = vec![tab(id, split(TreeAxis::Vertical, 0.7, leaf(1), leaf(2)))];
        diff(
            ws,
            &mut writer,
            &final_state,
            Some(id),
            SyncScope::Full,
            &[],
        );

        assert_eq!(watcher, writer);
    }

    #[test]
    fn a_delta_about_a_tab_the_mirror_does_not_hold_reports_itself() {
        let mut mirror = WsMirror::default();
        assert!(
            !apply_to_mirror(
                &mut mirror,
                &LayoutDelta::TabRenamed {
                    tab: TabId::new(),
                    name: Some("x".into()),
                },
            ),
            "an unappliable delta must say so, so the caller re-pulls"
        );
        assert!(!apply_to_mirror(
            &mut mirror,
            &LayoutDelta::TabClosed { tab: TabId::new() },
        ),);
    }

    #[test]
    fn a_live_leaf_keeps_its_pane_id_and_a_dead_one_lowers_to_a_revival_leaf() {
        use agentty_core::core::cli_agent::CLIAgent;
        let tab_id = TabId::new();
        let ws = agentty_core::core::machine::Workspace {
            tabs: vec![TreeTab {
                id: tab_id,
                name: Some("build".into()),
                sidebar_group: Some("/repo".into()),
                root: PaneNode::Split {
                    axis: TreeAxis::Vertical,
                    ratio: 0.3,
                    a: Box::new(PaneNode::Leaf { pane: 1 }),
                    b: Box::new(PaneNode::Leaf { pane: 2 }),
                },
            }],
            active_tab: Some(tab_id),
            ..Default::default()
        };
        let panes = vec![
            PaneRecord {
                id: 1,
                cwd: Some("/work".into()),
                live: true,
                ..PaneRecord::new(1)
            },
            PaneRecord {
                id: 2,
                cwd: Some("/work/api".into()),
                live: false,
                agent: Some(AgentFacts {
                    agent: CLIAgent::Claude,
                    container_id: Some("container-2".into()),
                    session_id: Some("sid".into()),
                    launch_argv: Some(vec!["claude".into()]),
                    status: None,
                    provider_title: None,
                    first_user_title: None,
                }),
                ..PaneRecord::new(2)
            },
        ];

        let session = session_from_tree(&ws, &panes);
        assert_eq!(session.tabs.len(), 1);
        assert_eq!(session.active, 0);
        let tab = &session.tabs[0];
        assert_eq!(
            tab.tree_id,
            Some(tab_id),
            "the daemon tab's identity rides along"
        );
        assert_eq!(tab.name.as_deref(), Some("build"));
        let SessionPane::Split { ratio, a, b, .. } = &tab.pane else {
            panic!("the split survives the lowering");
        };
        assert!((ratio - 0.3).abs() < 1e-6);
        match &**a {
            SessionPane::Leaf { pane_id, cwd, .. } => {
                assert_eq!(*pane_id, Some(1), "a live pane re-attaches by its id");
                assert_eq!(cwd.as_deref(), Some(std::path::Path::new("/work")));
            }
            _ => panic!("leaf"),
        }
        match &**b {
            SessionPane::Leaf {
                pane_id,
                cwd,
                live_binding,
                ..
            } => {
                assert_eq!(
                    *pane_id, None,
                    "a dead pane's leaf takes the fresh-spawn path — that is the revival"
                );
                assert_eq!(cwd.as_deref(), Some(std::path::Path::new("/work/api")));
                assert_eq!(live_binding.agent, Some(CLIAgent::Claude));
                assert_eq!(live_binding.session_id.as_deref(), Some("sid"));
                assert_eq!(live_binding.container_id, "container-2");
            }
            _ => panic!("leaf"),
        }
    }

    #[test]
    fn a_remote_hydrate_restores_the_persisted_first_user_title() {
        use agentty_core::core::cli_agent::CLIAgent;

        let tab_id = TabId::new();
        let ws = agentty_core::core::machine::Workspace {
            tabs: vec![TreeTab {
                id: tab_id,
                name: None,
                sidebar_group: None,
                root: PaneNode::Leaf { pane: 7 },
            }],
            active_tab: Some(tab_id),
            ..Default::default()
        };
        let panes = vec![PaneRecord {
            id: 7,
            live: true,
            agent: Some(AgentFacts {
                agent: CLIAgent::Codex,
                container_id: Some("container-7".into()),
                session_id: Some("session-7".into()),
                launch_argv: Some(vec!["codex".into()]),
                status: None,
                provider_title: Some("Historical provider title".into()),
                first_user_title: Some("Draw a fox".into()),
            }),
            ..PaneRecord::new(7)
        }];

        let session = session_from_tree(&ws, &panes);
        let SessionPane::Leaf { live_binding, .. } = &session.tabs[0].pane else {
            panic!("a single remote pane remains a leaf")
        };
        assert_eq!(live_binding.first_user_title(), Some("Draw a fox"));
        assert_eq!(
            live_binding.provider_title(),
            Some("Historical provider title")
        );
        assert_eq!(live_binding.container_id, "container-7");
    }

    #[test]
    fn a_dangling_active_tab_in_the_pulled_tree_falls_back_to_the_first() {
        let ws = agentty_core::core::machine::Workspace {
            tabs: vec![TreeTab {
                id: TabId::new(),
                name: None,
                sidebar_group: None,
                root: PaneNode::Leaf { pane: 1 },
            }],
            active_tab: Some(TabId::new()),
            ..Default::default()
        };
        assert_eq!(session_from_tree(&ws, &[]).active, 0);
    }

    #[test]
    fn a_pane_id_reused_in_another_tab_is_never_read_as_a_replace() {
        let ws = WorkspaceId::new();
        let (a, b) = (TabId::new(), TabId::new());
        let mut mirror = WsMirror::default();
        diff(
            ws,
            &mut mirror,
            &[tab(a, leaf(1)), tab(b, leaf(2))],
            Some(b),
            SyncScope::Full,
            &[],
        );

        let want = vec![tab(a, leaf(2)), tab(b, leaf(2))];
        let ops = diff(ws, &mut mirror, &want, Some(b), SyncScope::Full, &[]);
        assert!(
            !ops.iter()
                .any(|op| matches!(op, ControlRequest::PaneReplace { .. })),
            "got {ops:?}"
        );
    }
}
