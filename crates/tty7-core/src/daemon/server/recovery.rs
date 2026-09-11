//! Explicit stopped-session recovery on the machine that owns both the tree
//! and PTYs. The predecessor is the one-shot compare-and-swap identity.

use std::io;
use std::sync::Arc;

use super::Registry;
use crate::agent_sessions::{HistorySession, SessionIdentity};
use crate::core::cli_agent::ResumeInvocation;
use crate::core::machine::{PaneRecord, PaneSeed};
use crate::daemon::control::StoppedResumeRequest;
use crate::daemon::pane::DaemonPane;
use crate::host::server::Services;

/// Capture under target ownership, then let the caller write outside the locks.
/// Unlike restored_screen this does not consume the file or seed a process.
pub(super) fn stopped_output(
    registry: &Registry,
    guard: &crate::daemon::protocol::CheckedPaneAttachment,
    pane_id: u64,
) -> io::Result<Vec<crate::daemon::protocol::DaemonMsg>> {
    use crate::daemon::protocol::{DaemonMsg, PaneAttachExpectation};
    let PaneAttachExpectation::Stopped(identity) = &guard.identity else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected stopped output identity",
        ));
    };
    let services = registry.services.get().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "stopped output authority is unavailable",
        )
    })?;
    services.with_workspace_proof(&guard.proof, || {
        let machine = services.machine.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "stopped output machine is unavailable",
            )
        })?;
        machine.with_stopped_session(
            guard.proof.workspace,
            guard.tab,
            pane_id,
            identity,
            |record| {
                if let Some(pane) = registry.get(pane_id) {
                    if pane.info().alive {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "pane is still alive",
                        ));
                    }
                    pane.finalize_scrollback();
                }
                let (segments, _) = crate::daemon::scrollback::load(pane_id).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "stopped output is unavailable")
                })?;
                if segments.is_empty()
                    || segments.iter().map(|s| s.bytes.len()).sum::<usize>()
                        > crate::daemon::scrollback::SNAPSHOT_CAP
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid stopped output",
                    ));
                }
                let mut frames = Vec::with_capacity(segments.len() * 2 + 1);
                for segment in segments {
                    frames.push(DaemonMsg::Size(segment.size));
                    frames.push(DaemonMsg::Snapshot(segment.bytes));
                }
                frames.push(DaemonMsg::Exited {
                    code: record.exit.as_ref().and_then(|e| e.code),
                });
                Ok(frames)
            },
        )
    })
}

pub(super) fn sleep(
    registry: Arc<Registry>,
    request: &crate::daemon::control::SleepSessionRequest,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<()> {
    let services = registry.services.get().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "pane sleep authority is unavailable",
        )
    })?;
    sleep_transaction(services, request, cancelled, |_| {
        let pane = registry
            .get(request.pane)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "sleep pane is unavailable"))?;
        pane.sleep_session(&request.identity)
    })
}

pub(super) fn sleep_tab(
    registry: Arc<Registry>,
    request: &crate::daemon::control::SleepTabRequest,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<crate::daemon::control::SleepTabReply> {
    let services = registry.services.get().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "tab sleep authority is unavailable",
        )
    })?;
    sleep_tab_transaction(
        services,
        request,
        cancelled,
        |record, identity| {
            let pane = registry.get(record.id);
            if record.exit.is_some() {
                if pane.is_some_and(|p| p.info().alive) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "stopped member is alive",
                    ));
                }
                return Ok(());
            }
            pane.ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "sleep pane is unavailable")
            })?
            .check_sleep_session(identity)
        },
        |record, identity| {
            registry
                .get(record.id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "sleep pane disappeared"))?
                .sleep_session(identity)
        },
    )
}

