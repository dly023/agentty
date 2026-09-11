//! Real control RPC and PTY boundary, using an executable fixture, not a vendor CLI.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tty7_core::agent_sessions::{Provider, SessionIdentity};
use tty7_core::client::PaneClient;
use tty7_core::core::cli_agent::CLIAgent;
use tty7_core::core::machine::{
    AgentRecoveryBinding, LayoutDelta, MACHINE_FILE, Machine, MachineStore, PaneExit, PaneNode,
    PaneSeed,
};
use tty7_core::daemon::control::{
    ControlClient, ControlEvent, ControlHello, ControlRequest, ReplyOk, StoppedResumeRequest,
};
use tty7_core::daemon::protocol::{
    CheckedPaneAttachment, ClientMsg, DaemonMsg, PaneAttachExpectation, WinSize,
};

const ID: &str = "01900000-0000-7000-8000-000000000001";
const SECOND_ID: &str = "01900000-0000-7000-8000-000000000002";
const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    child: DaemonProcess,
    control: Arc<ControlClient>,
    events: Arc<Mutex<Vec<ControlEvent>>>,
    request: StoppedResumeRequest,
    dir: tempfile::TempDir,
}

// Own the child before handshake assertions so a setup failure cannot leak it.
struct DaemonProcess(Child);

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn until(mut done: impl FnMut() -> bool, description: &str) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out: {description}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

impl Fixture {
    fn start(immediate_exit: bool) -> Self {
        Self::start_with_split(immediate_exit, false)
    }

