pub use tty7_core::core::session::{
    EnvironmentWindow, EnvironmentWindows, RemoteRef, RemoteTarget, Session, SessionAxis,
    SessionPane, SessionTab, WorkspaceId,
};
pub use tty7_core::host::HostId;

#[cfg(test)]
pub use tty7_core::core::session::{WindowView, WindowViews};

pub struct WorkspaceStore {
    views: EnvironmentWindows,
}

impl gpui::Global for WorkspaceStore {}

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
            tty7_core::daemon::control::ControlRequest::WorkspaceTouch { workspace: ws }
        });
    }

    pub fn restore_one(cx: &mut gpui::App) -> Option<WorkspaceId> {
        let store = Self::try_store(cx)?;
        let keep = store.views.workspace_to_restore()?;
        let reattaching = store
            .views
            .get_workspace(keep)
            .is_some_and(|view| !view.open);
        let mut detached = 0usize;
        for view in &mut store.views.windows {
            if view.open && view.workspace != keep {
                view.open = false;
                detached += 1;
            }
        }
        store.views.active = store
            .views
            .get_workspace(keep)
            .map(|view| view.environment.id.clone());
        store.views.save();
        if reattaching {
            log::info!("launch: no window was open at quit; reattaching the last one closed");
        } else if detached > 0 {
            log::info!("launch: restoring 1 workspace, left {detached} detached");
        }
        Some(keep)
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
    ) -> tty7_core::core::environment::EnvironmentId {
        Self::all(cx)
            .get_workspace(id)
            .map(|window| window.environment.id.clone())
            .unwrap_or_default()
    }

    pub fn environment_workspace(
        cx: &gpui::App,
        environment: &tty7_core::core::environment::EnvironmentId,
    ) -> Option<WorkspaceId> {
        Self::all(cx)
            .get_environment(environment)
            .map(|view| view.workspace)
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
        let environment = tty7_core::core::environment::EnvironmentId::for_remote(&host.target);
        let existing = store
            .views
            .get_environment(&environment)
            .map(|w| w.workspace);
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
    use tty7_core::core::session::{WindowView, WindowViews};

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
}
