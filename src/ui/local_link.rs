use std::sync::Arc;

use agentty_core::daemon::control::ControlClient;
use gpui::{App, Global};

use crate::ui::remote_workspace::Backoff;

#[derive(Default)]
pub struct LocalLink {
    client: Option<Arc<ControlClient>>,
    /// Build stamp reported by the daemon at handshake; compared against this
    /// binary's own stamp to detect a stale (pre-rebuild) daemon.
    server_build: Option<String>,
    backoff: Backoff,
    next_attempt: Option<std::time::Instant>,
    attempting: bool,
    pumping: bool,
}

impl Global for LocalLink {}

impl LocalLink {
    pub fn install(cx: &mut App) {
        crate::ui::remote_workspace::install_event_observer();
        let link = cx.default_global::<LocalLink>();
        if link.pumping {
            return;
        }
        link.pumping = true;
        cx.spawn(async move |cx| {
            loop {
                cx.update(|cx| {
                    Self::tick(cx);
                    crate::ui::remote_workspace::drain_events(cx);
                });
                cx.background_executor()
                    .timer(crate::ui::remote_workspace::PUMP_TICK)
                    .await;
            }
        })
        .detach();
    }

    /// The connected daemon's build stamp when it differs from this binary's
    /// own — i.e. the daemon predates the last rebuild and a server restart
    /// would sync them. None when matching or not connected.
    pub fn stale_server_build(cx: &mut App) -> Option<(String, String)> {
        let daemon = cx.default_global::<LocalLink>().server_build.clone()?;
        let ours = agentty_core::daemon::protocol::build_stamp();
        agentty_core::daemon::protocol::is_stale_build(&daemon, &ours).then_some((daemon, ours))
    }

    pub fn client(cx: &mut App) -> Option<Arc<ControlClient>> {
        let link = cx.default_global::<LocalLink>();
        link.client.as_ref().filter(|c| c.is_connected()).cloned()
    }

    /// Drops the cached client without waiting for its reader to notice.
    ///
    /// `ControlClient::is_connected` only flips once the reader sees EOF, so
    /// for a moment after we kill the daemon ourselves the dead link still
    /// hands itself out and every call on it fails. Callers that know the far
    /// end is gone say so here, and the next tick reconnects.
    pub fn invalidate(cx: &mut App) {
        let link = cx.default_global::<LocalLink>();
        link.server_build = None;
        if link.client.take().is_some() {
            log::info!("dropped the control link to the local daemon; it was restarted");
        }
        link.backoff.reset();
        link.next_attempt = None;
    }

    fn tick(cx: &mut App) {
        let now = std::time::Instant::now();
        let link = cx.default_global::<LocalLink>();
        if link.attempting {
            return;
        }
        if let Some(client) = &link.client {
            if client.is_connected() {
                return;
            }
            log::info!("lost the control link to the local daemon; reconnecting");
            link.client = None;
            link.server_build = None;
        }
        if passive_connect_action(crate::daemon::spawn::is_reachable()).is_none() {
            link.backoff.reset();
            link.next_attempt = None;
            return;
        }
        match link.next_attempt {
            None if link.backoff.attempt() == 0 => {}
            None => {
                link.next_attempt = Some(now + link.backoff.delay());
                return;
            }
            Some(at) if at > now => return,
            Some(_) => {}
        }
        link.next_attempt = None;
        link.attempting = true;
        let _ = link.backoff.advance();

        cx.spawn(async move |cx| {
            let connected = cx
                .background_executor()
                .spawn(async move { connect_existing_blocking() })
                .await;
            cx.update(|cx| {
                let link = cx.default_global::<LocalLink>();
                link.attempting = false;
                match connected {
                    Ok(client) => {
                        log::info!("control link to the local daemon is up");
                        link.server_build = Some(client.hello().build.clone());
                        link.client = Some(client);
                        link.backoff.reset();
                        link.next_attempt = None;
                        crate::ui::machine_mirror::MachineMirrors::refresh(
                            cx,
                            agentty_core::host::HostId::LOCAL,
                        );
                        crate::ui::tree_sync::on_link_up(cx, agentty_core::host::HostId::LOCAL);
                    }
                    Err(e) => {
                        log::debug!("local control link attempt failed: {e}");
                    }
                }
            });
        })
        .detach();
    }
}

fn passive_connect_action(runtime_reachable: bool) -> Option<&'static str> {
    runtime_reachable.then_some("connect")
}

fn connect_existing_blocking() -> std::io::Result<Arc<ControlClient>> {
    use agentty_core::daemon::control::ControlHello;

    let hello = ControlHello::host_rpc(uuid::Uuid::new_v4().to_string(), "this computer");
    let sink: agentty_core::daemon::control::EventSink = Box::new(local_event_sink);
    #[cfg(unix)]
    let client = ControlClient::over_unix(
        std::os::unix::net::UnixStream::connect(agentty_core::host::server::control_socket_path()?)?,
        &hello,
        sink,
    )?;
    #[cfg(windows)]
    let client =
        ControlClient::over_tcp(agentty_core::host::server::connect_control()?, &hello, sink)?;
    Ok(Arc::new(client))
}

fn local_event_sink(event: agentty_core::daemon::control::ControlEvent) {
    agentty_core::daemon::control::observe_event(agentty_core::host::HostId::LOCAL, event);
}

#[cfg(test)]
mod tests {
    #[test]
    fn passive_local_link_never_starts_the_local_runtime() {
        assert_eq!(super::passive_connect_action(false), None);
        assert_eq!(super::passive_connect_action(true), Some("connect"));
    }
}