fn sleep_tab_transaction(
    services: &Services,
    request: &crate::daemon::control::SleepTabRequest,
    cancelled: &dyn Fn() -> bool,
    mut validate: impl FnMut(&PaneRecord, &SessionIdentity) -> io::Result<()>,
    mut stop: impl FnMut(&PaneRecord, &SessionIdentity) -> io::Result<()>,
) -> io::Result<crate::daemon::control::SleepTabReply> {
    use crate::daemon::control::{SleepTabFailure, SleepTabReply, WireError};
    services.with_workspace_proof(&request.proof, || {
        let machine = services.machine.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Unsupported, "machine sleep is unavailable")
        })?;
        machine.with_tab_sessions(request, |records| {
            for (record, member) in records.iter().zip(&request.panes) {
                if cancelled() {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "tab sleep cancelled before signaling",
                    ));
                }
                validate(record, &member.identity)?;
            }
            let mut reply = SleepTabReply {
                accepted: Vec::new(),
                already_stopped: records
                    .iter()
                    .filter(|r| r.exit.is_some())
                    .map(|r| r.id)
                    .collect(),
                failed: None,
            };
            for (record, member) in records
                .iter()
                .zip(&request.panes)
                .filter(|(r, _)| r.exit.is_none())
            {
                let result = if cancelled() {
                    Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "tab sleep interrupted",
                    ))
                } else {
                    stop(record, &member.identity)
                };
                match result {
                    Ok(()) => reply.accepted.push(record.id),
                    Err(error) => {
                        reply.failed = Some(SleepTabFailure {
                            pane: record.id,
                            error: WireError::from_io(&error),
                        });
                        break;
                    }
                }
            }
            Ok(reply)
        })
    })
}

fn sleep_transaction(
    services: &Services,
    request: &crate::daemon::control::SleepSessionRequest,
    cancelled: &dyn Fn() -> bool,
    stop: impl FnOnce(&PaneRecord) -> io::Result<()>,
) -> io::Result<()> {
    services.with_workspace_proof(&request.proof, || {
        let machine = services.machine.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Unsupported, "machine sleep is unavailable")
        })?;
        machine.with_live_session(
            request.proof.workspace,
            request.tab,
            request.pane,
            &request.identity,
            |record| {
                if cancelled() {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "session sleep cancelled",
                    ));
                }
                stop(record)
            },
        )
    })
}

pub(super) fn resume(
    registry: Arc<Registry>,
    request: &StoppedResumeRequest,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<u64> {
    let services = registry.services.get().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "pane recovery authority is unavailable",
        )
    })?;
    let mut allocated = None;
    let result = transaction(
        services,
        request,
        cancelled,
        crate::agent_sessions::resolve_current_process,
        crate::agent_sessions::validate_current_process,
        |record, invocation| {
            let id = registry.alloc_id();
            let on_dead = super::reaper(registry.clone(), id);
            let pane = DaemonPane::spawn_resume(
                id,
                request.identity.cwd.clone().into(),
                request.size,
                invocation,
                &request.proof.workspace.to_string(),
                on_dead,
            )
            .map_err(|error| io::Error::other(error.to_string()))?;
            // Keep a strong reference outside the transaction even if the
            // reader exits immediately. Never join it under the layout lock.
            allocated = Some(pane.clone());
            registry.insert(pane);
            Ok(PaneSeed {
                pane: id,
                cwd: record.recovery_binding.as_ref().map(|b| b.cwd.clone()),
                shell: None,
                ssh_spec: None,
                agent: None,
            })
        },
    );
    if result.is_err()
        && let Some(pane) = &allocated
    {
        pane.kill();
        registry.remove(pane.id);
    }
    result
}

