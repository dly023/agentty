use gpui::{
    AnyWindowHandle, App, AppContext as _, BorrowAppContext as _, Bounds, Global, Styled as _,
    TitlebarOptions, WeakEntity, Window, WindowBounds, WindowOptions, point, px, size,
};
use gpui_component::{Root, TitleBar};

use crate::core::config::{Config, StartupMode};
use crate::core::session::{RemoteRef, RemoteTarget, WorkspaceId, WorkspaceStore};
use crate::core::window_state::{WindowGeometry as _, WindowState};
use crate::ui::app::AgenttyApp;
use agentty_core::core::environment::EnvironmentId;

const CASCADE_STEP: f32 = 28.0;

const DEFAULT_SIZE: (f32, f32) = (1440.0, 900.0);

struct WindowEntry {
    workspace: WorkspaceId,
    environment: EnvironmentId,
    handle: AnyWindowHandle,
    app: WeakEntity<AgenttyApp>,
}

#[derive(Clone)]
struct SessionSearchPartition {
    workspace: WorkspaceId,
    environment: EnvironmentId,
    documents: Vec<crate::ui::session_navigator::SessionSearchDocument>,
}

#[derive(Default)]
struct SessionSearchIndex {
    partitions: Vec<SessionSearchPartition>,
}

impl SessionSearchIndex {
    fn publish(
        &mut self,
        workspace: WorkspaceId,
        environment: EnvironmentId,
        documents: Vec<crate::ui::session_navigator::SessionSearchDocument>,
    ) {
        let documents = documents
            .into_iter()
            .filter(|document| {
                document.workspace == workspace && document.id.environment() == &environment
            })
            .collect();
        let partition = SessionSearchPartition {
            workspace,
            environment,
            documents,
        };
        if let Some(existing) = self
            .partitions
            .iter_mut()
            .find(|partition| partition.workspace == workspace)
        {
            *existing = partition;
        } else {
            self.partitions.push(partition);
        }
    }

    fn remove(&mut self, workspace: WorkspaceId) {
        self.partitions
            .retain(|partition| partition.workspace != workspace);
    }

    fn documents(&self) -> Vec<crate::ui::session_navigator::SessionSearchDocument> {
        self.partitions
            .iter()
            .flat_map(|partition| {
                partition
                    .documents
                    .iter()
                    .filter(|document| document.id.environment() == &partition.environment)
                    .cloned()
            })
            .collect()
    }
}

#[derive(Default)]
pub struct WindowRegistry {
    windows: Vec<WindowEntry>,
    session_search: SessionSearchIndex,
}

impl Global for WindowRegistry {}

impl WindowRegistry {
    pub fn init(cx: &mut App) {
        cx.set_global(Self::default());
    }

    pub fn count(cx: &mut App) -> usize {
        Self::sweep(cx);
        cx.global::<Self>().windows.len()
    }

    pub fn open_windows(cx: &mut App) -> Vec<(WorkspaceId, WeakEntity<AgenttyApp>)> {
        Self::sweep(cx);
        cx.global::<Self>()
            .windows
            .iter()
            .map(|w| (w.workspace, w.app.clone()))
            .collect()
    }

    pub fn window_for(cx: &mut App, workspace: WorkspaceId) -> Option<AnyWindowHandle> {
        Self::sweep(cx);
        cx.global::<Self>()
            .windows
            .iter()
            .find(|w| w.workspace == workspace)
            .map(|w| w.handle)
    }

    pub fn window_for_environment(
        cx: &mut App,
        environment: &EnvironmentId,
    ) -> Option<AnyWindowHandle> {
        Self::sweep(cx);
        let preferred = WorkspaceStore::all(cx)
            .latest_environment(environment)
            .map(|window| window.workspace);
        let registry = cx.global::<Self>();
        preferred
            .and_then(|workspace| {
                registry
                    .windows
                    .iter()
                    .find(|window| window.workspace == workspace)
            })
            .or_else(|| {
                registry
                    .windows
                    .iter()
                    .find(|window| &window.environment == environment)
            })
            .map(|window| window.handle)
    }

    pub fn most_recent(cx: &mut App) -> Option<WorkspaceId> {
        Self::sweep(cx);
        let active = WorkspaceStore::all(cx)
            .active
            .as_ref()
            .and_then(|environment| WorkspaceStore::all(cx).latest_environment(environment))
            .map(|window| window.workspace);
        let registry = cx.global::<Self>();
        active
            .filter(|id| registry.windows.iter().any(|w| w.workspace == *id))
            .or_else(|| registry.windows.first().map(|w| w.workspace))
    }

    pub fn app_in(cx: &mut App, window: &Window) -> Option<gpui::Entity<AgenttyApp>> {
        Self::sweep(cx);
        let handle = window.window_handle();
        cx.global::<Self>()
            .windows
            .iter()
            .find(|w| w.handle == handle)
            .and_then(|w| w.app.upgrade())
    }

    pub fn app_for(cx: &mut App, workspace: WorkspaceId) -> Option<WeakEntity<AgenttyApp>> {
        Self::sweep(cx);
        cx.global::<Self>()
            .windows
            .iter()
            .find(|w| w.workspace == workspace)
            .map(|w| w.app.clone())
    }

