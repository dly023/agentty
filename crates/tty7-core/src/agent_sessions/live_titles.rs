//! Target-owned name invalidation. Never scans history membership or runs an agent.
use super::{ScanLimits, StoreRoots, current_roots, read_codex_titles};
use crate::core::{
    cli_agent::CLIAgent,
    machine::{MachineStore, ProviderTitle},
};
use notify::Watcher;
use std::{
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub struct TitleWatch {
    stop: Arc<AtomicBool>,
    wake: mpsc::SyncSender<()>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Drop for TitleWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn start_current(store: Arc<MachineStore>) -> io::Result<TitleWatch> {
    start(store, current_roots()?)
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryTitle {
    pub provider: super::Provider,
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryTitleEvent {
    Names(Vec<HistoryTitle>),
    Unavailable,
}

/// Target-owned lifetime. Disposal joins its reader and must run off the GUI
/// thread, like creation and target source validation.
pub struct HistoryTitleSubscription {
    events: smol::channel::Receiver<HistoryTitleEvent>,
    _guard: Box<dyn Send + Sync>,
}

impl HistoryTitleSubscription {
    pub(crate) fn new(
        events: smol::channel::Receiver<HistoryTitleEvent>,
        guard: Box<dyn Send + Sync>,
    ) -> Self {
        Self {
            events,
            _guard: guard,
        }
    }

    pub fn events(&self) -> &smol::channel::Receiver<HistoryTitleEvent> {
        &self.events
    }
}

impl Drop for HistoryTitleSubscription {
    fn drop(&mut self) {
        // Close clones held by forwarders before retiring the backend guard.
        self.events.close();
    }
}

pub fn watch_history_current(
    snapshot: &super::HistorySnapshot,
) -> io::Result<HistoryTitleSubscription> {
    watch_history(current_roots()?, snapshot)
}

fn validate_history(roots: &StoreRoots, snapshot: &super::HistorySnapshot) -> io::Result<()> {
    if snapshot.sessions.len() > 100 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many title interests",
        ));
    }
    let started = Instant::now();
    let mut seen = std::collections::HashSet::new();
    for row in &snapshot.sessions {
        super::check_deadline(started, ScanLimits::default())?;
        if !seen.insert((row.provider, &row.id)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "duplicate title interest",
            ));
        }
        super::validate(roots, row)?;
    }
    super::check_deadline(started, ScanLimits::default())
}

fn watch_history(
    roots: StoreRoots,
    snapshot: &super::HistorySnapshot,
) -> io::Result<HistoryTitleSubscription> {
    validate_history(&roots, snapshot)?;
    let snapshot = snapshot.clone();
    let (wake, heard) = mpsc::sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = IndexSource::new(roots, wake.clone(), epoch.clone())?;
    let (send, events) = smol::channel::bounded(1);
    let worker = thread::Builder::new()
        .name("tty7-history-titles".into())
        .spawn(move || {
            // Snapshot labels are client display data, not provider facts.
            let mut names = std::collections::HashMap::<String, String>::new();
            let mut last_sent = None;
            let mut validated_epoch = None;
            while !stopped.load(Ordering::Acquire) && !send.is_closed() {
                source.observe();
                let event = match source.read() {
                    Ok(Some((reading, titles))) => {
                        let mut candidate = names.clone();
                        for row in snapshot
                            .sessions
                            .iter()
                            .filter(|row| row.provider == super::Provider::Codex)
                        {
                            if let Some(title) = titles.get(&row.id) {
                                candidate.insert(row.id.clone(), title.clone());
                            }
                        }
                        if validated_epoch != Some(reading) {
                            if validate_history(&source.roots, &snapshot).is_ok() {
                                validated_epoch = Some(reading);
                            }
                        }
                        if stopped.load(Ordering::Acquire)
                            || epoch.load(Ordering::Acquire) != reading
                        {
                            continue;
                        }
                        if validated_epoch == Some(reading) {
                            names = candidate;
                            Some(HistoryTitleEvent::Names(
                                snapshot
                                    .sessions
                                    .iter()
                                    .filter_map(|row| {
                                        if row.provider != super::Provider::Codex {
                                            return None;
                                        }
                                        names.get(&row.id).map(|title| HistoryTitle {
                                            provider: row.provider,
                                            id: row.id.clone(),
                                            title: title.clone(),
                                        })
                                    })
                                    .collect(),
                            ))
                        } else {
                            Some(HistoryTitleEvent::Unavailable)
                        }
                    }
                    Ok(None) => None,
                    Err(_) => Some(HistoryTitleEvent::Unavailable),
                };
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                if let Some(event) = event.filter(|event| last_sent.as_ref() != Some(event)) {
                    match send.try_send(event.clone()) {
                        Ok(()) => last_sent = Some(event),
                        Err(smol::channel::TrySendError::Full(_)) => {}
                        Err(smol::channel::TrySendError::Closed(_)) => break,
                    }
                }
                let _ = heard.recv_timeout(Duration::from_millis(250));
            }
        })?;
    Ok(HistoryTitleSubscription::new(
        events,
        Box::new(TitleWatch {
            stop,
            wake,
            worker: Some(worker),
        }),
    ))
}

