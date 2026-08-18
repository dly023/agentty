pub use agentty_core::core::session::{
    EnvironmentWindow, EnvironmentWindows, RemoteRef, RemoteTarget, Session, SessionAxis,
    SessionPane, SessionTab, WorkspaceId,
};
pub use agentty_core::host::HostId;

#[cfg(test)]
pub use agentty_core::core::session::{WindowView, WindowViews};

pub struct WorkspaceStore {
    views: EnvironmentWindows,
}

impl gpui::Global for WorkspaceStore {}

fn select_remote_environment_window(
    views: &EnvironmentWindows,
    environment: &agentty_core::core::environment::EnvironmentId,
    remote_workspace: WorkspaceId,
) -> Option<WorkspaceId> {
    let exact = views
        .windows
        .iter()
        .filter(|window| &window.environment.id == environment)
        .filter(|window| window.remote_workspace == Some(remote_workspace))
        .max_by_key(|window| (window.open, window.last_active))
        .map(|window| window.workspace);

    exact.or_else(|| {
        views
            .latest_environment(environment)
            .map(|window| window.workspace)
    })
}

impl WorkspaceStore {
    pub fn init(cx: &mut gpui::App) {
        let views = EnvironmentWindows::load().unwrap_or_default();
        cx.set_global(Self { views });
    }

    #[cfg(test)]
    pub fn install_for_test(cx: &mut gpui::App, views: WindowViews) {
        cx.set_global(Self {
            views: EnvironmentWindows::from_legacy(views),
        });
    }

    pub fn all(cx: &gpui::App) -> &EnvironmentWindows {
        static EMPTY: std::sync::OnceLock<EnvironmentWindows> = std::sync::OnceLock::new();
        match cx.try_global::<Self>() {
            Some(store) => &store.views,
            None => EMPTY.get_or_init(EnvironmentWindows::default),
        }
    }

    fn try_store(cx: &mut gpui::App) -> Option<&mut Self> {
        cx.has_global::<Self>().then(|| cx.global_mut::<Self>())
    }

    pub fn claim(cx: &mut gpui::App, id: Option<WorkspaceId>) -> WorkspaceId {
        let Some(store) = Self::try_store(cx) else {
            return WorkspaceId::new();
        };
        let id = id.filter(|id| store.views.get_workspace(*id).is_some());
        let view = match id {
            Some(id) => store.views.get_workspace_mut(id).expect("filtered above"),
            None => {
                store.views.windows.push(EnvironmentWindow::default());
                store.views.windows.last_mut().expect("just pushed")
            }
        };
        view.open = true;
        view.touch();
        let claimed = view.workspace;
        store.views.active = Some(view.environment.id.clone());
        store.views.save();
        claimed
    }

    pub fn record_geometry(
        cx: &mut gpui::App,
        id: WorkspaceId,
        window: crate::core::window_state::WindowState,
    ) {
        let hint = Self::all(cx)
            .get_workspace(id)
            .and_then(|view| crate::ui::machine_mirror::display_hint(cx, view));
        let Some(store) = Self::try_store(cx) else {
            return;
        };
        let Some(view) = store.views.get_workspace_mut(id) else {
            return;
        };
        view.window = Some(window);
        if let Some((label, subject)) = hint {
            view.label = Some(label);
            view.subject = subject;
        }
        store.views.save();
    }

    pub fn focus(cx: &mut gpui::App, id: WorkspaceId) {
        let Some(store) = Self::try_store(cx) else {
            return;
        };
        if let Some(view) = store.views.get_workspace_mut(id) {
            view.touch();
            store.views.active = Some(view.environment.id.clone());
        }
        store.views.save();
        crate::ui::tree_sync::fire_workspace_op(cx, id, |ws| {
            agentty_core::daemon::control::ControlRequest::WorkspaceTouch { workspace: ws }
        });
    }

    pub fn restore_all(cx: &mut gpui::App) -> Vec<WorkspaceId> {
        let Some(store) = Self::try_store(cx) else {
            return Vec::new();
        };
        let restore = store.views.workspaces_to_restore();
        if restore.is_empty() {
            return Vec::new();
        }
        for workspace in &restore {
            if let Some(window) = store.views.get_workspace_mut(*workspace) {
                window.open = true;
            }
        }
        store.views.active = restore
            .last()
            .and_then(|workspace| store.views.get_workspace(*workspace))
            .map(|window| window.environment.id.clone());
        store.views.save();
        restore
    }