    fn register(
        cx: &mut App,
        workspace: WorkspaceId,
        handle: AnyWindowHandle,
        app: WeakEntity<AgenttyApp>,
    ) {
        let environment = WorkspaceStore::environment_id(cx, workspace);
        cx.global_mut::<Self>().windows.push(WindowEntry {
            workspace,
            environment,
            handle,
            app,
        });
    }

    pub fn unregister(cx: &mut App, workspace: WorkspaceId) {
        let registry = cx.global_mut::<Self>();
        registry.windows.retain(|w| w.workspace != workspace);
        registry.session_search.remove(workspace);
    }

    pub fn publish_session_documents(
        cx: &mut App,
        workspace: WorkspaceId,
        environment: EnvironmentId,
        documents: Vec<crate::ui::session_navigator::SessionSearchDocument>,
    ) {
        if !cx.has_global::<Self>() {
            return;
        }
        cx.global_mut::<Self>()
            .session_search
            .publish(workspace, environment, documents);
    }

    pub fn session_documents(cx: &App) -> Vec<crate::ui::session_navigator::SessionSearchDocument> {
        cx.try_global::<Self>()
            .map(|registry| registry.session_search.documents())
            .unwrap_or_default()
    }

    pub fn rebind(cx: &mut App, from: WorkspaceId, to: WorkspaceId) {
        let environment = WorkspaceStore::environment_id(cx, to);
        let registry = cx.global_mut::<Self>();
        registry.session_search.remove(from);
        if let Some(entry) = registry.windows.iter_mut().find(|w| w.workspace == from) {
            entry.workspace = to;
            entry.environment = environment;
        }
    }

    fn sweep(cx: &mut App) {
        let dead: Vec<WorkspaceId> = cx
            .global::<Self>()
            .windows
            .iter()
            .filter(|w| w.app.upgrade().is_none())
            .map(|w| w.workspace)
            .collect();
        if dead.is_empty() {
            return;
        }
        let registry = cx.global_mut::<Self>();
        registry.windows.retain(|w| !dead.contains(&w.workspace));
        for workspace in dead {
            registry.session_search.remove(workspace);
        }
    }
}

fn workspace_for_environment_target(
    views: &crate::core::session::EnvironmentWindows,
    target: Option<&RemoteTarget>,
) -> Option<WorkspaceId> {
    let environment = target.map(EnvironmentId::for_remote).unwrap_or_default();
    views
        .latest_environment(&environment)
        .map(|view| view.workspace)
}

pub fn open_or_focus_environment(
    cx: &mut App,
    target: Option<RemoteTarget>,
    remote_workspace: Option<WorkspaceId>,
) -> WorkspaceId {
    let environment = target
        .as_ref()
        .map(EnvironmentId::for_remote)
        .unwrap_or_default();
    if let Some(handle) = WindowRegistry::window_for_environment(cx, &environment) {
        activate_registered_window(cx, handle);
        return WorkspaceStore::environment_workspace(cx, &environment)
            .or(remote_workspace)
            .unwrap_or_else(WorkspaceId::new);
    }

    let workspace = match target {
        Some(target) => match remote_workspace {
            Some(daemon_workspace) => {
                WorkspaceStore::claim_remote(cx, RemoteRef::new(target, daemon_workspace))
            }
            None => workspace_for_environment_target(WorkspaceStore::all(cx), Some(&target))
                .unwrap_or_else(|| {
                    WorkspaceStore::claim_remote(cx, RemoteRef::new(target, WorkspaceId::new()))
                }),
        },
        None => workspace_for_environment_target(WorkspaceStore::all(cx), None)
            .unwrap_or_else(|| WorkspaceStore::claim(cx, None)),
    };
    open(cx, Some(workspace));
    workspace
}

pub fn open(cx: &mut App, workspace: Option<WorkspaceId>) {
    let _ = open_with_session(cx, workspace, None);
}

/// Bring a materialized application window to the foreground through one
/// canonical primitive. The immediate activation makes the result observable
/// to callers; the deferred pass protects menu/popup dismissal from restoring
/// focus to the window that dispatched the action.
fn activate_registered_window(cx: &mut App, handle: AnyWindowHandle) {
    cx.activate(true);
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    cx.defer(move |cx| {
        cx.activate(true);
        let _ = handle.update(cx, |_, window, _| window.activate_window());
    });
}

pub fn open_empty(cx: &mut App, workspace: WorkspaceId) -> Option<gpui::AnyWindowHandle> {
    open_with_session(
        cx,
        Some(workspace),
        Some(crate::core::session::Session::default()),
    )
}

