//! A view owns cancellation, never a blocking target subscription handle.
use gpui::BackgroundExecutor;
use smol::channel::{Receiver, Sender, bounded};
use tty7_core::agent_sessions::HistorySnapshot;
use tty7_core::agent_sessions::live_titles::{HistoryTitle, HistoryTitleEvent};
use tty7_core::host::SharedHost;
use uuid::Uuid;

pub(super) struct Interest {
    pub id: Uuid,
    pub stop: Sender<()>,
}

impl Interest {
    fn new() -> (Self, Receiver<()>) {
        let (stop, cancelled) = bounded(1);
        (
            Self {
                id: Uuid::new_v4(),
                stop,
            },
            cancelled,
        )
    }
}

impl Drop for Interest {
    fn drop(&mut self) {
        self.stop.close();
    }
}

pub(super) fn start(
    host: SharedHost,
    snapshot: HistorySnapshot,
    executor: &BackgroundExecutor,
) -> (Interest, Receiver<Result<Receiver<HistoryTitleEvent>, ()>>) {
    let (interest, cancelled) = Interest::new();
    let (ready, receiver) = bounded(1);
    executor
        .spawn(async move {
            // Even the blocking lane's thread-creation fallback runs on this
            // background executor, not in a UI continuation. The source handle
            // never crosses ready; every success/error/cancel path drops it here.
            crate::ui::host_ops::off_thread(move || {
                if cancelled.is_closed() {
                    return;
                }
                match host.watch_agent_session_titles(&snapshot) {
                    Ok(source) => {
                        if cancelled.is_closed() {
                            return;
                        }
                        if ready.try_send(Ok(source.events().clone())).is_err() {
                            return;
                        }
                        let _ = cancelled.recv_blocking();
                        drop(source);
                    }
                    Err(_) => {
                        let _ = ready.try_send(Err(()));
                    }
                }
            })
            .await;
        })
        .detach();
    (interest, receiver)
}