    pub fn close_window(cx: &mut gpui::App, id: WorkspaceId) {
        let hint = Self::all(cx)
            .get_workspace(id)
            .and_then(|view| crate::ui::machine_mirror::display_hint(cx, view));
        let Some(store) = Self::try_store(cx) else {
            return;
        };
        if let Some(view) = store.views.get_workspace_mut(id) {
            view.open = false;
            view.touch();
            if let Some((label, subject)) = hint {
                view.label = Some(label);
                view.subject = subject;
            }
        }
        store.views.save();
    }

    pub fn remove(cx: &mut gpui::App, id: WorkspaceId) {
        let Some(store) = Self::try_store(cx) else {
            return;
        };
        let removed_environment = store
            .views
            .get_workspace(id)
            .map(|w| w.environment.id.clone());
        store.views.windows.retain(|w| w.workspace != id);
        if store.views.active == removed_environment {
            store.views.active = None;
        }
        store.views.save();
    }

    pub fn host_of(cx: &gpui::App, id: WorkspaceId) -> HostId {
        Self::all(cx)
            .get(id)
            .map(EnvironmentWindow::host_id)
            .unwrap_or(HostId::LOCAL)
    }

    pub fn remote_ref(cx: &gpui::App, id: WorkspaceId) -> Option<RemoteRef> {
        Self::all(cx)
            .get_workspace(id)
            .and_then(EnvironmentWindow::remote_ref)
    }

    pub fn environment_id(
        cx: &gpui::App,
        id: WorkspaceId,
    ) -> agentty_core::core::environment::EnvironmentId {
        Self::all(cx)
            .get_workspace(id)
            .map(|window| window.environment.id.clone())
            .unwrap_or_default()
    }

    pub fn environment_workspace(
        cx: &gpui::App,
        environment: &agentty_core::core::environment::EnvironmentId,
    ) -> Option<WorkspaceId> {
        Self::all(cx)
            .latest_environment(environment)
            .map(|view| view.workspace)
    }

    pub fn claim_peer(cx: &mut gpui::App, source: WorkspaceId) -> Option<WorkspaceId> {
        let peer = Self::all(cx).get_workspace(source)?.peer();
        let workspace = peer.workspace;
        let environment = peer.environment.id.clone();
        let store = Self::try_store(cx)?;
        store.views.windows.push(peer);
        store.views.active = Some(environment);
        store.views.save();
        Some(workspace)
    }

    pub fn recover_unassigned_machine_workspaces(
        cx: &mut gpui::App,
        host: HostId,
        machine_workspaces: &[WorkspaceId],
    ) -> Vec<WorkspaceId> {
        let claimed: std::collections::HashSet<WorkspaceId> = Self::all(cx)
            .windows
            .iter()
            .filter(|window| window.host_id() == host)
            .map(|window| window.remote_workspace.unwrap_or(window.workspace))
            .collect();
        let template = Self::all(cx)
            .windows
            .iter()
            .filter(|window| window.host_id() == host)
            .max_by_key(|window| window.last_active)
            .cloned();
        if !host.is_local() && template.is_none() {
            return Vec::new();
        }
        let missing: Vec<_> = machine_workspaces
            .iter()
            .copied()
            .filter(|workspace| !claimed.contains(workspace))
            .collect();
        if missing.is_empty() {
            return Vec::new();
        }
        let Some(store) = Self::try_store(cx) else {
            return Vec::new();
        };
        let mut recovered = Vec::with_capacity(missing.len());
        for machine_workspace in missing {
            let mut window = match &template {
                Some(template) => template.peer(),
                None => EnvironmentWindow::default(),
            };
            if host.is_local() {
                window.workspace = machine_workspace;
                window.remote_workspace = None;
            } else {
                window.remote_workspace = Some(machine_workspace);
            }
            window.open = false;
            recovered.push(window.workspace);
            store.views.windows.push(window);
        }
        store.views.save();
        recovered
    }

    pub fn machine_is_connected(cx: &mut gpui::App, id: WorkspaceId) -> bool {
        let Some(host) = Self::remote_ref(cx, id) else {
            return true;
        };
        crate::ui::remote_connect::HostLinks::get(cx, host.host_id()).is_some()
    }