pub fn open_with_session(
    cx: &mut App,
    workspace: Option<WorkspaceId>,
    session: Option<crate::core::session::Session>,
) -> Option<gpui::AnyWindowHandle> {
    let workspace = workspace.unwrap_or_else(|| WorkspaceStore::claim(cx, None));
    if let Some(handle) = WindowRegistry::window_for(cx, workspace) {
        activate_registered_window(cx, handle);
        return Some(handle);
    }

    let options = window_options(cx, Some(workspace));
    let mut created: Option<gpui::Entity<AgenttyApp>> = None;
    let app_session = session;
    let opened = cx.open_window(options, |window, cx| {
        let app = if let Some(session) = app_session.clone() {
            cx.new(|cx| AgenttyApp::with_session(Some(workspace), Some(session), window, cx))
        } else {
            cx.new(|cx| AgenttyApp::for_workspace(Some(workspace), window, cx))
        };
        created = Some(app.clone());
        cx.new(|cx| Root::new(app, window, cx).bg(gpui::transparent_black()))
    });

    let handle = match opened {
        Ok(handle) => handle,
        Err(e) => {
            log::error!("failed to open window: {e}");
            return None;
        }
    };
    let Some(app) = created else {
        log::error!("opened a window but its AgenttyApp was never built; not registering");
        return None;
    };

    let id = app.read(cx).workspace;
    let handle: AnyWindowHandle = handle.into();
    WindowRegistry::register(cx, id, handle, app.downgrade());
    activate_registered_window(cx, handle);
    refresh_menu(cx);
    Some(handle)
}

pub fn refresh_menu(cx: &mut App) {
    crate::ui::theme::set_menus(cx);
}