fn publish_title(
    store: &Arc<MachineStore>,
    pane: u64,
    wanted: ProviderTitle,
    epoch: &AtomicU64,
    reading: u64,
    stopped: &AtomicBool,
) {
    store.note_pane_facts(pane, |current| {
        if stopped.load(Ordering::Acquire) || epoch.load(Ordering::Acquire) != reading {
            return;
        }
        let bound = current
            .provider_title_binding()
            .is_some_and(|b| b.agent == wanted.agent && b.session_id == wanted.session_id);
        if bound {
            current.provider_title = Some(wanted);
        }
    });
}

/// One target-owned source engine shared by title projections. It knows
/// nothing about pane membership, processes, UI rows or user aliases.
struct IndexSource {
    roots: StoreRoots,
    index: PathBuf,
    watcher: notify::RecommendedWatcher,
    epoch: Arc<AtomicU64>,
    repair: Arc<AtomicBool>,
    watched: Vec<PathBuf>,
    stamp: Option<(u64, Option<std::time::SystemTime>)>,
    parsed_epoch: Option<u64>,
    titles: std::collections::HashMap<String, String>,
}

impl IndexSource {
    fn new(
        roots: StoreRoots,
        wake: mpsc::SyncSender<()>,
        epoch: Arc<AtomicU64>,
    ) -> io::Result<Self> {
        let index = roots.codex.join("session_index.jsonl");
        let changed = index.clone();
        let source_epoch = epoch.clone();
        let repair = Arc::new(AtomicBool::new(true));
        let source_repair = repair.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let relevant = match event {
                Ok(event) => {
                    if !event.kind.is_access()
                        && event
                            .paths
                            .iter()
                            .any(|p| p != &changed && changed.starts_with(p))
                    {
                        source_repair.store(true, Ordering::Release);
                    }
                    !event.kind.is_access()
                        && event
                            .paths
                            .iter()
                            .any(|p| p == &changed || changed.starts_with(p))
                }
                Err(_) => {
                    source_repair.store(true, Ordering::Release);
                    true
                }
            };
            if relevant {
                source_epoch.fetch_add(1, Ordering::AcqRel);
                let _ = wake.try_send(());
            }
        })
        .map_err(io::Error::other)?;
        Ok(Self {
            roots,
            index,
            watcher,
            epoch,
            repair,
            watched: Vec::new(),
            stamp: None,
            parsed_epoch: None,
            titles: std::collections::HashMap::new(),
        })
    }

    fn observe(&mut self) {
        // Parent watching handles atomic replacement; absent roots use the
        // nearest existing ancestor until the provider directory appears.
        let nearest = self
            .roots
            .codex
            .ancestors()
            .find(|p| p.is_dir())
            .map(PathBuf::from);
        let desired: Vec<_> = nearest
            .iter()
            .cloned()
            .chain(nearest.as_ref().and_then(|p| p.parent()).map(PathBuf::from))
            .collect();
        if desired != self.watched || self.repair.swap(false, Ordering::AcqRel) {
            for path in &self.watched {
                let _ = self.watcher.unwatch(path);
            }
            self.watched.clear();
            for path in desired {
                if self
                    .watcher
                    .watch(&path, notify::RecursiveMode::NonRecursive)
                    .is_ok()
                {
                    self.watched.push(path);
                }
            }
            self.epoch.fetch_add(1, Ordering::AcqRel);
        }
        // Cheap repair for dropped events, never a history-directory scan.
        let current = std::fs::symlink_metadata(&self.index)
            .ok()
            .map(|m| (m.len(), m.modified().ok()));
        if current != self.stamp {
            self.stamp = current;
            self.epoch.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn read(&mut self) -> io::Result<Option<(u64, &std::collections::HashMap<String, String>)>> {
        let reading = self.epoch.load(Ordering::Acquire);
        if self.parsed_epoch != Some(reading) {
            let result = read_codex_titles(&self.roots, Instant::now(), ScanLimits::default())?;
            if self.epoch.load(Ordering::Acquire) != reading {
                return Ok(None);
            }
            self.titles = result;
            self.parsed_epoch = Some(reading);
        }
        Ok(Some((reading, &self.titles)))
    }
}

fn start(store: Arc<MachineStore>, roots: StoreRoots) -> io::Result<TitleWatch> {
    let (tx, rx) = mpsc::sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = IndexSource::new(roots, tx.clone(), epoch.clone())?;
    let machine_wake = tx.clone();
    let subscription = store.subscribe(Arc::new(move |_, _| {
        let _ = machine_wake.try_send(());
    }));
    let stopped = stop.clone();
    let worker = thread::Builder::new()
        .name("tty7-codex-titles".into())
        .spawn(move || {
            let _subscription = subscription;
            loop {
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                source.observe();
                let bindings: Vec<_> = store
                    .machine()
                    .panes
                    .into_iter()
                    .filter_map(|pane| {
                        let binding = pane.provider_title_binding()?;
                        (binding.agent == CLIAgent::Codex
                            && uuid::Uuid::parse_str(&binding.session_id).is_ok())
                        .then(|| (pane.id, binding.clone()))
                    })
                    .collect();
                if bindings.is_empty() {
                    let _ = rx.recv_timeout(Duration::from_secs(1));
                    continue;
                }
                let (reading, titles) = match source.read() {
                    Ok(Some(result)) => result,
                    _ => {
                        let _ = rx.recv_timeout(Duration::from_millis(250));
                        continue;
                    }
                };
                for (pane, binding) in bindings {
                    let Ok(uuid) = uuid::Uuid::parse_str(&binding.session_id) else {
                        continue;
                    };
                    let Some(title) = titles.get(&uuid.to_string()) else {
                        continue;
                    };
                    let wanted = ProviderTitle {
                        agent: CLIAgent::Codex,
                        session_id: binding.session_id,
                        title: title.clone(),
                    };
                    publish_title(&store, pane, wanted, &epoch, reading, &stopped);
                }
                let _ = rx.recv_timeout(Duration::from_secs(1));
            }
        })?;
    Ok(TitleWatch {
        stop,
        wake: tx,
        worker: Some(worker),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::machine::{AgentFacts, PaneSeed};
    use std::fs;

    const ID: &str = "12345678-1234-1234-1234-123456789abc";

    #[test]
    fn history_title_subscription_closes_receivers_before_backend_disposal() {
        struct Guard {
            sender: smol::channel::Sender<HistoryTitleEvent>,
            drops: Arc<AtomicU64>,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                assert!(self.sender.is_closed());
                self.drops.fetch_add(1, Ordering::SeqCst);
            }
        }
        let (sender, receiver) = smol::channel::bounded(1);
        let drops = Arc::new(AtomicU64::new(0));
        let sub = HistoryTitleSubscription::new(
            receiver,
            Box::new(Guard {
                sender: sender.clone(),
                drops: drops.clone(),
            }),
        );
        let observer = sub.events().clone();
        sender.try_send(HistoryTitleEvent::Unavailable).unwrap();
        assert_eq!(observer.try_recv().unwrap(), HistoryTitleEvent::Unavailable);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(sub);
        assert!(observer.is_closed());
        assert!(
            sender
                .try_send(HistoryTitleEvent::Names(Vec::new()))
                .is_err()
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    fn history_fixture() -> (tempfile::TempDir, StoreRoots, super::super::HistorySnapshot) {
        let dir = tempfile::tempdir().unwrap();
        let roots = StoreRoots {
            codex: dir.path().join("codex"),
            claude: dir.path().join("claude"),
        };
        let sessions = roots.codex.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("rollout.jsonl"), format!("{}\n", serde_json::json!({
            "type":"session_meta", "payload":{"id":ID,"cwd":dir.path(),"source":"cli","title":"initial"}
        }))).unwrap();
        let snapshot = super::super::scan(&roots, ScanLimits::default()).unwrap();
        assert_eq!(snapshot.sessions.len(), 1);
        (dir, roots, snapshot)
    }

    #[test]
    fn history_title_subscription_without_panes_is_bounded_and_read_only() {
        let (dir, roots, mut snapshot) = history_fixture();
        snapshot.sessions[0].title = "untrusted client title".into();
        let before = snapshot.clone();
        let index = roots.codex.join("session_index.jsonl");
        let row_bytes = fs::read(&snapshot.sessions[0].source_path).unwrap();
        let subscription = watch_history(roots, &snapshot).unwrap();
        let events = subscription.events().clone();
        let wait = |wanted: HistoryTitleEvent| {
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                if events.try_recv().ok().as_ref() == Some(&wanted) {
                    break;
                }
                assert!(
                    Instant::now() < until,
                    "history title event missing: {wanted:?}"
                );
                thread::sleep(Duration::from_millis(20));
            }
        };
        let names = |name: &str| {
            HistoryTitleEvent::Names(vec![HistoryTitle {
                provider: super::super::Provider::Codex,
                id: ID.into(),
                title: name.into(),
            }])
        };
        wait(HistoryTitleEvent::Names(Vec::new()));
        fs::write(&index, format!("{{\"id\":\"{ID}\",\"thread_name\":\"late\"}}\n{{\"id\":\"00000000-0000-0000-0000-000000000001\",\"thread_name\":\"not a row\"}}\n")).unwrap();
        wait(names("late"));
        fs::write(&index, "{\"id\":").unwrap();
        wait(HistoryTitleEvent::Unavailable);
        fs::remove_file(&index).unwrap();
        wait(names("late"));
        for name in ["second", "third", "latest"] {
            let replacement = dir.path().join("replacement");
            fs::write(
                &replacement,
                format!("{{\"id\":\"{ID}\",\"thread_name\":\"{name}\"}}\n"),
            )
            .unwrap();
            fs::rename(replacement, &index).unwrap();
        }
        wait(names("latest"));
        assert!(events.capacity().is_some_and(|n| n == 1));
        assert_eq!(
            fs::read(&snapshot.sessions[0].source_path).unwrap(),
            row_bytes
        );
        fs::write(&snapshot.sessions[0].source_path, "{}\n").unwrap();
        fs::write(
            &index,
            format!("{{\"id\":\"{ID}\",\"thread_name\":\"wrong source\"}}\n"),
        )
        .unwrap();
        wait(HistoryTitleEvent::Unavailable);
        drop(subscription);
        assert!(events.is_closed());
        assert_eq!(snapshot, before);
        assert!(!dir.path().join("machine.json").exists());
        assert!(!row_bytes.is_empty());
    }

    #[test]
    fn history_title_subscription_rejects_invalid_membership() {
        let (_dir, roots, snapshot) = history_fixture();
        let copy_roots = || StoreRoots {
            codex: roots.codex.clone(),
            claude: roots.claude.clone(),
        };
        let mut bad = snapshot.clone();
        bad.sessions.push(bad.sessions[0].clone());
        assert!(watch_history(copy_roots(), &bad).is_err());
        bad.sessions = vec![snapshot.sessions[0].clone(); 101];
        assert!(watch_history(copy_roots(), &bad).is_err());
        bad = snapshot.clone();
        bad.sessions[0].id = "00000000-0000-0000-0000-000000000001".into();
        assert!(watch_history(copy_roots(), &bad).is_err());
        bad = snapshot.clone();
        bad.sessions[0].source_path = roots.codex.join("outside.jsonl").to_string_lossy().into();
        fs::copy(
            &snapshot.sessions[0].source_path,
            &bad.sessions[0].source_path,
        )
        .unwrap();
        assert!(watch_history(copy_roots(), &bad).is_err());
    }

    #[test]
    fn title_source_without_panes_retains_complete_names() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join("codex");
        let (wake, heard) = mpsc::sync_channel(1);
        let epoch = Arc::new(AtomicU64::new(0));
        let mut source = IndexSource::new(
            StoreRoots {
                codex: codex.clone(),
                claude: dir.path().join("claude"),
            },
            wake,
            epoch.clone(),
        )
        .unwrap();
        source.observe();
        assert!(source.read().unwrap().unwrap().1.is_empty());
        fs::create_dir(&codex).unwrap();
        let index = codex.join("session_index.jsonl");
        let record = |name: &str| format!("{{\"id\":\"{ID}\",\"thread_name\":\"{name}\"}}\n");
        let wait = |source: &mut IndexSource, wanted: &str| {
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                source.observe();
                if source
                    .read()
                    .ok()
                    .flatten()
                    .is_some_and(|(_, names)| names.get(ID).is_some_and(|n| n == wanted))
                {
                    break;
                }
                assert!(
                    Instant::now() < until,
                    "no-pane source did not publish {wanted}"
                );
                let _ = heard.recv_timeout(Duration::from_millis(50));
            }
        };
        fs::write(&index, record("late official name")).unwrap();
        wait(&mut source, "late official name");
        fs::write(&index, format!("{}{{\"id\":", record("incomplete"))).unwrap();
        source.observe();
        epoch.fetch_add(1, Ordering::AcqRel);
        assert!(source.read().is_err());
        assert_eq!(source.titles.get(ID).unwrap(), "late official name");
        let replacement = codex.join("replacement");
        fs::write(&replacement, record("replaced name")).unwrap();
        fs::rename(replacement, &index).unwrap();
        wait(&mut source, "replaced name");
        fs::rename(&codex, dir.path().join("old-codex")).unwrap();
        fs::create_dir(&codex).unwrap();
        fs::write(&index, record("new directory")).unwrap();
        wait(&mut source, "new directory");
        // Only the disposable provider source was touched: no MachineStore,
        // pane, agent process, binding or history-membership scan is needed.
        assert!(!dir.path().join("machine.json").exists());
    }

    fn wait_title(store: &Arc<MachineStore>, heard: &mpsc::Receiver<()>, wanted: &str) {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if store
                .pane(1)
                .and_then(|p| p.provider_title)
                .is_some_and(|t| t.title == wanted)
            {
                return;
            }
            assert!(Instant::now() < until, "title did not become {wanted}");
            let _ = heard.recv_timeout(Duration::from_millis(100));
        }
    }

    #[test]
    fn live_title_index_create_replace_partial_and_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let store = MachineStore::open(dir.path().join("machine.json"));
        let ws = store.workspace_create(None, None, None).unwrap();
        store
            .tab_create(ws.id, None, PaneSeed::bare(1), None, None)
            .unwrap();
        store.note_pane_facts(1, |p| {
            p.observe_agent_identity(
                Some(AgentFacts {
                    agent: CLIAgent::Codex,
                    session_id: Some(ID.into()),
                    launch_argv: None,
                    status: None,
                }),
                Some("/work"),
            )
        });
        let (tx, rx) = mpsc::channel();
        let _sub = store.subscribe(Arc::new(move |_, _| {
            let _ = tx.send(());
        }));
        let codex = dir.path().join("codex");
        let watch = start(
            store.clone(),
            StoreRoots {
                codex: codex.clone(),
                claude: dir.path().join("claude"),
            },
        )
        .unwrap();
        fs::create_dir(&codex).unwrap();
        let index = codex.join("session_index.jsonl");
        let record = |title: &str| format!("{{\"id\":\"{ID}\",\"thread_name\":\"{title}\"}}\n");
        fs::write(&index, record("first")).unwrap();
        wait_title(&store, &rx, "first");
        fs::write(&index, format!("{}{{\"id\":", record("not committed"))).unwrap();
        // Explicit wake+parser error proves no partial result; allow a full
        // repair interval so merely winning a scheduling race cannot pass.
        assert!(
            read_codex_titles(
                &StoreRoots {
                    codex: codex.clone(),
                    claude: dir.path().join("claude")
                },
                Instant::now(),
                ScanLimits::default()
            )
            .is_err()
        );
        let until = Instant::now() + Duration::from_millis(350);
        while Instant::now() < until {
            let _ = rx.recv_timeout(Duration::from_millis(50));
        }
        assert_eq!(
            store.pane(1).unwrap().provider_title.unwrap().title,
            "first"
        );
        let replacement = codex.join("new-index");
        fs::write(&replacement, record("renamed")).unwrap();
        fs::rename(replacement, &index).unwrap();
        wait_title(&store, &rx, "renamed");
        fs::rename(&codex, dir.path().join("old-codex")).unwrap();
        fs::create_dir(&codex).unwrap();
        fs::write(&index, record("new directory")).unwrap();
        wait_title(&store, &rx, "new directory");
        store.note_pane_facts(1, |p| {
            p.observe_agent_identity(
                Some(AgentFacts {
                    agent: CLIAgent::Claude,
                    session_id: Some(ID.into()),
                    launch_argv: None,
                    status: None,
                }),
                Some("/work"),
            )
        });
        fs::write(&index, record("wrong provider")).unwrap();
        drop(watch); // Worker joined; no later title publication is possible.
        assert!(store.pane(1).unwrap().provider_title.is_none());
        fs::write(&index, record("after stop")).unwrap();
        assert!(store.pane(1).unwrap().provider_title.is_none());
        assert_eq!(store.workspace(ws.id).unwrap().tabs.len(), 1);
        assert!(store.workspace(ws.id).unwrap().tabs[0].name.is_none());
    }

    #[test]
    fn live_title_updates_after_exit_and_terminal_stop() {
        let dir = tempfile::tempdir().unwrap();
        let store = MachineStore::open(dir.path().join("machine.json"));
        let ws = store.workspace_create(None, None, None).unwrap();
        store
            .tab_create(ws.id, None, PaneSeed::bare(1), None, None)
            .unwrap();
        store.note_pane_facts(1, |p| {
            p.observe_agent_identity(
                Some(AgentFacts {
                    agent: CLIAgent::Codex,
                    session_id: Some(ID.into()),
                    launch_argv: None,
                    status: None,
                }),
                Some("/work"),
            );
            p.observe_agent_identity(None, Some("/elsewhere"));
        });
        let binding = store.pane(1).unwrap().recovery_binding;
        let (tx, rx) = mpsc::channel();
        let _sub = store.subscribe(Arc::new(move |_, _| {
            let _ = tx.send(());
        }));
        let codex = dir.path().join("codex");
        fs::create_dir(&codex).unwrap();
        let index = codex.join("session_index.jsonl");
        let watch = start(
            store.clone(),
            StoreRoots {
                codex,
                claude: dir.path().join("claude"),
            },
        )
        .unwrap();
        fs::write(
            &index,
            format!("{{\"id\":\"{ID}\",\"thread_name\":\"after exit\"}}\n"),
        )
        .unwrap();
        wait_title(&store, &rx, "after exit");
        store.note_pane_facts(1, |p| p.observe_terminal_exit(Some(0)));
        fs::write(
            &index,
            format!("{{\"id\":\"{ID}\",\"thread_name\":\"after terminal stop\"}}\n"),
        )
        .unwrap();
        wait_title(&store, &rx, "after terminal stop");
        drop(watch);
        let pane = store.pane(1).unwrap();
        assert!(!pane.live);
        assert!(pane.agent.is_none());
        assert_eq!(pane.recovery_binding, binding);
        assert_eq!(pane.recovery_binding.unwrap().cwd, "/work");
        assert_eq!(store.machine().panes.len(), 1);
        assert_eq!(store.workspace(ws.id).unwrap().tabs.len(), 1);
        assert!(store.workspace(ws.id).unwrap().tabs[0].name.is_none());
    }

    #[test]
    fn live_title_commit_rejects_replaced_binding_after_exit() {
        let dir = tempfile::tempdir().unwrap();
        let store = MachineStore::open(dir.path().join("machine.json"));
        let ws = store.workspace_create(None, None, None).unwrap();
        store
            .tab_create(ws.id, None, PaneSeed::bare(1), None, None)
            .unwrap();
        let facts = AgentFacts {
            agent: CLIAgent::Codex,
            session_id: Some(ID.into()),
            launch_argv: None,
            status: None,
        };
        store.note_pane_facts(1, |p| {
            p.observe_agent_identity(Some(facts.clone()), Some("/work"));
            p.observe_agent_identity(None, None);
        });
        let epoch = AtomicU64::new(1);
        let stopped = AtomicBool::new(false);
        let title = ProviderTitle {
            agent: CLIAgent::Codex,
            session_id: ID.into(),
            title: "late".into(),
        };
        publish_title(&store, 1, title.clone(), &epoch, 1, &stopped);
        assert_eq!(store.pane(1).unwrap().provider_title, Some(title.clone()));
        store.note_pane_facts(1, |p| {
            let mut replacement = facts.clone();
            replacement.session_id = Some("aaaaaaaa-1234-1234-1234-123456789abc".into());
            p.observe_agent_identity(Some(replacement), Some("/work"));
            p.observe_agent_identity(None, None);
        });
        publish_title(&store, 1, title.clone(), &epoch, 1, &stopped);
        assert!(store.pane(1).unwrap().provider_title.is_none());
        store.note_pane_facts(1, |p| {
            p.observe_agent_identity(Some(facts.clone()), Some("/work"));
            let mut unknown = facts;
            unknown.session_id = None;
            p.observe_agent_identity(Some(unknown), Some("/work"));
            p.observe_agent_identity(None, None);
        });
        publish_title(&store, 1, title, &epoch, 1, &stopped);
        assert!(store.pane(1).unwrap().provider_title.is_none());
        assert!(store.pane(1).unwrap().recovery_binding.is_none());
    }

    #[test]
    fn live_title_commit_rechecks_epoch_binding_and_stop() {
        let dir = tempfile::tempdir().unwrap();
        let store = MachineStore::open(dir.path().join("machine.json"));
        let ws = store.workspace_create(None, None, None).unwrap();
        let facts = AgentFacts {
            agent: CLIAgent::Codex,
            session_id: Some(ID.into()),
            launch_argv: None,
            status: None,
        };
        let mut seed = PaneSeed::bare(1);
        seed.agent = Some(facts.clone());
        store.tab_create(ws.id, None, seed, None, None).unwrap();
        let epoch = AtomicU64::new(1);
        let stopped = AtomicBool::new(false);
        let title = ProviderTitle {
            agent: CLIAgent::Codex,
            session_id: ID.into(),
            title: "native".into(),
        };
        publish_title(&store, 1, title.clone(), &epoch, 1, &stopped);
        assert!(
            store.pane(1).unwrap().provider_title.is_none(),
            "client seed is not target identity"
        );
        store.note_pane_facts(1, |p| p.observe_agent_identity(Some(facts), Some("/work")));
        publish_title(&store, 1, title.clone(), &epoch, 0, &stopped);
        assert!(
            store.pane(1).unwrap().provider_title.is_none(),
            "stale read cannot commit"
        );
        stopped.store(true, Ordering::Release);
        publish_title(&store, 1, title.clone(), &epoch, 1, &stopped);
        assert!(store.pane(1).unwrap().provider_title.is_none());
        stopped.store(false, Ordering::Release);
        store.note_pane_facts(1, |p| {
            p.ssh_spec = Some(Box::new(
                serde_json::from_str(
                    r#"{"host":"remote","port":22,"user":"me","auth_mode":"auto"}"#,
                )
                .unwrap(),
            ))
        });
        publish_title(&store, 1, title.clone(), &epoch, 1, &stopped);
        assert!(
            store.pane(1).unwrap().provider_title.is_none(),
            "native SSH cannot read local titles"
        );
        store.note_pane_facts(1, |p| p.ssh_spec = None);
        publish_title(&store, 1, title.clone(), &epoch, 1, &stopped);
        assert_eq!(store.pane(1).unwrap().provider_title, Some(title));
    }
}