pub(super) fn project(snapshot: &mut HistorySnapshot, names: &[HistoryTitle]) -> Result<bool, ()> {
    if names.len() > 100 {
        return Err(());
    }
    let mut seen = std::collections::HashSet::new();
    let mut plan = Vec::with_capacity(names.len());
    for name in names {
        if name.title.trim().is_empty() || !seen.insert((name.provider, &name.id)) {
            return Err(());
        }
        let mut matches = snapshot
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, row)| row.provider == name.provider && row.id == name.id);
        let (index, _) = matches.next().ok_or(())?;
        if matches.next().is_some() {
            return Err(());
        }
        plan.push((index, &name.title));
    }
    let mut changed = false;
    for (index, title) in plan {
        if snapshot.sessions[index].title != *title {
            snapshot.sessions[index].title.clone_from(title);
            changed = true;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;
    use tty7_core::agent_sessions::live_titles::{HistoryTitle, HistoryTitleEvent};
    use tty7_core::agent_sessions::{HistorySession, Provider};

    fn sample() -> HistorySnapshot {
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

    fn names(title: &str) -> Vec<HistoryTitle> {
        vec![HistoryTitle {
            provider: Provider::Codex,
            id: sample().sessions[0].id.clone(),
            title: title.into(),
        }]
    }

    #[test]
    fn history_title_projection_is_atomic_and_metadata_only() {
        let original = sample();
        for kind in ["foreign", "duplicate", "empty", "oversized", "provider"] {
            let mut data = original.clone();
            let mut batch = names("new");
            match kind {
                "foreign" => {
                    let mut bad = batch[0].clone();
                    bad.id = "foreign".into();
                    batch.push(bad);
                }
                "duplicate" => batch.push(batch[0].clone()),
                "empty" => batch[0].title = "  ".into(),
                "oversized" => batch = vec![batch[0].clone(); 101],
                "provider" => batch[0].provider = Provider::Claude,
                _ => unreachable!(),
            }
            assert!(project(&mut data, &batch).is_err(), "{kind}");
            assert_eq!(data, original, "{kind} changed a complete snapshot");
        }
        let mut data = original.clone();
        assert!(project(&mut data, &names("official")).unwrap());
        assert!(!project(&mut data, &names("official")).unwrap());
        assert!(!project(&mut data, &[]).unwrap());
        data.sessions[0].title = original.sessions[0].title.clone();
        assert_eq!(data, original);
    }

    #[gpui::test]
    fn history_title_manager_updates_search_without_resuming(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 2);
        app.update_in(&mut vcx, |app, window, cx| {
            app.open_history_manager(
                HostId::from_connection_key("offline-title"),
                "fixture".into(),
                window,
                cx,
            );
            let host = HostRegistry::local(cx);
            let (interest, _stop) = Interest::new();
            let token = interest.id;
            let manager = app.history_manager.as_mut().unwrap();
            manager.host = HostId::LOCAL;
            manager.titles = Some(interest);
            manager.state.snapshot = Some(sample());
            manager.state.error = Some("retain action error".into());
            manager.filter = "official".into();
            manager.reconcile_selection();
            let action = Uuid::new_v4();
            let alive = manager.start_action(action);
            app.tabs[0].name = Some("manual alias".into());
            let tabs = app.tabs.iter().map(|t| t.tree_id.get()).collect::<Vec<_>>();
            assert!(app.deliver_history_titles(
                token,
                &host,
                Some(HistoryTitleEvent::Names(names("official name"))),
                cx
            ));
            let manager = app.history_manager.as_ref().unwrap();
            assert_eq!(manager.visible().len(), 1);
            assert_eq!(
                manager.selected.as_ref().unwrap().1,
                sample().sessions[0].id
            );
            assert_eq!(manager.resuming, Some(action));
            assert!(alive.load(std::sync::atomic::Ordering::Acquire));
            assert_eq!(manager.state.error.as_deref(), Some("retain action error"));
            assert!(app.deliver_history_titles(
                token,
                &host,
                Some(HistoryTitleEvent::Unavailable),
                cx
            ));
            let manager = app.history_manager.as_ref().unwrap();
            assert!(manager.title_unavailable);
            assert_eq!(
                manager.state.snapshot.as_ref().unwrap().sessions[0].title,
                "official name"
            );
            assert!(app.deliver_history_titles(
                token,
                &host,
                Some(HistoryTitleEvent::Names(names("renamed"))),
                cx
            ));
            let manager = app.history_manager.as_ref().unwrap();
            assert!(!manager.title_unavailable);
            assert!(manager.selected.is_none());
            assert_eq!(manager.resuming, Some(action));
            assert_eq!(app.tabs[0].name.as_deref(), Some("manual alias"));
            assert_eq!(
                app.tabs.iter().map(|t| t.tree_id.get()).collect::<Vec<_>>(),
                tabs
            );
        });
    }

    #[gpui::test]
    fn history_title_manager_rejects_old_interest_and_replaced_host(cx: &mut gpui::TestAppContext) {
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 0);
        app.update_in(&mut vcx, |app, window, cx| {
            let offline = HostId::from_connection_key("offline-title-stale");
            app.open_history_manager(offline, "fixture".into(), window, cx);
            let expected = HostRegistry::local(cx);
            let (interest, _stop) = Interest::new();
            let token = interest.id;
            let manager = app.history_manager.as_mut().unwrap();
            manager.host = HostId::LOCAL;
            manager.state.snapshot = Some(sample());
            manager.titles = Some(interest);
            assert!(!app.deliver_history_titles(
                Uuid::new_v4(),
                &expected,
                Some(HistoryTitleEvent::Names(names("old"))),
                cx
            ));
            HostRegistry::insert(cx, tty7_core::host::local::LocalHost::new());
            assert!(!app.deliver_history_titles(
                token,
                &expected,
                Some(HistoryTitleEvent::Names(names("wrong host"))),
                cx
            ));
            assert_eq!(
                app.history_manager.as_ref().unwrap().state.snapshot,
                Some(sample())
            );
            HostRegistry::insert(cx, expected.clone());
            app.history_manager.as_mut().unwrap().host = offline;
            app.refresh_history_manager(cx);
            assert!(!app.deliver_history_titles(
                token,
                &expected,
                Some(HistoryTitleEvent::Names(names("after refresh"))),
                cx
            ));
            app.close_history_manager(window, cx);
            app.open_history_manager(offline, "new manager".into(), window, cx);
            assert!(!app.deliver_history_titles(
                token,
                &expected,
                Some(HistoryTitleEvent::Unavailable),
                cx
            ));
            assert!(
                app.history_manager
                    .as_ref()
                    .unwrap()
                    .state
                    .snapshot
                    .is_none()
            );
            assert!(app.tabs.is_empty());
        });
    }

    #[cfg(unix)]
    struct Peer {
        host: Arc<tty7_core::host::remote::RemoteHost>,
        writer: Arc<std::sync::Mutex<std::os::unix::net::UnixStream>>,
        opened: std::sync::mpsc::Receiver<Uuid>,
        closed: std::sync::mpsc::Receiver<Uuid>,
        scanned: std::sync::mpsc::Receiver<()>,
        next_scan: Arc<std::sync::Mutex<Option<tty7_core::daemon::control::ControlReply>>>,
        release: Option<std::sync::mpsc::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    #[cfg(unix)]
    impl Peer {
        fn new() -> Self {
            Self::with_gate(false)
        }

        fn with_gate(paused: bool) -> Self {
            Self::configured(paused, false)
        }

        fn configured(paused: bool, pause_scan: bool) -> Self {
            use tty7_core::daemon::control::*;
            let (socket, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
            peer.set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let writer = Arc::new(std::sync::Mutex::new(peer.try_clone().unwrap()));
            let writes = writer.clone();
            let (opened_tx, opened) = std::sync::mpsc::channel();
            let (closed_tx, closed) = std::sync::mpsc::channel();
            let (scanned_tx, scanned) = std::sync::mpsc::channel();
            let next_scan = Arc::new(std::sync::Mutex::new(None));
            let scan_reply = next_scan.clone();
            let (release, gate) = std::sync::mpsc::channel();
            let mut gate = (paused || pause_scan).then_some(gate);
            let thread = std::thread::spawn(move || {
                assert!(matches!(
                    ControlClientMsg::read(&mut peer).unwrap(),
                    ControlClientMsg::Hello(_)
                ));
                ControlServerMsg::HelloOk(ControlHelloOk {
                    control_version: CONTROL_VERSION,
                    protocol_version: tty7_core::daemon::protocol::PROTOCOL_VERSION,
                    build: "fixture".into(),
                    separator: '/',
                    home: "/fixture".into(),
                    features: vec![],
                    instance: "title-ui-fixture".into(),
                })
                .encode(&mut *writes.lock().unwrap())
                .unwrap();
                while let Ok(ControlClientMsg::Request { req_id, req }) =
                    ControlClientMsg::read(&mut peer)
                {
                    let (reply, opened_id, closed_id) = match req {
                        ControlRequest::AgentSessions => {
                            scanned_tx.send(()).unwrap();
                            if pause_scan && let Some(gate) = gate.take() {
                                gate.recv_timeout(std::time::Duration::from_secs(10))
                                    .unwrap();
                            }
                            if let Some(reply) = scan_reply.lock().unwrap().take() {
                                ControlServerMsg::Response { req_id, reply }
                                    .encode(&mut *writes.lock().unwrap())
                                    .unwrap();
                                continue;
                            }
                            (ReplyOk::AgentSessions(sample()), None, None)
                        }
                        ControlRequest::TitleWatchOpen { id, snapshot } => {
                            assert_eq!(snapshot, sample());
                            (ReplyOk::Unit, Some(id), None)
                        }
                        ControlRequest::TitleWatchClose { id } => (ReplyOk::Unit, None, Some(id)),
                        ControlRequest::Ping => (ReplyOk::Pong, None, None),
                        other => panic!("title update performed unrelated operation: {other:?}"),
                    };
                    if let Some(id) = opened_id {
                        opened_tx.send(id).unwrap();
                        if let Some(gate) = gate.take() {
                            gate.recv_timeout(std::time::Duration::from_secs(10))
                                .unwrap();
                        }
                    }
                    ControlServerMsg::Response {
                        req_id,
                        reply: ControlReply::Ok(reply),
                    }
                    .encode(&mut *writes.lock().unwrap())
                    .unwrap();
                    if let Some(id) = closed_id {
                        closed_tx.send(id).unwrap();
                    }
                }
            });
            let host = tty7_core::host::remote::RemoteHost::over_unix(
                socket,
                "history-ui-title-fixture",
                &ControlHello::host_rpc("fixture", "fixture"),
            )
            .unwrap();
            Self {
                host,
                writer,
                opened,
                closed,
                scanned,
                next_scan,
                release: (paused || pause_scan).then_some(release),
                thread: Some(thread),
            }
        }

        fn send(&self, id: Uuid, event: HistoryTitleEvent) {
            use tty7_core::daemon::control::{ControlEvent, ControlServerMsg};
            ControlServerMsg::Event(ControlEvent::HistoryTitles { id, event })
                .encode(&mut *self.writer.lock().unwrap())
                .unwrap();
        }
    }

    #[cfg(unix)]
    impl Drop for Peer {
        fn drop(&mut self) {
            if let Some(release) = &self.release {
                let _ = release.send(());
            }
            self.host.client().close();
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }

    #[cfg(unix)]
    fn until(
        vcx: &mut gpui::VisualTestContext,
        mut check: impl FnMut(&mut gpui::VisualTestContext) -> bool,
    ) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            vcx.run_until_parked();
            if check(vcx) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "title UI did not reach expected state"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    #[gpui::test]
    fn history_title_manager_receives_remote_updates_after_discovery(
        cx: &mut gpui::TestAppContext,
    ) {
        use tty7_core::host::Host;
        cx.executor().allow_parking();
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 0);
        let peer = Peer::new();
        app.update_in(&mut vcx, |app, window, cx| {
            HostRegistry::insert(cx, peer.host.clone());
            app.open_history_manager(peer.host.id(), "Remote".into(), window, cx);
        });
        let mut id = None;
        until(&mut vcx, |_| {
            id = peer.opened.try_recv().ok().or(id);
            id.is_some()
        });
        vcx.simulate_input("official");
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("history-selected-row").is_none());
        peer.send(
            id.unwrap(),
            HistoryTitleEvent::Names(names("official name")),
        );
        until(&mut vcx, |vcx| {
            app.update_in(vcx, |app, _, _| {
                app.history_manager.as_ref().is_some_and(|m| {
                    m.visible().len() == 1
                        && m.state.snapshot.as_ref().unwrap().sessions[0].title == "official name"
                })
            })
        });
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("history-selected-row").is_some());
        app.update_in(&mut vcx, |app, window, cx| {
            assert!(app.tabs.is_empty());
            assert!(app.history_manager.as_ref().unwrap().resuming.is_none());
            app.close_history_manager(window, cx);
        });
        until(&mut vcx, |_| peer.closed.try_recv().ok() == id);
        assert!(vcx.debug_bounds("history-manager-card").is_none());
    }

    #[cfg(unix)]
    #[gpui::test]
    fn history_title_manager_close_before_ready_retires_late_subscription(
        cx: &mut gpui::TestAppContext,
    ) {
        use tty7_core::host::Host;
        cx.executor().allow_parking();
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 0);
        let peer = Peer::with_gate(true);
        app.update_in(&mut vcx, |app, window, cx| {
            HostRegistry::insert(cx, peer.host.clone());
            app.open_history_manager(peer.host.id(), "Remote".into(), window, cx);
        });
        let mut id = None;
        until(&mut vcx, |_| {
            id = peer.opened.try_recv().ok().or(id);
            id.is_some()
        });
        app.update_in(&mut vcx, |app, window, cx| {
            app.close_history_manager(window, cx)
        });
        peer.release.as_ref().unwrap().send(()).unwrap();
        until(&mut vcx, |_| peer.closed.try_recv().ok() == id);
        app.update_in(&mut vcx, |app, _, _| {
            assert!(app.history_manager.is_none());
            assert!(app.tabs.is_empty());
        });
    }

    #[cfg(unix)]
    #[gpui::test]
    fn history_title_manager_refresh_ignores_retired_stream(cx: &mut gpui::TestAppContext) {
        use tty7_core::host::Host;
        cx.executor().allow_parking();
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 0);
        let peer = Peer::new();
        app.update_in(&mut vcx, |app, window, cx| {
            HostRegistry::insert(cx, peer.host.clone());
            app.open_history_manager(peer.host.id(), "Remote".into(), window, cx);
        });
        let mut old = None;
        until(&mut vcx, |_| {
            old = peer.opened.try_recv().ok().or(old);
            old.is_some()
        });
        app.update_in(&mut vcx, |app, _, cx| app.refresh_history_manager(cx));
        let mut new = None;
        until(&mut vcx, |_| {
            new = peer.opened.try_recv().ok().or(new);
            new.is_some()
        });
        assert_ne!(old, new);
        until(&mut vcx, |_| peer.closed.try_recv().ok() == old);
        peer.send(new.unwrap(), HistoryTitleEvent::Names(names("current")));
        until(&mut vcx, |vcx| {
            app.update_in(vcx, |app, _, _| {
                app.history_manager
                    .as_ref()
                    .unwrap()
                    .state
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .sessions[0]
                    .title
                    == "current"
            })
        });
        peer.send(old.unwrap(), HistoryTitleEvent::Names(names("stale")));
        peer.host.client().ping().unwrap(); // Reader processed the old event before this reply.
        vcx.run_until_parked();
        app.update_in(&mut vcx, |app, window, cx| {
            assert_eq!(
                app.history_manager
                    .as_ref()
                    .unwrap()
                    .state
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .sessions[0]
                    .title,
                "current"
            );
            assert!(app.tabs.is_empty());
            app.close_history_manager(window, cx);
        });
        until(&mut vcx, |_| peer.closed.try_recv().ok() == new);
    }

    #[cfg(unix)]
    #[gpui::test]
    fn history_title_failed_refresh_keeps_snapshot_without_subscription(
        cx: &mut gpui::TestAppContext,
    ) {
        use tty7_core::daemon::control::{ControlReply, ReplyOk, WireError};
        use tty7_core::host::Host;
        cx.executor().allow_parking();
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 0);
        let peer = Peer::new();
        app.update_in(&mut vcx, |app, window, cx| {
            HostRegistry::insert(cx, peer.host.clone());
            app.open_history_manager(peer.host.id(), "Remote".into(), window, cx);
        });
        let mut id = None;
        until(&mut vcx, |_| {
            id = peer.opened.try_recv().ok().or(id);
            id.is_some()
        });
        peer.send(id.unwrap(), HistoryTitleEvent::Names(names("enriched")));
        let mut expected = sample();
        expected.sessions[0].title = "enriched".into();
        until(&mut vcx, |vcx| {
            app.update_in(vcx, |app, _, _| {
                app.history_manager
                    .as_ref()
                    .unwrap()
                    .state
                    .snapshot
                    .as_ref()
                    == Some(&expected)
            })
        });
        let mut missing = sample();
        missing.sessions.clear();
        missing.missing.push(Provider::Codex);
        for reply in [
            ControlReply::Err(WireError::from_io(&std::io::Error::other(
                "scan unavailable",
            ))),
            ControlReply::Ok(ReplyOk::AgentSessions(missing)),
        ] {
            *peer.next_scan.lock().unwrap() = Some(reply);
            app.update_in(&mut vcx, |app, _, cx| app.refresh_history_manager(cx));
            until(&mut vcx, |vcx| {
                app.update_in(vcx, |app, _, _| {
                    app.history_manager
                        .as_ref()
                        .unwrap()
                        .state
                        .generation
                        .is_none()
                })
            });
            app.update_in(&mut vcx, |app, _, _| {
                let manager = app.history_manager.as_ref().unwrap();
                assert_eq!(manager.state.snapshot.as_ref(), Some(&expected));
                assert!(manager.state.error.is_some());
                assert!(manager.titles.is_none());
                assert_eq!(manager.visible().len(), 1);
                assert!(app.tabs.is_empty());
            });
        }
        until(&mut vcx, |_| peer.closed.try_recv().ok() == id);
        assert!(peer.opened.try_recv().is_err());
        app.update_in(&mut vcx, |app, window, cx| {
            app.close_history_manager(window, cx)
        });
    }

    #[cfg(unix)]
    #[gpui::test]
    fn history_title_closed_window_rejects_late_discovery(cx: &mut gpui::TestAppContext) {
        use crate::ui::windows::WindowRegistry;
        use tty7_core::host::Host;
        cx.executor().allow_parking();
        cx.update(WindowRegistry::init);
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 1);
        let (other, mut other_vcx, _other_streams) =
            crate::ui::app::test_window::harness_with_tabs(cx, 1);
        other.update_in(&mut other_vcx, |app, window, cx| {
            let weak = cx.weak_entity();
            WindowRegistry::register(cx, app.workspace, window.window_handle(), weak);
        });
        let peer = Peer::configured(false, true);
        app.update_in(&mut vcx, |app, window, cx| {
            let weak = cx.weak_entity();
            WindowRegistry::register(cx, app.workspace, window.window_handle(), weak);
            HostRegistry::insert(cx, peer.host.clone());
            app.open_history_manager(peer.host.id(), "Remote".into(), window, cx);
        });
        until(&mut vcx, |_| peer.scanned.try_recv().is_ok());
        assert!(Arc::strong_count(&peer.host) > 2);
        vcx.dispatch_action(crate::core::actions::CloseWindow);
        peer.release.as_ref().unwrap().send(()).unwrap();
        // Only the fixture and HostRegistry remain after both the worker and
        // its GUI completion have released their exact Host captures.
        until(&mut vcx, |_| Arc::strong_count(&peer.host) == 2);
        app.read_with(cx, |app, _| {
            let manager = app.history_manager.as_ref().unwrap();
            assert!(manager.dismissed.get());
            assert!(manager.state.snapshot.is_none());
            assert!(manager.titles.is_none());
            assert_eq!(app.tabs.len(), 1);
        });
        assert!(peer.opened.try_recv().is_err());
        assert_eq!(cx.update(|cx| cx.windows().len()), 1);
    }

    #[cfg(unix)]
    #[gpui::test]
    fn history_title_window_close_retires_interest_with_retained_app(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::ui::windows::WindowRegistry;
        use tty7_core::host::Host;
        cx.executor().allow_parking();
        cx.update(WindowRegistry::init);
        let (app, mut vcx, _streams) = crate::ui::app::test_window::harness_with_tabs(cx, 1);
        let (other, mut other_vcx, _other_streams) =
            crate::ui::app::test_window::harness_with_tabs(cx, 1);
        other.update_in(&mut other_vcx, |app, window, cx| {
            let weak = cx.weak_entity();
            WindowRegistry::register(cx, app.workspace, window.window_handle(), weak)
        });
        let peer = Peer::new();
        app.update_in(&mut vcx, |app, window, cx| {
            let weak = cx.weak_entity();
            WindowRegistry::register(cx, app.workspace, window.window_handle(), weak);
            HostRegistry::insert(cx, peer.host.clone());
            app.open_history_manager(peer.host.id(), "Remote".into(), window, cx);
        });
        let mut id = None;
        until(&mut vcx, |_| {
            id = peer.opened.try_recv().ok().or(id);
            id.is_some()
        });
        vcx.dispatch_action(crate::core::actions::CloseWindow);
        until(&mut vcx, |_| peer.closed.try_recv().ok() == id);
        app.read_with(cx, |app, _| {
            assert_eq!(
                app.history_manager.as_ref().unwrap().state.snapshot,
                Some(sample())
            )
        });
        assert_eq!(cx.update(|cx| cx.windows().len()), 1);
    }
}