pub fn discard_empty_window(cx: &mut App, workspace: WorkspaceId) {
    let handle = WindowRegistry::window_for(cx, workspace);
    WindowRegistry::unregister(cx, workspace);
    crate::ui::tree_sync::forget(cx, workspace);
    WorkspaceStore::remove(cx, workspace);
    if let Some(handle) = handle {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
    refresh_menu(cx);
}

pub const MENU_SLOTS: usize = 9;

pub fn menu_order(cx: &App) -> Vec<(WorkspaceId, bool)> {
    let all = WorkspaceStore::all(cx);
    let mut open: Vec<_> = all.windows.iter().filter(|w| w.open).collect();
    let mut closed: Vec<_> = all.windows.iter().filter(|w| !w.open).collect();
    open.sort_by(|a, b| b.last_active.cmp(&a.last_active));
    closed.sort_by(|a, b| b.last_active.cmp(&a.last_active));
    open.into_iter()
        .map(|w| (w.workspace, true))
        .chain(closed.into_iter().map(|w| (w.workspace, false)))
        .take(MENU_SLOTS)
        .collect()
}

pub struct PaneCountQuery {
    route: crate::terminal::PaneRoute,
    claimed: Vec<u64>,
}

pub fn pane_count_query(cx: &App, workspace: WorkspaceId) -> Option<PaneCountQuery> {
    let ws = WorkspaceStore::all(cx).get(workspace)?;
    Some(PaneCountQuery {
        route: crate::ui::remote_workspace::pane_route_for(cx, workspace),
        claimed: crate::ui::machine_mirror::pane_ids(cx, ws)?,
    })
}

pub fn live_pane_count(q: &PaneCountQuery) -> Option<usize> {
    let PaneCountQuery { route, claimed } = q;
    if claimed.is_empty() {
        return Some(0);
    }
    match crate::terminal::RemoteTerminal::try_list_panes_on(route) {
        Ok(panes) => {
            let alive: std::collections::HashSet<u64> = panes
                .into_iter()
                .filter(|p| p.alive)
                .map(|p| p.pane_id)
                .collect();
            Some(claimed.iter().filter(|id| alive.contains(id)).count())
        }
        Err(_) if matches!(route, crate::terminal::PaneRoute::Local) => Some(0),
        Err(_) => None,
    }
}

pub fn confirm_and_stop(cx: &mut App, window: &mut Window, workspace: WorkspaceId) {
    confirm_destructive(cx, window, workspace, "Stop", stop_workspace);
}

pub fn confirm_and_delete(cx: &mut App, window: &mut Window, workspace: WorkspaceId) {
    confirm_destructive(cx, window, workspace, "Delete", delete_workspace);
}

fn destructive_detail(live: Option<usize>, verb: &str) -> String {
    match (live, verb) {
        (None, "Delete") => "Its machine could not be reached. Any shells still running there \
                             will be ended, and the layout forgotten."
            .to_string(),
        (None, _) => {
            "Its machine could not be reached. Any shells still running there will be ended."
                .to_string()
        }
        (Some(0), _) => "Its layout and working directories will be forgotten.".to_string(),
        (Some(1), "Delete") => {
            "1 running shell will be ended and its layout forgotten.".to_string()
        }
        (Some(n), "Delete") => {
            format!("{n} running shells will be ended and the layout forgotten.")
        }
        (Some(1), _) => "1 running shell will be ended.".to_string(),
        (Some(n), _) => format!("{n} running shells will be ended."),
    }
}

fn confirm_destructive(
    cx: &mut App,
    window: &mut Window,
    workspace: WorkspaceId,
    verb: &'static str,
    act: fn(&mut App, WorkspaceId),
) {
    let name = crate::ui::machine_mirror::display_name_for(cx, workspace)
        .unwrap_or_else(|| "this workspace".to_string());
    let query = pane_count_query(cx, workspace);
    let handle = window.window_handle();

    cx.spawn(async move |cx| {
        let live = match query {
            Some(q) => {
                cx.background_spawn(async move { live_pane_count(&q) })
                    .await
            }
            None => None,
        };

        if live == Some(0) && verb == "Stop" {
            let _ = cx.update(|cx| act(cx, workspace));
            return;
        }

        let detail = destructive_detail(live, verb);
        let Ok(answer) = handle.update(cx, |_, window, cx| {
            window.prompt(
                gpui::PromptLevel::Warning,
                &format!("{verb} Workspace \u{201c}{name}\u{201d}?"),
                Some(&detail),
                &["Cancel", verb],
                cx,
            )
        }) else {
            return;
        };

        if let Ok(1) = answer.await {
            let _ = cx.update(|cx| act(cx, workspace));
        }
    })
    .detach();
}

pub fn stop_workspace(cx: &mut App, workspace: WorkspaceId) {
    let doomed = doomed_pane_ids(cx, workspace);
    stop_workspace_keeping(cx, workspace, doomed);
}

fn doomed_pane_ids(cx: &App, workspace: WorkspaceId) -> Vec<u64> {
    WorkspaceStore::all(cx)
        .get(workspace)
        .and_then(|ws| crate::ui::machine_mirror::pane_ids(cx, ws))
        .unwrap_or_default()
}

fn stop_workspace_keeping(cx: &mut App, workspace: WorkspaceId, ids: Vec<u64>) {
    let route = crate::ui::remote_workspace::pane_route_for(cx, workspace);
    let host = WorkspaceStore::all(cx)
        .get(workspace)
        .map(|w| w.host_id())
        .unwrap_or(crate::ui::host_ops::HostId::LOCAL);
    if !ids.is_empty() {
        let route = route.clone();
        cx.background_executor()
            .spawn(async move {
                for pane_id in ids {
                    crate::terminal::RemoteTerminal::kill_pane_on(&route, pane_id);
                }
            })
            .detach();
    }
    if cx
        .try_global::<crate::terminal::pane_liveness::PaneLivenessCache>()
        .is_some()
    {
        cx.update_global::<crate::terminal::pane_liveness::PaneLivenessCache, _>(|cache, _| {
            cache.invalidate(host)
        });
    }
    if let Some(app) = WindowRegistry::app_for(cx, workspace)
        && let Some(app) = app.upgrade()
    {
        app.read(cx).teardown_workspace_forwards(cx);
    }
    close_window_for(cx, workspace);
    WorkspaceStore::close_window(cx, workspace);
    refresh_menu(cx);
}

pub fn delete_workspace(cx: &mut App, workspace: WorkspaceId) {
    let doomed = delete_from_tree(cx, workspace);
    stop_workspace_keeping(cx, workspace, doomed);
    WorkspaceStore::remove(cx, workspace);
    release_unused_hosts(cx);
    refresh_menu(cx);
}

fn delete_from_tree(cx: &mut App, workspace: WorkspaceId) -> Vec<u64> {
    let doomed = doomed_pane_ids(cx, workspace);
    crate::ui::tree_sync::fire_workspace_op(cx, workspace, |ws| {
        agentty_core::daemon::control::ControlRequest::WorkspaceRemove { workspace: ws }
    });
    crate::ui::tree_sync::forget(cx, workspace);
    doomed
}

fn release_unused_hosts(cx: &mut App) {
    let live: Vec<_> = WorkspaceStore::all(cx)
        .windows
        .iter()
        .filter(|w| w.is_remote())
        .map(|w| w.host_id())
        .collect();
    for id in crate::ui::host_registry::HostRegistry::ids(cx) {
        if !id.is_local() && !live.contains(&id) {
            crate::ui::remote_connect::HostLinks::remove(cx, id);
        }
    }
}

fn close_window_for(cx: &mut App, workspace: WorkspaceId) {
    let showing = WindowRegistry::app_for(cx, workspace);
    let Some(handle) = WindowRegistry::window_for(cx, workspace) else {
        return;
    };
    let Some(app) = showing.and_then(|weak| weak.upgrade()) else {
        return;
    };

    if WindowRegistry::count(cx) > 1 {
        WindowRegistry::unregister(cx, workspace);
        let _ = handle.update(cx, |_, window, _| window.remove_window());
        return;
    }

    let fresh = WorkspaceStore::claim(cx, None);
    WindowRegistry::rebind(cx, workspace, fresh);
    let _ = handle.update(cx, |_, window, cx| {
        app.update(cx, |app, cx| {
            app.adopt_workspace(fresh, crate::core::session::Session::default(), window, cx)
        });
    });
}

fn window_options(cx: &mut App, workspace: Option<WorkspaceId>) -> WindowOptions {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    static APP_ICON: std::sync::LazyLock<Option<std::sync::Arc<image::RgbaImage>>> =
        std::sync::LazyLock::new(|| {
            image::load_from_memory(include_bytes!("../../assets/app-icon.png"))
                .ok()
                .map(|image| std::sync::Arc::new(image.thumbnail(256, 256).into_rgba8()))
        });

    let remember = cx.global::<Config>().remember_window_size;
    let remembered = remember
        .then(|| {
            workspace
                .and_then(|id| WorkspaceStore::all(cx).get(id).and_then(|w| w.window))
                .or_else(WindowState::load)
        })
        .flatten();

    let existing = WindowRegistry::count(cx);
    let bounds = match remembered {
        Some(state) => {
            let bounds = state.bounds();
            if cx.displays().iter().any(|d| d.bounds().intersects(&bounds)) {
                bounds
            } else {
                Bounds::centered(None, bounds.size, cx)
            }
        }
        None => Bounds::centered(None, size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)), cx),
    };
    let bounds = cascade(bounds, existing);

    let window_bounds = match cx.global::<Config>().startup_mode {
        _ if existing > 0 => WindowBounds::Windowed(bounds),
        StartupMode::Normal => WindowBounds::Windowed(bounds),
        StartupMode::Maximized => WindowBounds::Maximized(bounds),
        StartupMode::Fullscreen => WindowBounds::Fullscreen(bounds),
    };

    WindowOptions {
        window_bounds: Some(window_bounds),
        app_id: Some("agentty".to_owned()),
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        icon: APP_ICON.as_ref().cloned(),
        titlebar: Some(TitlebarOptions {
            traffic_light_position: Some(crate::ui::theme::traffic_light_position()),
            ..TitleBar::title_bar_options()
        }),
        window_background: crate::ui::theme::background_appearance(cx),
        ..Default::default()
    }
}

