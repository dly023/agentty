//! Complete target layout staging; no UI state is changed here.
use std::collections::BTreeMap;
use std::sync::Arc;

use tty7_core::core::machine::{Machine, PaneNode, TabId};
use tty7_core::core::session::WorkspaceId;
use tty7_core::daemon::control::{AttachmentProof, WorkspaceUse};
use tty7_core::daemon::protocol::{
    CheckedPaneAttachment, PaneAttachExpectation, PaneAttachIdentity,
};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Plan {
    pub workspace: WorkspaceId,
    pub layout: Vec<(TabId, PaneNode)>,
    pub panes: BTreeMap<u64, (TabId, PaneAttachExpectation)>,
}

impl Plan {
    pub fn from_machine(machine: &Machine, workspace: WorkspaceId) -> Option<Self> {
        let target = machine.workspaces.iter().find(|w| w.id == workspace)?;
        if target.tabs.is_empty()
            || machine
                .workspaces
                .iter()
                .filter(|w| w.id == workspace)
                .count()
                != 1
        {
            return None;
        }
        let mut panes = BTreeMap::new();
        for tab in &target.tabs {
            if target.tabs.iter().filter(|t| t.id == tab.id).count() != 1 {
                return None;
            }
            for id in tab.root.pane_ids() {
                let mut records = machine.panes.iter().filter(|p| p.id == id);
                let pane = records.next()?;
                if !pane.live
                    || records.next().is_some()
                    || machine
                        .workspaces
                        .iter()
                        .flat_map(|w| &w.tabs)
                        .flat_map(|t| t.root.pane_ids())
                        .filter(|p| *p == id)
                        .count()
                        != 1
                {
                    return None;
                }
                let identity = match &pane.agent {
                    Some(agent) => PaneAttachExpectation::Agent(PaneAttachIdentity {
                        agent: agent.agent,
                        session_id: agent.session_id.clone().filter(|s| !s.trim().is_empty())?,
                    }),
                    None => PaneAttachExpectation::Shell,
                };
                panes.insert(id, (tab.id, identity));
            }
        }
        Some(Self {
            workspace,
            layout: target.tabs.iter().map(|t| (t.id, t.root.clone())).collect(),
            panes,
        })
    }

    pub fn policy(&self, proof: &AttachmentProof) -> Option<crate::terminal::RestorePolicy> {
        (proof.workspace == self.workspace).then(|| {
            crate::terminal::RestorePolicy::Checked(Arc::new(
                self.panes
                    .iter()
                    .map(|(id, (tab, identity))| {
                        (
                            *id,
                            CheckedPaneAttachment {
                                identity: identity.clone(),
                                proof: proof.clone(),
                                tab: *tab,
                            },
                        )
                    })
                    .collect(),
            ))
        })
    }
}

pub(super) fn stage<T>(
    plan: &Plan,
    mut attach: impl FnMut(u64) -> Result<T, String>,
    mut current: impl FnMut() -> bool,
) -> Result<BTreeMap<u64, T>, String> {
    let mut panes = BTreeMap::new();
    for id in plan.panes.keys() {
        if !current() {
            return Err(super::t(super::L10nKey::HistoryRejoinUnavailable).into());
        }
        panes.insert(*id, attach(*id)?);
    }
    if !current() {
        return Err(super::t(super::L10nKey::HistoryRejoinUnavailable).into());
    }
    Ok(panes)
}

pub(super) struct Ticket {
    pub target: super::RejoinTarget,
    pub prepared: Option<crate::ui::app::PreparedWorkspace>,
    pub usage: WorkspaceUse,
}

pub(super) fn release(client: &tty7_core::daemon::control::ControlClient, usage: WorkspaceUse) {
    if let Err(error) = client.finish_workspace_use(usage) {
        log::warn!("could not release cancelled Rejoin ownership: {error}");
    }
}