fn transaction(
    services: &Services,
    request: &StoppedResumeRequest,
    cancelled: &dyn Fn() -> bool,
    resolve: impl FnOnce(&SessionIdentity) -> io::Result<HistorySession>,
    validate: impl FnOnce(&HistorySession) -> io::Result<()>,
    spawn: impl FnOnce(&PaneRecord, &ResumeInvocation) -> io::Result<PaneSeed>,
) -> io::Result<u64> {
    let check_cancelled = || {
        if cancelled() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "session recovery cancelled",
            ))
        } else {
            Ok(())
        }
    };
    check_cancelled()?;
    if request.size.cols == 0
        || request.size.rows == 0
        || request.size.cols > 1000
        || request.size.rows > 1000
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid recovery terminal size",
        ));
    }
    let mut identity = request.identity.clone();
    identity.id = identity.canonical_id()?;
    services.with_workspace_proof(&request.proof, || {
        services
            .machine
            .as_ref()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "machine recovery is unavailable",
                )
            })?
            .check_stopped_session(
                request.proof.workspace,
                request.tab,
                request.pane,
                &identity,
            )
    })?;
    let session = resolve(&identity)?;
    if session.provider != identity.provider
        || session.id != identity.id
        || session.cwd != identity.cwd
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "resolved session identity changed",
        ));
    }
    services.with_workspace_proof(&request.proof, || {
        check_cancelled()?;
        let machine = services.machine.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "machine recovery is unavailable",
            )
        })?;
        machine.resume_stopped_with(
            request.proof.workspace,
            request.tab,
            request.pane,
            &identity,
            |record| {
                validate(&session)?;
                check_cancelled()?;
                let binding = record
                    .recovery_binding
                    .as_ref()
                    .expect("validated by transaction");
                let invocation =
                    binding
                        .agent
                        .resume_invocation(&identity.id)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::Unsupported,
                                "provider recovery is unavailable",
                            )
                        })?;
                spawn(record, &invocation)
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cli_agent::CLIAgent;
    use crate::core::machine::{AgentFacts, MachineStore};
    use crate::daemon::control::{ControlClient, ControlHello, ControlRequest, ReplyOk};

    fn fixture(
        stopped: bool,
    ) -> (
        tempfile::TempDir,
        Services,
        ControlClient,
        StoppedResumeRequest,
        HistorySession,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let machine = MachineStore::open(dir.path().join("machine.json"));
        let workspace = machine.workspace_create(None, None, None).unwrap();
        let cwd = dir.path().to_string_lossy().into_owned();
        let tab = machine
            .tab_create(
                workspace.id,
                None,
                PaneSeed {
                    pane: 7,
                    cwd: Some(cwd.clone()),
                    agent: None,
                    shell: None,
                    ssh_spec: None,
                },
                None,
                None,
            )
            .unwrap();
        let identity = SessionIdentity {
            provider: crate::agent_sessions::Provider::Codex,
            id: "01900000-0000-7000-8000-000000000001".into(),
            cwd: cwd.clone(),
        };
        machine.note_pane_facts(7, |p| {
            p.observe_agent_identity(
                Some(AgentFacts {
                    agent: CLIAgent::Codex,
                    session_id: Some(identity.id.clone()),
                    launch_argv: None,
                    status: None,
                }),
                Some(&cwd),
            );
            if stopped {
                p.observe_terminal_exit(Some(0));
            } else {
                p.live = true;
            }
        });
        machine.flush();
        let services = Services::with_machine(machine);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server_services = services.clone();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = crate::host::server::serve_with(
                stream,
                crate::host::local::LocalHost::new(),
                server_services,
            );
        });
        let client = ControlClient::over_tcp(
            std::net::TcpStream::connect(addr).unwrap(),
            &ControlHello::host_rpc("stopped-resume-test", "fixture"),
            Box::new(|_| {}),
        )
        .unwrap();
        let ReplyOk::WorkspaceAcquired { proof, .. } = client
            .call(ControlRequest::WorkspaceAttachIfFree {
                id: workspace.id.to_string(),
            })
            .unwrap()
        else {
            panic!("expected acquisition proof")
        };
        let request = StoppedResumeRequest {
            proof,
            tab: tab.id,
            pane: 7,
            identity: identity.clone(),
            size: crate::daemon::protocol::WinSize {
                cols: 80,
                rows: 24,
                cell_w: 8,
                cell_h: 16,
            },
        };
        let session = HistorySession {
            provider: identity.provider,
            id: identity.id,
            cwd,
            title: "fixture".into(),
            source_path: dir
                .path()
                .join("fixture.jsonl")
                .to_string_lossy()
                .into_owned(),
            modified_ms: 0,
        };
        (dir, services, client, request, session)
    }

    #[test]
    fn sleep_rejects_changed_target_before_signaling() {
        for mode in [
            "valid",
            "stop-error",
            "cancel",
            "proof",
            "slot",
            "identity",
            "cwd",
            "provider",
            "binding",
            "stopped",
            "unknown",
            "native-ssh",
        ] {
            let (_dir, services, client, resume, _) = fixture(false);
            let machine = services.machine.as_ref().unwrap();
            let mut request = crate::daemon::control::SleepSessionRequest {
                proof: resume.proof,
                tab: resume.tab,
                pane: resume.pane,
                identity: resume.identity,
            };
            match mode {
                "proof" => request.proof.nonce = uuid::Uuid::new_v4(),
                "slot" => request.tab = crate::core::machine::TabId::new(),
                "identity" => request.identity.id = uuid::Uuid::new_v4().to_string(),
                "cwd" => request.identity.cwd.push_str("/other"),
                "provider" => request.identity.provider = crate::agent_sessions::Provider::Claude,
                "binding" => machine.note_pane_facts(7, |p| p.recovery_binding = None),
                "stopped" => machine.note_pane_facts(7, |p| p.observe_terminal_exit(None)),
                "unknown" => machine.note_pane_facts(7, |p| p.live = false),
                "native-ssh" => machine.note_pane_facts(7, |p| p.ssh_spec = Some(Box::new(serde_json::from_value(serde_json::json!({"host":"fixture.invalid","port":22,"user":"fixture","auth_mode":"auto"})).unwrap()))),
                _ => {}
            }
            let before = machine.machine();
            let result = sleep_transaction(&services, &request, &|| mode == "cancel", |record| {
                assert!(
                    matches!(mode, "valid" | "stop-error"),
                    "refused request reached signaling"
                );
                assert_eq!(record.id, 7);
                if mode == "stop-error" {
                    return Err(io::Error::other("injected stop failure"));
                }
                Ok(())
            });
            assert_eq!(result.is_ok(), mode == "valid", "{mode}");
            if mode == "valid" {
                // This isolated control server has no pane services: no fallback.
                assert!(
                    client
                        .call(ControlRequest::SleepSession { request })
                        .is_err()
                );
                assert!(matches!(
                    client.call(ControlRequest::Ping).unwrap(),
                    ReplyOk::Pong
                ));
            }
            assert_eq!(
                machine.machine(),
                before,
                "stop request cannot manufacture exit or mutate layout"
            );
        }
    }

    #[test]
    fn sleep_tab_preflights_all_members_before_signaling() {
        use crate::daemon::control::{SleepTabPane, SleepTabRequest};
        use std::cell::{Cell, RefCell};
        for mode in [
            "valid",
            "stopped",
            "unknown",
            "unbound",
            "membership",
            "duplicate",
            "order",
            "proof",
            "identity",
            "cancel",
            "runtime",
            "partial",
            "cancel-after-first",
        ] {
            let (_dir, services, client, resume, _) = fixture(false);
            let machine = services.machine.as_ref().unwrap();
            let mut members = vec![SleepTabPane {
                pane: 7,
                identity: resume.identity.clone(),
            }];
            for pane in [8, 9] {
                machine
                    .pane_split(
                        resume.proof.workspace,
                        pane - 1,
                        crate::core::machine::Axis::Horizontal,
                        0.5,
                        PaneSeed {
                            pane,
                            cwd: Some(resume.identity.cwd.clone()),
                            agent: None,
                            shell: None,
                            ssh_spec: None,
                        },
                        false,
                        None,
                    )
                    .unwrap();
                let mut identity = resume.identity.clone();
                identity.id = uuid::Uuid::new_v4().to_string();
                machine.note_pane_facts(pane, |record| {
                    record.observe_agent_identity(
                        Some(AgentFacts {
                            agent: CLIAgent::Codex,
                            session_id: Some(identity.id.clone()),
                            launch_argv: None,
                            status: None,
                        }),
                        Some(&identity.cwd),
                    );
                    record.live = true;
                });
                members.push(SleepTabPane { pane, identity });
            }
            let mut request = SleepTabRequest {
                proof: resume.proof,
                tab: resume.tab,
                panes: members,
            };
            match mode {
                "stopped" => machine.note_pane_facts(8, |p| p.observe_terminal_exit(Some(0))),
                "unknown" => machine.note_pane_facts(9, |p| p.live = false),
                "unbound" => machine.note_pane_facts(9, |p| p.recovery_binding = None),
                "membership" => {
                    request.panes.pop();
                }
                "duplicate" => request.panes[2] = request.panes[1].clone(),
                "order" => request.panes.reverse(),
                "proof" => request.proof.nonce = uuid::Uuid::new_v4(),
                "identity" => request.panes[2].identity.cwd.push_str("/changed"),
                _ => {}
            }
            let before = machine.machine();
            let checked = Cell::new(0);
            let attempted = RefCell::new(Vec::new());
            let result = sleep_tab_transaction(
                &services,
                &request,
                &|| {
                    mode == "cancel"
                        || (mode == "cancel-after-first" && !attempted.borrow().is_empty())
                },
                |record, _| {
                    checked.set(checked.get() + 1);
                    if mode == "runtime" && record.id == 9 {
                        return Err(io::Error::other("changed runtime"));
                    }
                    Ok(())
                },
                |record, _| {
                    assert_eq!(
                        checked.get(),
                        3,
                        "every runtime must pass before first signal"
                    );
                    attempted.borrow_mut().push(record.id);
                    if mode == "partial" && record.id == 8 {
                        return Err(io::Error::other("runtime exited during execution"));
                    }
                    Ok(())
                },
            );
            match mode {
                "valid" | "stopped" => {
                    let reply = result.unwrap();
                    assert_eq!(
                        reply.accepted,
                        if mode == "stopped" {
                            vec![7, 9]
                        } else {
                            vec![7, 8, 9]
                        }
                    );
                    assert_eq!(
                        reply.already_stopped,
                        if mode == "stopped" { vec![8] } else { vec![] }
                    );
                    assert!(reply.failed.is_none());
                    assert_eq!(*attempted.borrow(), reply.accepted);
                }
                "partial" | "cancel-after-first" => {
                    let reply = result.unwrap();
                    assert_eq!(reply.accepted, vec![7]);
                    assert_eq!(reply.failed.unwrap().pane, 8);
                    assert_eq!(
                        *attempted.borrow(),
                        if mode == "partial" {
                            vec![7, 8]
                        } else {
                            vec![7]
                        }
                    );
                }
                _ => {
                    assert!(result.is_err(), "{mode}");
                    assert!(attempted.borrow().is_empty(), "{mode}");
                }
            }
            assert_eq!(
                machine.machine(),
                before,
                "{mode}: signaling is not an exit fact or tree edit"
            );
            if mode == "valid" {
                assert!(
                    client.call(ControlRequest::SleepTab { request }).is_err(),
                    "no pane services cannot fall back locally"
                );
                assert!(matches!(
                    client.call(ControlRequest::Ping).unwrap(),
                    ReplyOk::Pong
                ));
            }
        }
    }

    #[test]
    fn stopped_resume_transaction_rechecks_proof_cancel_source_and_slot() {
        for mode in [
            "cancel-before",
            "cancel-after",
            "source-error",
            "source-changed",
            "proof",
            "slot",
            "validate-error",
        ] {
            let (_dir, services, client, request, session) = fixture(true);
            let cancelled = std::cell::Cell::new(mode == "cancel-before");
            let result = transaction(
                &services,
                &request,
                &|| cancelled.get(),
                |_| {
                    match mode {
                        "cancel-before" => panic!("cancelled request cannot read sources"),
                        "cancel-after" => cancelled.set(true),
                        "source-error" => return Err(io::Error::other("injected source failure")),
                        "source-changed" => {
                            return Ok(HistorySession {
                                cwd: "/changed".into(),
                                ..session.clone()
                            });
                        }
                        "proof" => {
                            client
                                .call(ControlRequest::WorkspaceDetach {
                                    id: request.proof.workspace.to_string(),
                                })
                                .unwrap();
                        }
                        "slot" => {
                            services
                                .machine
                                .as_ref()
                                .unwrap()
                                .tab_close(request.proof.workspace, request.tab, None)
                                .unwrap();
                        }
                        _ => {}
                    }
                    Ok(session.clone())
                },
                |_| {
                    assert_eq!(
                        mode, "validate-error",
                        "earlier refusal must not reach final source check"
                    );
                    Err(io::Error::other("injected source replacement"))
                },
                |_, _| panic!("refused request cannot allocate"),
            );
            assert!(result.is_err(), "{mode}");
            assert!(services.machine.as_ref().unwrap().pane(42).is_none());
            if mode != "slot" {
                assert!(services.machine.as_ref().unwrap().pane(7).is_some());
            }
        }
    }

    #[test]
    fn stopped_resume_rejects_foreign_carriers_before_source_access() {
        for mode in [
            "wrong-session",
            "native-ssh",
            "unknown",
            "forged-proof",
            "size",
        ] {
            let (_dir, services, _client, mut request, _session) = fixture(true);
            let machine = services.machine.as_ref().unwrap();
            match mode {
                "wrong-session" => request.identity.id = uuid::Uuid::new_v4().to_string(),
                "native-ssh" => machine.note_pane_facts(7, |p| {
                    p.ssh_spec = Some(Box::new(serde_json::from_value(serde_json::json!({
                        "host": "fixture.invalid", "port": 22, "user": "fixture", "auth_mode": "auto"
                    })).unwrap()));
                }),
                "unknown" => {
                    machine.pane_replace(request.proof.workspace, 7, PaneSeed::bare(8), None).unwrap();
                    request.pane = 8;
                }
                "forged-proof" => request.proof.nonce = uuid::Uuid::new_v4(),
                "size" => request.size.cols = 0,
                _ => unreachable!(),
            }
            let before = machine.machine();
            assert!(
                transaction(
                    &services,
                    &request,
                    &|| false,
                    |_| panic!("{mode}: invalid carrier cannot read provider stores"),
                    |_| panic!("{mode}: invalid carrier cannot validate a source"),
                    |_, _| panic!("{mode}: invalid carrier cannot launch a process"),
                )
                .is_err()
            );
            assert_eq!(machine.machine(), before);
        }
    }

    #[test]
    fn stopped_resume_transaction_duplicate_never_reallocates() {
        let (_dir, services, client, mut request, session) = fixture(true);
        let original = request.identity.id.clone();
        request.identity.id = format!("{{{original}}}");
        let run = || {
            transaction(
                &services,
                &request,
                &|| false,
                |identity| {
                    assert_eq!(identity.id, original);
                    Ok(session.clone())
                },
                |_| Ok(()),
                |record, invocation| {
                    assert!(record.exit.is_some());
                    assert_eq!(invocation.program(), "codex");
                    assert_eq!(invocation.args(), &["resume".to_string(), original.clone()]);
                    Ok(PaneSeed {
                        pane: 42,
                        cwd: Some(session.cwd.clone()),
                        shell: None,
                        ssh_spec: None,
                        agent: None,
                    })
                },
            )
        };
        assert_eq!(run().unwrap(), 42);
        assert!(run().is_err());
        // A control-only target cannot silently route recovery to a local
        // daemon or use ordinary Spawn; the link remains usable after refusal.
        assert!(
            client
                .call(ControlRequest::ResumeStoppedSession { request })
                .is_err()
        );
        assert!(matches!(
            client.call(ControlRequest::Ping).unwrap(),
            ReplyOk::Pong
        ));
    }
}