fn cascade(bounds: Bounds<gpui::Pixels>, existing: usize) -> Bounds<gpui::Pixels> {
    if existing == 0 {
        return bounds;
    }
    let step = (existing % 5) as f32 * CASCADE_STEP;
    Bounds {
        origin: bounds.origin + point(px(step), px(step)),
        size: bounds.size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds_at(x: f32, y: f32) -> Bounds<gpui::Pixels> {
        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(800.), px(600.)),
        }
    }

    #[test]
    fn the_first_window_is_not_cascaded() {
        let b = bounds_at(100., 100.);
        assert_eq!(cascade(b, 0).origin, b.origin);
    }

    #[test]
    fn each_extra_window_steps_down_and_right() {
        let b = bounds_at(100., 100.);
        assert_eq!(
            cascade(b, 1).origin,
            point(px(100. + CASCADE_STEP), px(100. + CASCADE_STEP))
        );
        assert_eq!(
            cascade(b, 2).origin,
            point(px(100. + 2. * CASCADE_STEP), px(100. + 2. * CASCADE_STEP))
        );
        assert_eq!(cascade(b, 3).size, b.size);
    }

    #[test]
    fn cascade_wraps_so_windows_never_march_off_screen() {
        let b = bounds_at(100., 100.);
        assert_eq!(cascade(b, 5).origin, b.origin);
        assert_eq!(cascade(b, 6).origin, cascade(b, 1).origin);
    }

    #[test]
    fn the_confirmation_says_which_of_the_three_answers_it_got() {
        assert_eq!(
            destructive_detail(Some(1), "Stop"),
            "1 running shell will be ended."
        );
        assert_eq!(
            destructive_detail(Some(3), "Stop"),
            "3 running shells will be ended."
        );
        assert_eq!(
            destructive_detail(Some(1), "Delete"),
            "1 running shell will be ended and its layout forgotten."
        );
        assert_eq!(
            destructive_detail(Some(3), "Delete"),
            "3 running shells will be ended and the layout forgotten."
        );
        assert_eq!(
            destructive_detail(Some(0), "Delete"),
            "Its layout and working directories will be forgotten."
        );

        for verb in ["Stop", "Delete"] {
            let detail = destructive_detail(None, verb);
            assert!(
                detail.contains("could not be reached"),
                "{verb}: {detail:?} must say why there is no count"
            );
            assert!(
                !detail.contains("forgotten.") || verb == "Delete",
                "{verb}: {detail:?} promises a delete-only consequence"
            );
            assert!(
                !detail.chars().any(|c| c.is_ascii_digit()),
                "{verb}: {detail:?} states a count it does not have"
            );
        }
    }

    #[gpui::test]
    fn a_delete_reads_its_kill_list_before_the_removal_blanks_the_mirror(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::core::session::{WindowView, WindowViews};
        use agentty_core::core::machine::{Machine, PaneRecord, Tab, Workspace as TreeWorkspace};

        cx.update(|cx| {
            let view = WindowView::default();
            let id = view.id;
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![view],
                    active: None,
                },
            );
            crate::ui::machine_mirror::MachineMirrors::install(
                cx,
                crate::ui::host_ops::HostId::LOCAL,
                Machine {
                    workspaces: vec![TreeWorkspace {
                        id,
                        tabs: vec![Tab::leaf(1), Tab::leaf(2), Tab::leaf(3)],
                        ..TreeWorkspace::default()
                    }],
                    panes: vec![PaneRecord::new(1), PaneRecord::new(2), PaneRecord::new(3)],
                },
            );

            let doomed = delete_from_tree(cx, id);
            assert_eq!(
                doomed,
                vec![1, 2, 3],
                "every shell the confirm prompt counted must be on the kill list"
            );
            assert!(
                doomed_pane_ids(cx, id).is_empty(),
                "the removal has been folded into the mirror — which is exactly why \
                 the list must be read first"
            );
        });
    }
}

#[cfg(test)]
mod environment_tests {
    use super::*;
    use crate::core::session::{EnvironmentWindows, WindowView, WindowViews};

