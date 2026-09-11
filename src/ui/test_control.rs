//! Isolated scripted control peer shared by GUI lifecycle tests.
use std::sync::Arc;

type Exchange = (
    tty7_core::daemon::control::ControlRequest,
    tty7_core::daemon::control::ReplyOk,
    bool,
);

pub(super) fn control_fixture(
    exchanges: Vec<Exchange>,
    alive: Arc<std::sync::atomic::AtomicBool>,
    observe: impl Fn(&tty7_core::daemon::control::ControlRequest) + Send + 'static,
) -> (
    tty7_core::daemon::control::ControlClient,
    std::thread::JoinHandle<()>,
) {
    control_fixture_with_features(exchanges, alive, observe, vec![])
}

pub(super) fn control_fixture_with_features(
    exchanges: Vec<Exchange>,
    alive: Arc<std::sync::atomic::AtomicBool>,
    observe: impl Fn(&tty7_core::daemon::control::ControlRequest) + Send + 'static,
    features: Vec<String>,
) -> (
    tty7_core::daemon::control::ControlClient,
    std::thread::JoinHandle<()>,
) {
    use tty7_core::daemon::control::{ControlClient, ControlHello};
    let (socket, server) = control_socket_with_features(exchanges, alive, observe, features);
    let client = ControlClient::over_unix(
        socket,
        &ControlHello::gui("fixture", "fixture"),
        Box::new(|_| {}),
    )
    .unwrap();
    (client, server)
}

pub(super) fn control_socket_fixture(
    exchanges: Vec<Exchange>,
    alive: Arc<std::sync::atomic::AtomicBool>,
    observe: impl Fn(&tty7_core::daemon::control::ControlRequest) + Send + 'static,
) -> (std::os::unix::net::UnixStream, std::thread::JoinHandle<()>) {
    control_socket_with_features(exchanges, alive, observe, vec![])
}

fn control_socket_with_features(
    exchanges: Vec<Exchange>,
    alive: Arc<std::sync::atomic::AtomicBool>,
    observe: impl Fn(&tty7_core::daemon::control::ControlRequest) + Send + 'static,
    features: Vec<String>,
) -> (std::os::unix::net::UnixStream, std::thread::JoinHandle<()>) {
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tty7_core::daemon::control::{
        CONTROL_VERSION, ControlClientMsg, ControlHelloOk, ControlReply, ControlServerMsg,
    };
    let (mut peer, socket) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    peer.set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let server = std::thread::spawn(move || {
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
            features,
            instance: "isolated-prepare".into(),
        })
        .encode(&mut peer)
        .unwrap();
        for (expected, reply, cancel) in exchanges {
            let ControlClientMsg::Request { req_id, req } =
                ControlClientMsg::read(&mut peer).unwrap()
            else {
                panic!("expected a control request");
            };
            assert_eq!(req, expected);
            observe(&req);
            if cancel {
                alive.store(false, Ordering::Release);
            }
            ControlServerMsg::Response {
                req_id,
                reply: ControlReply::Ok(reply),
            }
            .encode(&mut peer)
            .unwrap();
        }
        assert_eq!(
            peer.read(&mut [0u8; 1]).unwrap(),
            0,
            "unexpected trailing request"
        );
    });
    (socket, server)
}