pub(super) fn prepare(
    client: &tty7_core::daemon::control::ControlClient,
    mut workspace: Option<crate::terminal::PaneWorkspace>,
    source: &tty7_core::agent_sessions::HistorySession,
    alive: &std::sync::atomic::AtomicBool,
) -> Result<Ticket, String> {
    use tty7_core::daemon::control::{ControlRequest, ReplyOk};
    let unavailable = || super::t(super::L10nKey::HistoryRejoinUnavailable).to_string();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let current = || {
        alive.load(std::sync::atomic::Ordering::Acquire)
            && std::time::Instant::now() < deadline
            && client.is_connected()
    };
    let get_machine = || match client
        .call(ControlRequest::MachineGet)
        .map_err(|e| e.to_string())?
    {
        ReplyOk::MachineTree(machine) => Ok(*machine),
        _ => Err(unavailable()),
    };
    if !current() {
        return Err(unavailable());
    }
    let machine = get_machine()?;
    let target = super::nonresident_rejoin_target(&machine, source).ok_or_else(unavailable)?;
    let plan = Plan::from_machine(&machine, target.workspace).ok_or_else(unavailable)?;
    if let Some(workspace) = workspace.as_mut() {
        workspace.workspace = target.workspace;
    }
    if !current() {
        return Err(unavailable());
    }
    let usage = client
        .acquire_workspace_use(target.workspace)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                unavailable()
            } else {
                error.to_string()
            }
        })?;
    let proof = usage.proof().clone();
    let result = (|| {
        let policy = plan.policy(&proof).ok_or_else(unavailable)?;
        let mut parts = stage(
            &plan,
            |id| {
                crate::terminal::view::TerminalView::spawn_shell_terminal_in(
                    workspace.clone(),
                    None,
                    Some(id),
                    None,
                    Some(target.workspace),
                    policy.clone(),
                )
                .map_err(|e| e.to_string())
            },
            current,
        )?;
        loop {
            if !current() {
                return Err(unavailable());
            }
            if parts
                .iter_mut()
                .all(|(id, part)| part.matches_checked_identity(&plan.panes[id].1))
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let machine = get_machine()?;
        if Plan::from_machine(&machine, target.workspace).as_ref() != Some(&plan) {
            return Err(unavailable());
        }
        match client
            .call(ControlRequest::WorkspaceAttachmentProof {
                workspace: target.workspace,
            })
            .map_err(|e| e.to_string())?
        {
            ReplyOk::AttachmentProof(current_proof) if current_proof == proof => {}
            _ => return Err(unavailable()),
        }
        if !current() {
            return Err(unavailable());
        }
        Ok(crate::ui::app::PreparedWorkspace {
            target_workspace: target.workspace,
            machine,
            policy,
            parts,
        })
    })();
    match result {
        Ok(prepared) => Ok(Ticket {
            target,
            prepared: Some(prepared),
            usage,
        }),
        Err(error) => {
            release(client, usage);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tty7_core::core::machine::{Axis, PaneRecord, Tab, Workspace};

    #[cfg(unix)]
    use crate::ui::test_control::control_fixture;

    #[test]
    #[cfg(unix)]
    fn rejoin_cleanup_preserves_other_tracked_users() {
        use std::sync::atomic::AtomicBool;
        use tty7_core::daemon::control::{ControlRequest, ReplyOk};
        let proof = AttachmentProof {
            workspace: WorkspaceId::new(),
            nonce: uuid::Uuid::new_v4(),
        };
        let mut exchanges = Vec::new();
        for newly_acquired in [true, false] {
            exchanges.push((
                ControlRequest::WorkspaceAttachIfFree {
                    id: proof.workspace.to_string(),
                },
                ReplyOk::WorkspaceAcquired {
                    proof: proof.clone(),
                    newly_acquired,
                },
                false,
            ));
        }
        // An ordered round trip proves retiring the first user sent no release.
        exchanges.push((ControlRequest::Ping, ReplyOk::Pong, false));
        // The second user alone owns the final release, despite its reused proof.
        exchanges.push((
            ControlRequest::WorkspaceReleaseProof {
                proof: proof.clone(),
            },
            ReplyOk::Unit,
            false,
        ));
        let (client, server) = control_fixture(exchanges, Arc::new(AtomicBool::new(true)), |_| {});
        let first = client.acquire_workspace_use(proof.workspace).unwrap();
        let second = client.acquire_workspace_use(proof.workspace).unwrap();
        release(&client, first.clone());
        release(&client, first);
        assert!(matches!(
            client.call(ControlRequest::Ping).unwrap(),
            ReplyOk::Pong
        ));
        release(&client, second);
        client.close();
        server.join().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn rejoin_cleanup_releases_only_fresh_exact_proof() {
        use std::sync::atomic::AtomicBool;
        use tty7_core::daemon::control::{ControlRequest, ReplyOk};
        for (newly_acquired, reply) in [
            (false, ReplyOk::Unit),
            (true, ReplyOk::Unit),
            (true, ReplyOk::Pong),
        ] {
            let proof = AttachmentProof {
                workspace: WorkspaceId::new(),
                nonce: uuid::Uuid::new_v4(),
            };
            let unexpected = matches!(reply, ReplyOk::Pong);
            let mut exchanges = vec![(
                ControlRequest::WorkspaceAttachIfFree {
                    id: proof.workspace.to_string(),
                },
                ReplyOk::WorkspaceAcquired {
                    proof: proof.clone(),
                    newly_acquired,
                },
                false,
            )];
            if newly_acquired {
                exchanges.push((
                    ControlRequest::WorkspaceReleaseProof {
                        proof: proof.clone(),
                    },
                    reply,
                    false,
                ));
            }
            let (client, server) =
                control_fixture(exchanges, Arc::new(AtomicBool::new(true)), |_| {});
            let usage = client.acquire_workspace_use(proof.workspace).unwrap();
            let duplicate = usage.clone();
            let result = client.finish_workspace_use(usage);
            if unexpected {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap(), newly_acquired);
            }
            // An unknown release result is not permission to retry.
            release(&client, duplicate);
            client.close();
            server.join().unwrap();
        }
    }

    fn fixture() -> (Machine, WorkspaceId) {
        let mut workspace = Workspace::default();
        let mut tab = Tab::leaf(1);
        tab.root = PaneNode::Split {
            axis: Axis::Horizontal,
            ratio: 0.5,
            a: Box::new(PaneNode::Leaf { pane: 1 }),
            b: Box::new(PaneNode::Leaf { pane: 2 }),
        };
        workspace.tabs.push(tab);
        let id = workspace.id;
        let mut panes = vec![PaneRecord::new(1), PaneRecord::new(2)];
        for pane in &mut panes {
            pane.live = true;
        }
        (
            Machine {
                workspaces: vec![workspace],
                panes,
                ..Default::default()
            },
            id,
        )
    }

    #[test]
    fn rejoin_plan_requires_every_neighbor_to_be_unique_and_live() {
        let (machine, workspace) = fixture();
        let plan = Plan::from_machine(&machine, workspace).unwrap();
        assert_eq!(plan.panes.len(), 2);
        for case in [
            "dead",
            "duplicate-leaf",
            "duplicate-record",
            "foreign-owner",
            "missing",
        ] {
            let mut machine = machine.clone();
            match case {
                "dead" => machine.panes[1].live = false,
                "duplicate-leaf" => {
                    machine.workspaces[0].tabs[0].root = PaneNode::Split {
                        axis: Axis::Horizontal,
                        ratio: 0.5,
                        a: Box::new(PaneNode::Leaf { pane: 1 }),
                        b: Box::new(PaneNode::Leaf { pane: 1 }),
                    }
                }
                "duplicate-record" => machine.panes.push(machine.panes[1].clone()),
                "foreign-owner" => {
                    let mut other = Workspace::default();
                    other.tabs.push(Tab::leaf(2));
                    machine.workspaces.push(other);
                }
                "missing" => {
                    machine.panes.pop();
                }
                _ => unreachable!(),
            }
            assert!(Plan::from_machine(&machine, workspace).is_none(), "{case}");
        }
        assert!(
            plan.policy(&AttachmentProof {
                workspace: WorkspaceId::new(),
                nonce: uuid::Uuid::new_v4()
            })
            .is_none()
        );
    }

    #[test]
    #[cfg(unix)]
    fn rejoin_prepare_cancellation_uses_exact_control_cleanup() {
        use std::sync::atomic::AtomicBool;
        use tty7_core::agent_sessions::{HistorySession, Provider};
        use tty7_core::core::machine::AgentFacts;
        use tty7_core::daemon::control::{ControlRequest, ReplyOk};

        for (phase, newly_acquired) in [
            ("read", false),
            ("acquire", false),
            ("acquire", true),
            ("wrong-reply", false),
        ] {
            let source = HistorySession {
                provider: Provider::Codex,
                id: "01900000-0000-7000-8000-000000000001".into(),
                title: "fixture".into(),
                cwd: "/fixture".into(),
                source_path: "/fixture/rollout.jsonl".into(),
                modified_ms: 1,
            };
            let (mut machine, workspace) = fixture();
            machine.panes[0].agent = Some(AgentFacts {
                agent: crate::core::cli_agent::CLIAgent::Codex,
                session_id: Some(source.id.clone()),
                launch_argv: None,
                status: None,
            });
            let proof = AttachmentProof {
                workspace,
                nonce: uuid::Uuid::new_v4(),
            };
            let mut exchanges = vec![(
                ControlRequest::MachineGet,
                ReplyOk::MachineTree(Box::new(machine)),
                phase == "read",
            )];
            if phase != "read" {
                exchanges.push((
                    ControlRequest::WorkspaceAttachIfFree {
                        id: workspace.to_string(),
                    },
                    if phase == "wrong-reply" {
                        ReplyOk::Pong
                    } else {
                        ReplyOk::WorkspaceAcquired {
                            proof: proof.clone(),
                            newly_acquired,
                        }
                    },
                    phase == "acquire",
                ));
                if newly_acquired {
                    exchanges.push((
                        ControlRequest::WorkspaceReleaseProof { proof },
                        ReplyOk::Unit,
                        false,
                    ));
                }
            }
            let alive = Arc::new(AtomicBool::new(true));
            let (client, server) = control_fixture(exchanges, alive.clone(), |_| {});
            // Even a regression must not route to the user's local daemon.
            let route = crate::terminal::PaneWorkspace {
                workspace,
                target: crate::core::session::RemoteTarget::LocalStdio {
                    program: "/__agentty_nonexistent_test_peer__".into(),
                    args: vec![],
                },
                spec: None,
                label: None,
                resize_echo: false,
            };
            let result = prepare(&client, Some(route), &source, &alive);
            client.close();
            server.join().unwrap();
            assert!(result.is_err(), "{phase} published a prepared workspace");
            if phase == "wrong-reply" {
                assert_eq!(
                    result.err().unwrap(),
                    super::super::t(super::super::L10nKey::HistoryRejoinUnavailable)
                );
            }
        }
    }

    #[test]
    #[cfg(unix)]
    fn rejoin_inflight_cancellation_drops_before_conditional_release() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::mpsc;
        use std::time::Duration;
        use tty7_core::daemon::control::{ControlRequest, ReplyOk};

        struct Attached(Arc<AtomicUsize>);
        impl Drop for Attached {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }

        // Cancel while the first or the final attach is still in flight.
        // Channels establish ordering; no sleep races or real sessions.
        for stop_at in [1, 2] {
            for newly_acquired in [false, true] {
                let (machine, workspace) = fixture();
                let plan = Plan::from_machine(&machine, workspace).unwrap();
                let proof = AttachmentProof {
                    workspace,
                    nonce: uuid::Uuid::new_v4(),
                };
                let alive = Arc::new(AtomicBool::new(true));
                let worker_alive = alive.clone();
                let count = Arc::new(AtomicUsize::new(0));
                let worker_count = count.clone();
                let peer_count = count.clone();
                let mut exchanges = vec![(
                    ControlRequest::WorkspaceAttachIfFree {
                        id: workspace.to_string(),
                    },
                    ReplyOk::WorkspaceAcquired {
                        proof: proof.clone(),
                        newly_acquired,
                    },
                    false,
                )];
                if newly_acquired {
                    exchanges.push((
                        ControlRequest::WorkspaceReleaseProof { proof },
                        ReplyOk::Unit,
                        false,
                    ));
                }
                let (client, server) = control_fixture(exchanges, alive.clone(), move |request| {
                    if matches!(request, ControlRequest::WorkspaceReleaseProof { .. }) {
                        assert_eq!(
                            peer_count.load(Ordering::SeqCst),
                            0,
                            "lease released before panes dropped"
                        );
                    }
                });
                let usage = client.acquire_workspace_use(workspace).unwrap();
                let (entered_tx, entered_rx) = mpsc::channel();
                let (continue_tx, continue_rx) = mpsc::channel();
                let worker = std::thread::spawn(move || {
                    let mut attached_ids = Vec::new();
                    let result = stage(
                        &plan,
                        |id| {
                            attached_ids.push(id);
                            worker_count.fetch_add(1, Ordering::SeqCst);
                            let attached = Attached(worker_count.clone());
                            if id == stop_at {
                                entered_tx.send(()).unwrap();
                                continue_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                            }
                            Ok(attached)
                        },
                        || worker_alive.load(Ordering::Acquire),
                    );
                    assert!(result.is_err(), "late attach result was accepted");
                    assert_eq!(attached_ids, (1..=stop_at).collect::<Vec<_>>());
                    assert_eq!(
                        worker_count.load(Ordering::SeqCst),
                        0,
                        "partial results survived cancellation"
                    );
                    release(&client, usage);
                    client.close();
                    server.join().unwrap();
                });
                entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                assert_eq!(count.load(Ordering::SeqCst), stop_at as usize);
                alive.store(false, Ordering::Release);
                continue_tx.send(()).unwrap();
                worker.join().unwrap();
                assert_eq!(count.load(Ordering::SeqCst), 0);
            }
        }
    }

    #[test]
    fn rejoin_staging_drops_partial_results_before_failure() {
        struct Attached(Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for Attached {
            fn drop(&mut self) {
                self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let (machine, workspace) = fixture();
        let plan = Plan::from_machine(&machine, workspace).unwrap();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let result = stage(
            &plan,
            |id| {
                if id == 2 {
                    return Err("second pane refused".into());
                }
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(Attached(count.clone()))
            },
            || true,
        );
        assert!(result.is_err());
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(stage::<()>(&plan, |_| panic!("cancelled operation attached"), || false).is_err());
    }
}