    fn start_with_split(immediate_exit: bool, split: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("project with spaces");
        fs::create_dir(&cwd).unwrap();
        let cwd = cwd.to_str().unwrap().to_owned();
        let source = dir.path().join("codex/sessions");
        fs::create_dir_all(&source).unwrap();
        // TempDir supplies a generated path; no arbitrary JSON input is interpolated.
        assert!(!cwd.contains(['"', '\\', '\n']));
        fs::write(
            source.join(format!("rollout-{ID}.jsonl")),
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ID}\",\"cwd\":\"{cwd}\",\"source\":\"cli\"}}}}\n"
            ),
        )
        .unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let executable = bin.join("codex");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$RESUME_TEST_ROOT/argv\"\npwd > \"$RESUME_TEST_ROOT/cwd\"\nprintf '%s\\n' \"$$\" >> \"$RESUME_TEST_ROOT/launches\"\nprintf 'RESUME_FIXTURE_READY\\n'\n{}\n",
                if immediate_exit { "exit 23" } else { "exec /bin/sleep 60" }
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();

        let store = MachineStore::open(dir.path().join(MACHINE_FILE));
        let workspace = store
            .workspace_create(None, Some("retained".into()), None)
            .unwrap();
        let tab = store
            .tab_create(workspace.id, None, PaneSeed::bare(41), None, None)
            .unwrap();
        store.note_pane_facts(41, |pane| {
            pane.exit = Some(PaneExit { code: Some(0) });
            pane.recovery_binding = Some(AgentRecoveryBinding {
                agent: CLIAgent::Codex,
                session_id: ID.into(),
                cwd: cwd.clone(),
                launch_argv: None,
            });
        });
        if split {
            fs::write(source.join(format!("rollout-{SECOND_ID}.jsonl")), format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{SECOND_ID}\",\"cwd\":\"{cwd}\",\"source\":\"cli\"}}}}\n"
            )).unwrap();
            store
                .pane_split(
                    workspace.id,
                    41,
                    tty7_core::core::machine::Axis::Horizontal,
                    0.4,
                    PaneSeed::bare(42),
                    false,
                    None,
                )
                .unwrap();
            store.note_pane_facts(42, |pane| {
                pane.exit = Some(PaneExit { code: Some(0) });
                pane.recovery_binding = Some(AgentRecoveryBinding {
                    agent: CLIAgent::Codex,
                    session_id: SECOND_ID.into(),
                    cwd: cwd.clone(),
                    launch_argv: None,
                });
            });
        }
        store.flush();
        drop(store);

        // Clear inherited provider roots, credentials and hooks. Nothing here
        // launches the user's CLI or accesses their configuration.
        let mut child = DaemonProcess(
            Command::new(env!("CARGO_BIN_EXE_tty7-server"))
                .args(["--daemon", "--config-dir"])
                .arg(dir.path())
                .env_clear()
                .env("HOME", dir.path())
                .env("SHELL", "/bin/sh")
                .env("TERM", "xterm-256color")
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("CODEX_HOME", dir.path().join("codex"))
                .env("CLAUDE_CONFIG_DIR", dir.path().join("claude"))
                .env("TTY7_DATA_DIR", dir.path())
                .env("TTY7_CONTROL_SOCK", dir.path().join("control.sock"))
                .env("RESUME_TEST_ROOT", dir.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + WAIT;
        let stream = loop {
            if let Ok(stream) = UnixStream::connect(dir.path().join("control.sock")) {
                break stream;
            }
            if child.0.try_wait().unwrap().is_some() || Instant::now() >= deadline {
                panic!("isolated daemon did not open its control endpoint");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let control = Arc::new(
            ControlClient::over_unix(
                stream,
                &ControlHello::gui("resume-test", "fixture"),
                Box::new(move |event| sink.lock().unwrap().push(event)),
            )
            .unwrap(),
        );
        let ReplyOk::WorkspaceAcquired { proof, .. } = control
            .call(ControlRequest::WorkspaceAttachIfFree {
                id: workspace.id.to_string(),
            })
            .unwrap()
        else {
            panic!("no workspace proof")
        };
        Self {
            child,
            control,
            events,
            request: StoppedResumeRequest {
                proof,
                tab: tab.id,
                pane: 41,
                identity: SessionIdentity {
                    provider: Provider::Codex,
                    id: ID.into(),
                    cwd,
                },
                size: WinSize {
                    cols: 80,
                    rows: 24,
                    cell_w: 8,
                    cell_h: 16,
                },
            },
            dir,
        }
    }

    fn panes(&self) -> PaneClient {
        PaneClient::at(self.dir.path().join("daemon.sock"))
    }

    fn child_pids(&self) -> Vec<u32> {
        let output = Command::new("/bin/ps")
            .args(["-axo", "pid=,ppid="])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid = fields.next()?.parse::<u32>().ok()?;
                let parent = fields.next()?.parse::<u32>().ok()?;
                (parent == self.child.0.id()).then_some(pid)
            })
            .collect()
    }

    fn machine(&self) -> Machine {
        match self.control.call(ControlRequest::MachineGet).unwrap() {
            ReplyOk::MachineTree(machine) => *machine,
            other => panic!("unexpected tree reply: {other:?}"),
        }
    }

    fn resume(&self) -> std::io::Result<ReplyOk> {
        self.control.call(ControlRequest::ResumeStoppedSession {
            request: self.request.clone(),
        })
    }

    fn resumed_id(&self) -> u64 {
        match self.resume().unwrap() {
            ReplyOk::StoppedSessionResumed {
                workspace,
                tab,
                predecessor,
                pane,
            } => {
                assert_eq!(workspace, self.request.proof.workspace);
                assert_eq!(tab, self.request.tab);
                assert_eq!(predecessor, 41);
                assert_ne!(pane, predecessor);
                pane
            }
            other => panic!("unexpected resume reply: {other:?}"),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(panes) = self.panes().list() {
            for pane in panes {
                let _ = self.panes().kill(pane.pane_id);
            }
        }
        // Only this still-owned daemon's direct children are eligible for
        // cleanup if a broken registry omitted a test process.
        for pid in self.child_pids() {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

#[test]
fn whole_tab_sleep_preserves_split_and_each_resume_identity() {
    use tty7_core::daemon::control::{SleepTabPane, SleepTabRequest};
    let fixture = Fixture::start_with_split(false, true);
    let mut resume_second = fixture.request.clone();
    resume_second.pane = 42;
    resume_second.identity.id = SECOND_ID.into();
    let first = fixture.resumed_id();
    let second = match fixture
        .control
        .call(ControlRequest::ResumeStoppedSession {
            request: resume_second.clone(),
        })
        .unwrap()
    {
        ReplyOk::StoppedSessionResumed { pane, .. } => pane,
        reply => panic!("unexpected second resume: {reply:?}"),
    };
    for pane in [first, second] {
        let mut observer = fixture.panes().observe(pane, fixture.request.size).unwrap();
        observer.set_recv_timeout(Some(WAIT)).unwrap();
        let mut output = Vec::new();
        while !String::from_utf8_lossy(&output).contains("RESUME_FIXTURE_READY") {
            match observer.recv().unwrap() {
                DaemonMsg::Snapshot(bytes) | DaemonMsg::Output(bytes) => output.extend(bytes),
                _ => {}
            }
        }
        observer.detach().unwrap();
    }
    let before = fixture.machine();
    let root = before.workspaces[0].tabs[0].root.clone();
    assert_eq!(root.pane_ids(), vec![first, second]);
    let request = SleepTabRequest {
        proof: fixture.request.proof.clone(),
        tab: fixture.request.tab,
        panes: vec![
            SleepTabPane {
                pane: first,
                identity: fixture.request.identity.clone(),
            },
            SleepTabPane {
                pane: second,
                identity: resume_second.identity.clone(),
            },
        ],
    };
    let mut foreign = request.clone();
    foreign.panes[1].identity.id = "01900000-0000-7000-8000-000000000003".into();
    assert!(
        fixture
            .control
            .call(ControlRequest::SleepTab { request: foreign })
            .is_err()
    );
    let live = fixture.panes().list().unwrap();
    assert!(
        [first, second]
            .iter()
            .all(|id| live.iter().any(|p| p.pane_id == *id && p.alive))
    );
    let ReplyOk::TabSleep(reply) = fixture
        .control
        .call(ControlRequest::SleepTab {
            request: request.clone(),
        })
        .unwrap()
    else {
        panic!("expected whole-tab result");
    };
    assert_eq!(reply.accepted, vec![first, second]);
    assert!(reply.already_stopped.is_empty());
    assert!(reply.failed.is_none());
    until(
        || {
            let machine = fixture.machine();
            [first, second].iter().all(|id| {
                machine
                    .panes
                    .iter()
                    .any(|p| p.id == *id && p.exit.is_some() && !p.live)
            })
        },
        "both explicit exits observed",
    );
    until(
        || fixture.panes().list().unwrap().is_empty(),
        "both exited carriers retired",
    );
    let stopped = fixture.machine();
    assert_eq!(stopped.workspaces[0].tabs[0].root, root);
    assert_eq!(stopped.workspaces[0].tabs[0].id, fixture.request.tab);
    for id in [first, second] {
        let old = before.panes.iter().find(|p| p.id == id).unwrap();
        let new = stopped.panes.iter().find(|p| p.id == id).unwrap();
        assert_eq!(new.recovery_binding, old.recovery_binding);
        let raw = fs::read(
            fixture
                .dir
                .path()
                .join("scrollback")
                .join(format!("{id}.bin")),
        )
        .unwrap();
        let (segments, _) = tty7_core::daemon::scrollback::decode(&raw).unwrap();
        let output: Vec<u8> = segments.into_iter().flat_map(|s| s.bytes).collect();
        assert!(String::from_utf8_lossy(&output).contains("RESUME_FIXTURE_READY"));
    }
    let ReplyOk::TabSleep(repeated) = fixture
        .control
        .call(ControlRequest::SleepTab { request })
        .unwrap()
    else {
        panic!("expected already-stopped result");
    };
    assert!(repeated.accepted.is_empty());
    assert_eq!(repeated.already_stopped, vec![first, second]);
    assert!(repeated.failed.is_none());
    let mut resumed = Vec::new();
    for (pane, mut resume) in [(first, fixture.request.clone()), (second, resume_second)] {
        resume.pane = pane;
        let ReplyOk::StoppedSessionResumed {
            pane: successor,
            tab,
            predecessor,
            ..
        } = fixture
            .control
            .call(ControlRequest::ResumeStoppedSession { request: resume })
            .unwrap()
        else {
            panic!("expected explicit resume");
        };
        assert_eq!(tab, fixture.request.tab);
        assert_eq!(predecessor, pane);
        assert_ne!(successor, pane);
        resumed.push(successor);
    }
    assert_eq!(
        fixture.machine().workspaces[0].tabs[0].root.pane_ids(),
        resumed
    );
}

#[test]
fn explicit_sleep_retains_original_slot_and_resumes_again() {
    let mut fixture = Fixture::start(false);
    let id = fixture.resumed_id();
    until(
        || fixture.dir.path().join("launches").exists(),
        "first provider started",
    );
    let mut observer = fixture.panes().observe(id, fixture.request.size).unwrap();
    observer.set_recv_timeout(Some(WAIT)).unwrap();
    let mut observed = Vec::new();
    while !String::from_utf8_lossy(&observed).contains("RESUME_FIXTURE_READY") {
        match observer.recv().unwrap() {
            DaemonMsg::Snapshot(bytes) | DaemonMsg::Output(bytes) => observed.extend(bytes),
            _ => {}
        }
    }
    observer.detach().unwrap();
    let before = fixture.machine();
    let original = before.panes.iter().find(|p| p.id == id).unwrap();
    let sleep = tty7_core::daemon::control::SleepSessionRequest {
        proof: fixture.request.proof.clone(),
        tab: fixture.request.tab,
        pane: id,
        identity: fixture.request.identity.clone(),
    };
    let mut foreign = sleep.clone();
    foreign.identity.id = "01900000-0000-7000-8000-000000000002".into();
    assert!(
        fixture
            .control
            .call(ControlRequest::SleepSession { request: foreign })
            .is_err()
    );
    assert!(
        fixture
            .machine()
            .panes
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .live
    );
    assert!(matches!(
        fixture
            .control
            .call(ControlRequest::SleepSession {
                request: sleep.clone()
            })
            .unwrap(),
        ReplyOk::Unit
    ));
    until(
        || {
            fixture
                .machine()
                .panes
                .iter()
                .any(|p| p.id == id && p.exit.is_some())
        },
        "target confirms explicit stop",
    );
    let stopped = fixture.machine();
    // The daemon has not reached its 30-second periodic snapshot yet.
    until(
        || {
            fixture
                .panes()
                .list()
                .unwrap()
                .iter()
                .all(|p| p.pane_id != id)
        },
        "exited carrier retired",
    );
    let snapshot_path = fixture
        .dir
        .path()
        .join("scrollback")
        .join(format!("{id}.bin"));
    let raw = fs::read(&snapshot_path).expect("final output must precede carrier retirement");
    let (segments, _) = tty7_core::daemon::scrollback::decode(&raw).unwrap();
    let output: Vec<u8> = segments
        .into_iter()
        .flat_map(|segment| segment.bytes)
        .collect();
    assert!(String::from_utf8_lossy(&output).contains("RESUME_FIXTURE_READY"));
    for case in [
        "read",
        "repeat",
        "proof",
        "session",
        "cwd",
        "tab",
        "missing",
        "corrupt",
        "truncated",
        "empty",
        "oversized",
        "restored",
    ] {
        let valid = matches!(case, "read" | "repeat" | "restored");
        let saved_path = snapshot_path.with_extension("saved");
        match case {
            "missing" => fs::rename(&snapshot_path, &saved_path).unwrap(),
            "corrupt" => fs::write(&snapshot_path, b"invalid snapshot header").unwrap(),
            "truncated" => fs::write(&snapshot_path, &raw[..raw.len() / 2]).unwrap(),
            "empty" => fs::write(
                &snapshot_path,
                tty7_core::daemon::scrollback::encode(&[], None),
            )
            .unwrap(),
            "oversized" => fs::write(
                &snapshot_path,
                tty7_core::daemon::scrollback::encode(
                    &[tty7_core::daemon::scrollback::Segment {
                        size: fixture.request.size,
                        bytes: vec![b'x'; tty7_core::daemon::scrollback::SNAPSHOT_CAP + 1],
                    }],
                    None,
                ),
            )
            .unwrap(),
            _ => {}
        }
        let expected_file = match fs::read(&snapshot_path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("cannot inspect {case} fixture: {error}"),
        };
        let mut guard = CheckedPaneAttachment {
            proof: sleep.proof.clone(),
            tab: sleep.tab,
            identity: PaneAttachExpectation::Stopped(sleep.identity.clone()),
        };
        match case {
            "proof" => guard.proof.nonce = Default::default(),
            "tab" => guard.tab = tty7_core::core::machine::TabId::new(),
            "session" | "cwd" => {
                let PaneAttachExpectation::Stopped(identity) = &mut guard.identity else {
                    unreachable!()
                };
                if case == "session" {
                    identity.id = "01900000-0000-7000-8000-000000000002".into();
                } else {
                    identity.cwd = "/wrong-output-owner".into();
                }
            }
            _ => {}
        }
        let mut stream = UnixStream::connect(fixture.dir.path().join("daemon.sock")).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        ClientMsg::AttachChecked {
            pane_id: id,
            size: fixture.request.size,
            guard,
            allow_remote_clipboard_write: false,
        }
        .encode(&mut stream)
        .unwrap();
        let mut preview = Vec::new();
        loop {
            match DaemonMsg::read(&mut stream).unwrap() {
                DaemonMsg::Size(_) => assert!(valid, "{case} leaked geometry"),
                DaemonMsg::Snapshot(bytes) => {
                    assert!(valid, "{case} leaked output");
                    preview.extend(bytes);
                }
                DaemonMsg::Exited { .. } => {
                    assert!(valid, "{case} reported successful output");
                    assert_eq!(preview, output);
                    break;
                }
                DaemonMsg::Error(_) => {
                    assert!(!valid, "valid stopped output refused: {case}");
                    assert!(preview.is_empty(), "foreign request leaked output");
                    break;
                }
                other => panic!("unexpected stopped-output side effect: {other:?}"),
            }
        }
        match expected_file {
            Some(bytes) => assert_eq!(
                fs::read(&snapshot_path).unwrap(),
                bytes,
                "{case} consumed or repaired output"
            ),
            None => assert_eq!(
                fs::read(&snapshot_path).unwrap_err().kind(),
                std::io::ErrorKind::NotFound,
                "{case} recreated output"
            ),
        }
        assert_eq!(fixture.machine(), stopped, "preview mutated target facts");
        // Restore only this test's temporary snapshot for the next case and Resume.
        if case == "missing" {
            fs::rename(&saved_path, &snapshot_path).unwrap();
        } else if matches!(case, "corrupt" | "truncated" | "empty" | "oversized") {
            fs::write(&snapshot_path, &raw).unwrap();
        }
    }
    assert_eq!(
        fs::metadata(&snapshot_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(stopped.workspaces, before.workspaces);
    let record = stopped.panes.iter().find(|p| p.id == id).unwrap();
    assert!(!record.live);
    assert!(record.agent.is_none());
    assert_eq!(record.recovery_binding, original.recovery_binding);
    assert!(
        fixture
            .control
            .call(ControlRequest::SleepSession { request: sleep })
            .is_err()
    );
    fixture.request.pane = id;
    let ReplyOk::StoppedSessionResumed {
        pane: successor,
        predecessor,
        ..
    } = fixture.resume().unwrap()
    else {
        panic!("expected explicit resume");
    };
    assert_eq!(predecessor, id);
    assert_ne!(successor, id);
    until(
        || {
            fs::read_to_string(fixture.dir.path().join("launches"))
                .unwrap()
                .lines()
                .count()
                == 2
        },
        "exactly two explicit launches",
    );
    let restored = fixture.machine();
    assert_eq!(restored.workspaces[0].tabs[0].id, fixture.request.tab);
    assert_eq!(
        restored.workspaces[0].tabs[0].root.pane_ids(),
        vec![successor]
    );
    fixture.panes().kill(id).unwrap();
    until(
        || !snapshot_path.exists(),
        "explicit deletion forgets the old output",
    );
}

#[test]
fn stopped_resume_rpc_runs_exact_argv_once() {
    let fixture = Fixture::start(false);
    let before = fixture.machine();
    let id = fixture.resumed_id();
    // The real target checks both workspace ownership and an empty controller
    // slot even though the newly resumed provider has not reported an ID.
    let attach = || {
        let mut stream = UnixStream::connect(fixture.dir.path().join("daemon.sock")).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        ClientMsg::AttachChecked {
            pane_id: id,
            size: fixture.request.size,
            guard: CheckedPaneAttachment {
                identity: PaneAttachExpectation::Owned,
                proof: fixture.request.proof.clone(),
                tab: fixture.request.tab,
            },
            allow_remote_clipboard_write: false,
        }
        .encode(&mut stream)
        .unwrap();
        stream
    };
    let mut controller = attach();
    assert!(matches!(
        DaemonMsg::read(&mut controller).unwrap(),
        DaemonMsg::Size(_)
    ));
    let mut competing = attach();
    assert!(
        matches!(DaemonMsg::read(&mut competing).unwrap(), DaemonMsg::Error(reason) if reason.contains("controller"))
    );
    until(
        || fixture.dir.path().join("launches").exists(),
        "fixture started",
    );
    assert_eq!(
        fs::read_to_string(fixture.dir.path().join("argv")).unwrap(),
        format!("resume\n{ID}\n")
    );
    let actual_cwd = fs::read_to_string(fixture.dir.path().join("cwd")).unwrap();
    assert_eq!(
        fs::canonicalize(actual_cwd.trim()).unwrap(),
        fs::canonicalize(&fixture.request.identity.cwd).unwrap()
    );
    let after = fixture.machine();
    assert_eq!(
        after.workspaces[0].tabs[0].id,
        before.workspaces[0].tabs[0].id
    );
    assert_eq!(
        after.workspaces[0].tabs[0].root,
        PaneNode::Leaf { pane: id }
    );
    assert!(after.panes.iter().all(|p| p.id != 41));
    assert!(
        fixture.resume().is_err(),
        "consumed predecessor launched again"
    );
    assert_eq!(
        fs::read_to_string(fixture.dir.path().join("launches"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    until(
        || {
            fixture.events.lock().unwrap().iter().any(|event| {
                matches!(event,
                    ControlEvent::Layout { delta: LayoutDelta::TabRestructured { tab, .. }, .. }
                    if tab.id == fixture.request.tab && tab.root == PaneNode::Leaf { pane: id }
                )
            })
        },
        "replacement delta",
    );
    // Reading output does not acquire the pane controller. The GUI's checked
    // attachment is a separate acceptance requirement.
    let mut session = fixture.panes().observe(id, fixture.request.size).unwrap();
    session.set_recv_timeout(Some(WAIT)).unwrap();
    let mut output = Vec::new();
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "fixture output did not arrive");
        match session.recv().unwrap() {
            DaemonMsg::Snapshot(bytes) | DaemonMsg::Output(bytes) => output.extend(bytes),
            DaemonMsg::Exited { code } => panic!("fixture exited early: {code:?}"),
            _ => {}
        }
        if String::from_utf8_lossy(&output).contains("RESUME_FIXTURE_READY") {
            break;
        }
    }
}

#[test]
fn stopped_resume_rpc_immediate_exit_remains_recoverable() {
    let fixture = Fixture::start(true);
    let id = fixture.resumed_id();
    until(
        || {
            fixture
                .machine()
                .panes
                .iter()
                .any(|p| p.id == id && p.exit.is_some())
        },
        "confirmed immediate exit",
    );
    let machine = fixture.machine();
    let pane = machine.panes.iter().find(|p| p.id == id).unwrap();
    assert_eq!(pane.exit, Some(PaneExit { code: Some(23) }));
    assert_eq!(pane.recovery_binding.as_ref().unwrap().session_id, ID);
    assert!(!pane.resume_pending);
    assert!(!pane.live);
    assert_eq!(
        machine.workspaces[0].tabs[0].root,
        PaneNode::Leaf { pane: id }
    );
    assert!(fixture.resume().is_err());
    until(
        || fixture.panes().list().unwrap().is_empty(),
        "exited process reaped",
    );
    until(
        || fixture.child_pids().is_empty(),
        "immediate child reaped by OS",
    );
}

#[test]
fn stopped_resume_rpc_disk_failure_does_not_leak_a_pane() {
    let fixture = Fixture::start(false);
    let before = fixture.machine();
    let file = fixture.dir.path().join(MACHINE_FILE);
    let saved = fs::read(&file).unwrap();
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    assert!(
        fixture.resume().is_err(),
        "persistence obstruction must fail"
    );
    assert_eq!(fixture.machine(), before);
    until(
        || fixture.panes().list().unwrap().is_empty(),
        "failed allocation removed",
    );
    until(
        || fixture.child_pids().is_empty(),
        "failed allocation OS process reaped",
    );
    assert!(
        !fixture.events.lock().unwrap().iter().any(|event| matches!(
            event,
            ControlEvent::Layout {
                delta: LayoutDelta::TabRestructured { .. },
                ..
            }
        )),
        "failed transaction published a replacement"
    );
    fs::remove_dir(&file).unwrap();
    fs::write(&file, saved).unwrap();
    assert!(matches!(
        fixture.control.call(ControlRequest::Ping).unwrap(),
        ReplyOk::Pong
    ));
}