    #[test]
    fn window_environment_is_derived_from_its_persisted_authority() {
        let local = WindowView::default();
        let remote_target = RemoteTarget::Alias {
            alias: "build".into(),
        };
        let remote =
            WindowView::on_remote(RemoteRef::new(remote_target.clone(), WorkspaceId::new()));
        let local_id = local.id;
        let remote_id = remote.id;
        let views = EnvironmentWindows::from_legacy(WindowViews {
            views: vec![local, remote],
            active: None,
        });

        assert_eq!(
            workspace_for_environment_target(&views, None),
            Some(local_id)
        );
        assert_eq!(
            workspace_for_environment_target(&views, Some(&remote_target)),
            Some(remote_id)
        );
    }

    #[test]
    fn different_remote_targets_never_retarget_the_same_persisted_window() {
        let build = RemoteTarget::Alias {
            alias: "build".into(),
        };
        let gpu = RemoteTarget::Alias {
            alias: "gpu".into(),
        };
        let remote = WindowView::on_remote(RemoteRef::new(build.clone(), WorkspaceId::new()));
        let views = EnvironmentWindows::from_legacy(WindowViews {
            views: vec![remote],
            active: None,
        });

        assert!(workspace_for_environment_target(&views, Some(&build)).is_some());
        assert_eq!(workspace_for_environment_target(&views, Some(&gpu)), None);
        assert_eq!(workspace_for_environment_target(&views, None), None);
    }

    fn install_views(cx: &mut App, views: WindowViews) {
        crate::core::config::pin_test_config_dir();
        WorkspaceStore::install_for_test(cx, views);
    }

    fn alias(name: &str) -> RemoteTarget {
        RemoteTarget::Alias { alias: name.into() }
    }