    pub fn claim_remote(cx: &mut gpui::App, host: RemoteRef) -> WorkspaceId {
        let Some(store) = Self::try_store(cx) else {
            return WorkspaceId::new();
        };
        let environment = agentty_core::core::environment::EnvironmentId::for_remote(&host.target);
        let existing = select_remote_environment_window(&store.views, &environment, host.workspace);
        let id = match existing {
            Some(id) => id,
            None => {
                let id = WorkspaceId::new();
                let view = EnvironmentWindow::remote(host.target, id, host.workspace);
                store.views.windows.push(view);
                id
            }
        };
        store.views.save();
        id
    }
}

#[cfg(test)]
fn host_for(views: &WindowViews, workspace: WorkspaceId) -> HostId {
    views
        .get(workspace)
        .map(WindowView::host_id)
        .unwrap_or(HostId::LOCAL)
}

pub(crate) fn crosses_machines(previous: HostId, current: HostId) -> bool {
    previous != current
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentty_core::core::session::{
        EnvironmentWindow, EnvironmentWindows, WindowView, WindowViews,
    };

    fn install_environment_views(cx: &mut gpui::App, windows: Vec<EnvironmentWindow>) {
        crate::core::config::pin_test_config_dir();
        cx.set_global(WorkspaceStore {
            views: EnvironmentWindows {
                version: EnvironmentWindows::VERSION,
                windows,
                active: None,
            },
        });
    }

    #[test]
    fn a_window_binds_to_exactly_one_machine() {
        let build = RemoteTarget::Alias {
            alias: "build-box".into(),
        };
        let gpu = RemoteTarget::direct("me", "gpu.lab", 2222);

        let local = WindowView::default();
        let build_a = WindowView::on_remote(RemoteRef::new(build.clone(), WorkspaceId::new()));
        let build_b = WindowView::on_remote(RemoteRef::new(build, WorkspaceId::new()));
        let gpu_a = WindowView::on_remote(RemoteRef::new(gpu, WorkspaceId::new()));
        let (local_id, build_a_id, build_b_id, gpu_id) =
            (local.id, build_a.id, build_b.id, gpu_a.id);

        let views = WindowViews {
            views: vec![local, build_a, build_b, gpu_a],
            ..WindowViews::default()
        };

        let l = host_for(&views, local_id);
        let b1 = host_for(&views, build_a_id);
        let b2 = host_for(&views, build_b_id);
        let g = host_for(&views, gpu_id);
        assert_eq!(l, HostId::LOCAL);
        assert_eq!(b1, b2, "two workspaces on one box share its connection");
        assert_ne!(b1, g);
        assert_ne!(b1, l);
        assert_ne!(g, l);

        assert_eq!(host_for(&views, build_a_id), b1);

        assert_eq!(host_for(&views, WorkspaceId::new()), HostId::LOCAL);

        assert!(!crosses_machines(b1, b2));
        assert!(crosses_machines(l, b1));
        assert!(crosses_machines(b1, g));
    }

    #[test]
    fn host_ids_group_by_machine_not_by_workspace() {
        let build = RemoteTarget::Alias {
            alias: "build-box".into(),
        };
        let other = RemoteTarget::Alias {
            alias: "other-box".into(),
        };
        let a = WindowView::on_remote(RemoteRef::new(build.clone(), WorkspaceId::new()));
        let b = WindowView::on_remote(RemoteRef::new(build, WorkspaceId::new()));
        let c = WindowView::on_remote(RemoteRef::new(other, WorkspaceId::new()));
        let local = WindowView::default();

        assert_eq!(a.host_id(), b.host_id());
        assert_ne!(a.host_id(), c.host_id());
        assert_eq!(local.host_id(), HostId::LOCAL);
        assert_ne!(a.host_id(), HostId::LOCAL);
    }

    #[gpui::test]
    fn window_restore_keeps_all_open_peer_windows(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            WorkspaceStore::install_for_test(cx, WindowViews::default());
            let first = WorkspaceStore::claim(cx, None);
            let second = WorkspaceStore::claim_peer(cx, first).expect("peer window");

            let restored = WorkspaceStore::restore_all(cx);

            assert_eq!(restored.len(), 2);
            assert!(restored.contains(&first));
            assert!(restored.contains(&second));
            assert!(
                restored.iter().all(|workspace| WorkspaceStore::all(cx)
                    .get_workspace(*workspace)
                    .is_some_and(|window| window.open)),
                "launch may not silently detach one of the persisted peer windows"
            );
        });
    }

    #[gpui::test]
    fn missing_window_record_recovers_unassigned_machine_workspace_without_deletion(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            WorkspaceStore::install_for_test(cx, WindowViews::default());
            let first = WorkspaceId::new();
            let second = WorkspaceId::new();

            let recovered = WorkspaceStore::recover_unassigned_machine_workspaces(
                cx,
                HostId::LOCAL,
                &[first, second],
            );

            assert_eq!(recovered.len(), 2);
            assert_eq!(WorkspaceStore::all(cx).windows.len(), 2);
            assert!(
                WorkspaceStore::all(cx)
                    .windows
                    .iter()
                    .all(|window| !window.open),
                "recovery keeps unassigned sessions discoverable without opening surprise windows"
            );
            assert!(WorkspaceStore::all(cx).get_workspace(first).is_some());
            assert!(WorkspaceStore::all(cx).get_workspace(second).is_some());

            assert!(
                WorkspaceStore::recover_unassigned_machine_workspaces(
                    cx,
                    HostId::LOCAL,
                    &[first, second],
                )
                .is_empty(),
                "reconciliation must be idempotent"
            );
            assert_eq!(WorkspaceStore::all(cx).windows.len(), 2);
        });
    }

    #[gpui::test]
    fn claim_remote_prefers_exact_remote_workspace_peer(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let target = RemoteTarget::Alias {
                alias: "build".into(),
            };
            let requested_remote_workspace = WorkspaceId::new();
            let mut first =
                EnvironmentWindow::remote(target.clone(), WorkspaceId::new(), WorkspaceId::new());
            first.open = true;
            first.last_active = 100;
            let mut exact = EnvironmentWindow::remote(
                target.clone(),
                WorkspaceId::new(),
                requested_remote_workspace,
            );
            exact.open = false;
            exact.last_active = 1;
            let exact_window = exact.workspace;
            install_environment_views(cx, vec![first, exact]);

            let claimed = WorkspaceStore::claim_remote(
                cx,
                RemoteRef::new(target, requested_remote_workspace),
            );

            assert_eq!(claimed, exact_window);
            assert_eq!(
                WorkspaceStore::remote_ref(cx, claimed)
                    .expect("exact peer remains remote")
                    .workspace,
                requested_remote_workspace
            );
            assert_eq!(WorkspaceStore::all(cx).windows.len(), 2);
        });
    }

    #[gpui::test]
    fn claim_remote_falls_back_to_latest_open_peer(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let target = RemoteTarget::Alias {
                alias: "build".into(),
            };
            let mut older_open =
                EnvironmentWindow::remote(target.clone(), WorkspaceId::new(), WorkspaceId::new());
            older_open.open = true;
            older_open.last_active = 10;
            let mut latest_open =
                EnvironmentWindow::remote(target.clone(), WorkspaceId::new(), WorkspaceId::new());
            latest_open.open = true;
            latest_open.last_active = 20;
            let mut newer_closed =
                EnvironmentWindow::remote(target.clone(), WorkspaceId::new(), WorkspaceId::new());
            newer_closed.open = false;
            newer_closed.last_active = 100;
            let expected = latest_open.workspace;
            install_environment_views(cx, vec![older_open, latest_open, newer_closed]);

            let claimed =
                WorkspaceStore::claim_remote(cx, RemoteRef::new(target, WorkspaceId::new()));

            assert_eq!(claimed, expected);
            assert_eq!(WorkspaceStore::all(cx).windows.len(), 3);
        });
    }

    #[gpui::test]
    fn claim_remote_allocates_only_when_environment_has_no_peer(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            install_environment_views(cx, Vec::new());
            let target = RemoteTarget::Alias {
                alias: "new-host".into(),
            };
            let requested_remote_workspace = WorkspaceId::new();

            let claimed = WorkspaceStore::claim_remote(
                cx,
                RemoteRef::new(target.clone(), requested_remote_workspace),
            );

            assert_eq!(WorkspaceStore::all(cx).windows.len(), 1);
            assert_eq!(
                WorkspaceStore::remote_ref(cx, claimed)
                    .expect("new claim is remote")
                    .workspace,
                requested_remote_workspace
            );
            assert_eq!(
                WorkspaceStore::environment_id(cx, claimed),
                agentty_core::core::environment::EnvironmentId::for_remote(&target)
            );
        });
    }
}