    #[gpui::test]
    fn environment_menu_first_click_activates_and_reuses_same_window(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{MouseButton, VisualTestContext};

        const TRIGGER: &str = "ENVIRONMENT_MENU_TRIGGER";
        const TARGET: &str = "ENVIRONMENT_MENU_TARGET_PROFILE_00000000-0000-0000-0000-000000000000";

        fn click(selector: &'static str, visual: &mut VisualTestContext) {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("production selector {selector} must be rendered"));
            let point = gpui::point(
                bounds.origin.x + bounds.size.width / 2.,
                bounds.origin.y + bounds.size.height / 2.,
            );
            visual.simulate_mouse_move(point, None, gpui::Modifiers::none());
            visual.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::none());
            visual.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::none());
            visual.run_until_parked();
        }

        cx.executor().allow_parking();
        let source = WindowView::default();
        let source_id = source.id;
        let target_id = uuid::Uuid::nil();
        cx.update(|cx| {
            crate::core::config::pin_test_config_dir();
            gpui_component::init(cx);
            let mut config = crate::core::config::Config::default();
            let mut profile = crate::core::ssh_profile::SshProfile::new("first-click-target");
            profile.id = target_id;
            profile.host = "127.0.0.1".into();
            profile.port = 1;
            config.ssh_profiles.push(profile);
            cx.set_global(config);
            crate::ui::keymap::init(cx);
            WindowRegistry::init(cx);
            WorkspaceStore::install_for_test(
                cx,
                WindowViews {
                    views: vec![source.clone()],
                    active: Some(source_id),
                },
            );
        });

        let source_handle = cx.update(|cx| {
            open_with_session(
                cx,
                Some(source_id),
                Some(crate::core::session::Session::default()),
            )
            .expect("production source window should materialize")
        });
        source_handle
            .update(cx, |_, window, _| window.activate_window())
            .expect("source window should activate");
        let mut visual = VisualTestContext::from_window(source_handle, cx);
        visual.run_until_parked();

        assert_eq!(
            visual.update(|_, cx| cx.active_window()),
            Some(source_handle),
            "the source window is active before selecting another environment"
        );
        let before_count = visual.update(|_, cx| WindowRegistry::count(cx));

        click(TRIGGER, &mut visual);
        assert!(
            visual.debug_bounds(TARGET).is_some(),
            "the configured remote profile must be a real production menu row"
        );
        click(TARGET, &mut visual);

        let target_environment =
            EnvironmentId::for_remote(&RemoteTarget::Profile { id: target_id });
        let target_workspace = visual
            .update(|_, cx| WorkspaceStore::environment_workspace(cx, &target_environment))
            .expect("first click must claim a workspace for the target Environment");
        let first_status = visual.update(|_, cx| {
            crate::ui::remote_workspace::RemoteLinks::status_of(cx, target_workspace)
        });
        // Failed is still a supervised link state; Disconnected is the only
        // result that proves the host was never materialized into RemoteLinks.
        assert!(
            matches!(
                first_status,
                Some(crate::ui::remote_workspace::RemoteStatus::Connecting)
                    | Some(crate::ui::remote_workspace::RemoteStatus::Attached)
                    | Some(crate::ui::remote_workspace::RemoteStatus::Reconnecting { .. })
                    | Some(crate::ui::remote_workspace::RemoteStatus::Failed(_))
            ),
            "first click must leave the target host under RemoteLinks supervision, got {first_status:?}"
        );
        let target_handle = visual.update(|_, cx| {
            WindowRegistry::window_for_environment(cx, &target_environment)
                .expect("first click must publish the target window")
        });
        assert_ne!(target_handle, source_handle);
        assert_eq!(
            visual.update(|_, cx| cx.active_window()),
            Some(target_handle),
            "the first menu click must activate the newly registered native handle"
        );
        assert!(
            target_handle
                .update(&mut visual, |_, window, _| window.is_window_active())
                .expect("target handle should remain valid")
        );
        assert_eq!(
            visual.update(|_, cx| WorkspaceStore::all(cx).active.clone()),
            Some(target_environment.clone()),
            "window activation must focus the target Environment record"
        );
        assert_eq!(
            visual.update(|_, cx| WindowRegistry::count(cx)),
            before_count + 1,
            "first click creates exactly one target window"
        );

        source_handle
            .update(&mut visual, |_, window, _| window.activate_window())
            .expect("source window should be re-activatable for the repeat click");
        visual.run_until_parked();
        click(TRIGGER, &mut visual);
        click(TARGET, &mut visual);
        let target_workspace_again = visual
            .update(|_, cx| WorkspaceStore::environment_workspace(cx, &target_environment))
            .expect("repeat click keeps the target workspace claim");
        assert_eq!(
            target_workspace_again, target_workspace,
            "repeat click must not allocate a second workspace for the supervised host"
        );
        let target_handle_again = visual.update(|_, cx| {
            WindowRegistry::window_for_environment(cx, &target_environment)
                .expect("repeat click keeps the target registered")
        });
        assert_eq!(target_handle_again, target_handle);
        assert_eq!(
            visual.update(|_, cx| WindowRegistry::count(cx)),
            before_count + 1,
            "repeat click must not duplicate the target window"
        );
        let second_status = visual.update(|_, cx| {
            crate::ui::remote_workspace::RemoteLinks::status_of(cx, target_workspace)
        });
        assert!(
            matches!(
                second_status,
                Some(crate::ui::remote_workspace::RemoteStatus::Connecting)
                    | Some(crate::ui::remote_workspace::RemoteStatus::Attached)
                    | Some(crate::ui::remote_workspace::RemoteStatus::Reconnecting { .. })
                    | Some(crate::ui::remote_workspace::RemoteStatus::Failed(_))
            ),
            "repeat click must keep the same target host supervised, got {second_status:?}"
        );
        assert_eq!(
            visual.update(|_, cx| cx.active_window()),
            Some(target_handle)
        );
    }

    #[gpui::test]
    fn opening_same_environment_focuses_existing_window(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            install_views(cx, WindowViews::default());
            let target = alias("build");
            let first = WorkspaceStore::claim_remote(
                cx,
                RemoteRef::new(target.clone(), WorkspaceId::new()),
            );
            let window_count = WorkspaceStore::all(cx).windows.len();
            let second =
                WorkspaceStore::claim_remote(cx, RemoteRef::new(target, WorkspaceId::new()));
            assert_eq!(
                first, second,
                "opening an environment that already has a window must reuse it"
            );
            assert_eq!(
                WorkspaceStore::all(cx).windows.len(),
                window_count,
                "re-opening must not duplicate the window entry"
            );
            let environment = EnvironmentId::for_remote(&alias("build"));
            assert_eq!(
                WorkspaceStore::environment_workspace(cx, &environment),
                Some(first),
                "the environment resolves to the one window it already owns"
            );
        });
    }

    #[gpui::test]
    fn peer_window_preserves_environment_and_allocates_distinct_workspace(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            let remote = WindowView::on_remote(RemoteRef::new(alias("build"), WorkspaceId::new()));
            let source = remote.id;
            install_views(
                cx,
                WindowViews {
                    views: vec![remote],
                    active: None,
                },
            );
            let source_environment = WorkspaceStore::environment_id(cx, source);
            let source_machine_workspace = WorkspaceStore::remote_ref(cx, source)
                .expect("remote source")
                .workspace;

            let peer = WorkspaceStore::claim_peer(cx, source).expect("peer window");
            let peer_remote = WorkspaceStore::remote_ref(cx, peer).expect("remote peer");

            assert_ne!(peer, source, "application windows need distinct identities");
            assert_eq!(
                WorkspaceStore::environment_id(cx, peer),
                source_environment,
                "the peer remains in the same Environment"
            );
            assert_ne!(
                peer_remote.workspace, source_machine_workspace,
                "each peer window owns a distinct machine-tree workspace"
            );
        });
    }

    #[gpui::test]
    fn entering_remote_environment_never_retargets_current_window(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let local = WindowView::default();
            let local_id = local.id;
            install_views(
                cx,
                WindowViews {
                    views: vec![local],
                    active: None,
                },
            );

            let build = WorkspaceStore::claim_remote(
                cx,
                RemoteRef::new(alias("build"), WorkspaceId::new()),
            );
            assert_ne!(build, local_id);
            assert!(
                WorkspaceStore::environment_id(cx, local_id).is_local(),
                "entering a remote environment must not touch the current window's authority"
            );
            let local_view = WorkspaceStore::all(cx).get(local_id).expect("local view");
            assert!(local_view.open, "the current window stays open");

            let gpu =
                WorkspaceStore::claim_remote(cx, RemoteRef::new(alias("gpu"), WorkspaceId::new()));
            assert_ne!(gpu, build);
            assert_eq!(
                WorkspaceStore::environment_id(cx, build),
                EnvironmentId::for_remote(&alias("build")),
                "a second remote environment must not retarget the first remote window"
            );
        });
    }

    #[gpui::test]
    fn native_ssh_environment_action_opens_a_new_window(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            // The environment indicator's SSH action goes through
            // open_or_focus_environment; for a fresh target its store effect is
            // a dedicated claim. Pin that this never reuses the current window.
            let local = WindowView::default();
            let local_id = local.id;
            install_views(
                cx,
                WindowViews {
                    views: vec![local],
                    active: None,
                },
            );

            let claimed =
                WorkspaceStore::claim_remote(cx, RemoteRef::new(alias("tty7"), WorkspaceId::new()));
            assert_ne!(
                claimed, local_id,
                "a managed SSH environment gets its own window, not the current one"
            );
            assert_eq!(
                WorkspaceStore::environment_id(cx, claimed),
                EnvironmentId::for_remote(&alias("tty7"))
            );
            assert!(
                WorkspaceStore::environment_id(cx, local_id).is_local(),
                "the window the action was triggered from keeps its local authority"
            );
        });
    }

    #[gpui::test]
    fn terminal_ssh_does_not_mutate_window_environment(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            // Running `ssh host` inside a terminal is an OpenSSH child process:
            // nothing in that path calls into the workspace store. Pin that
            // environment authority and pane routing are derived solely from
            // the persisted window binding, so terminal content cannot change
            // them. script/check_environment_boundary greps for forbidden
            // couplings; this pins the derivation source at runtime.
            let local = WindowView::default();
            let local_id = local.id;
            install_views(
                cx,
                WindowViews {
                    views: vec![local],
                    active: None,
                },
            );

            assert!(WorkspaceStore::environment_id(cx, local_id).is_local());
            assert!(WorkspaceStore::remote_ref(cx, local_id).is_none());
            assert!(matches!(
                crate::ui::remote_workspace::pane_route_for(cx, local_id),
                crate::terminal::PaneRoute::Local
            ));
        });
    }

    #[gpui::test]
    fn remote_disconnect_never_becomes_local(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let target = alias("build");
            let remote = WindowView::on_remote(RemoteRef::new(target.clone(), WorkspaceId::new()));
            let remote_id = remote.id;
            install_views(
                cx,
                WindowViews {
                    views: vec![remote],
                    active: None,
                },
            );

            // No HostLinks entry: the connection is down. Authority must not
            // fall back to local just because the link is gone.
            assert!(
                !WorkspaceStore::machine_is_connected(cx, remote_id),
                "test setup: the remote is disconnected"
            );
            assert_eq!(
                WorkspaceStore::environment_id(cx, remote_id),
                EnvironmentId::for_remote(&target),
                "a disconnected remote window keeps its remote authority"
            );
            assert!(
                WorkspaceStore::remote_ref(cx, remote_id).is_some(),
                "the persisted remote binding survives the disconnect"
            );
        });
    }
}

#[cfg(test)]
mod session_search_registry_tests {
    use super::SessionSearchIndex;
    use crate::core::session::WorkspaceId;
    use crate::ui::palette::session_commands_from_documents;
    use crate::ui::session_navigator::{SessionSearchDocument, SessionSearchDocumentId};
    use agentty_core::agent_runtime::{AgentSessionKey, AgentSessionRecord, SessionNavigator};
    use agentty_core::core::environment::EnvironmentId;

    fn document(workspace: WorkspaceId, environment: EnvironmentId) -> SessionSearchDocument {
        let mut navigator = SessionNavigator::default();
        navigator.refresh(
            &[AgentSessionRecord {
                key: AgentSessionKey {
                    provider: "codex".into(),
                    session_id: "session-1".into(),
                },
                agent: crate::core::cli_agent::CLIAgent::Codex,
                title: Some("Fix remote discovery".into()),
                title_candidates: Default::default(),
                cwd: Some("/repo".into()),
                updated_at_unix_ms: None,
                launch_argv: Vec::new(),
                source_path: None,
                created_at_unix_ms: None,
            }],
            &[],
        );
        let row = &navigator.rows()[0];
        SessionSearchDocument {
            id: SessionSearchDocumentId::new(environment, row.row_id.clone()),
            workspace,
            title: row.title.clone().unwrap(),
            subtitle: "Codex · local".into(),
            search_text: "codex /repo session-1".into(),
        }
    }

    #[test]
    fn palette_search_uses_committed_rows_without_discovery() {
        let workspace = WorkspaceId::new();
        let environment = EnvironmentId::local();
        let mut index = SessionSearchIndex::default();
        index.publish(
            workspace,
            environment.clone(),
            vec![document(workspace, environment)],
        );

        let commands = session_commands_from_documents(index.documents());
        assert_eq!(commands.len(), 1);
        assert!(commands[0].title.contains("Fix remote discovery"));
    }

    #[test]
    fn closing_window_removes_its_session_search_partition() {
        let first = WorkspaceId::new();
        let second = WorkspaceId::new();
        let mut index = SessionSearchIndex::default();
        index.publish(
            first,
            EnvironmentId::local(),
            vec![document(first, EnvironmentId::local())],
        );
        let remote: EnvironmentId = "ssh:build".parse().unwrap();
        index.publish(second, remote.clone(), vec![document(second, remote)]);

        index.remove(first);

        let documents = index.documents();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].workspace, second);
    }
}
