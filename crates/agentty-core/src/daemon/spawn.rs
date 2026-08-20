use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::core::config;
use crate::daemon::protocol::{ClientMsg, DaemonMsg, DaemonVersion, PROTOCOL_VERSION};
use crate::daemon::{pidfile, transport};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(6);
pub const SERVER_EXE_ENV: &str = "AGENTTY_SERVER_EXE";
#[cfg(any(target_os = "macos", target_os = "linux"))]
const REAP_TERM_TIMEOUT: Duration = Duration::from_secs(6);
#[cfg(any(target_os = "macos", target_os = "linux"))]
const REAP_KILL_TIMEOUT: Duration = Duration::from_secs(2);

pub struct MismatchedDaemon {
    pub version: Option<DaemonVersion>,
}

static MISMATCHED_DAEMON: std::sync::Mutex<Option<MismatchedDaemon>> = std::sync::Mutex::new(None);

pub fn take_mismatched_daemon() -> Option<MismatchedDaemon> {
    MISMATCHED_DAEMON.lock().ok()?.take()
}

static LOCAL_DAEMON: std::sync::Mutex<Option<DaemonVersion>> = std::sync::Mutex::new(None);
static STARTUP_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_startup_gate<T>(operation: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    let path = config::config_path("daemon.start.lock")
        .ok_or_else(|| anyhow::anyhow!("no config directory for the local runtime startup gate"))?;
    with_startup_gate_at(&path, operation)
}

fn with_startup_gate_at<T>(
    path: &Path,
    operation: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let _guard = STARTUP_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _file_lock = StartupFileLock::acquire(path)?;
    operation()
}

struct StartupFileLock {
    _file: std::fs::File,
}

impl StartupFileLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        acquire_startup_file_lock(path).map(|file| Self { _file: file })
    }
}

#[cfg(unix)]
fn acquire_startup_file_lock(path: &Path) -> io::Result<std::fs::File> {
    use std::os::fd::AsRawFd as _;

    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)?;
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(file);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(windows)]
fn acquire_startup_file_lock(path: &Path) -> io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    loop {
        match std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .share_mode(0)
            .open(path)
        {
            Ok(file) => return Ok(file),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
                ) =>
            {
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn acquire_startup_file_lock(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
}

pub fn local_daemon_supports(feature: &str) -> bool {
    LOCAL_DAEMON
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|v| v.has_feature(feature)))
        .unwrap_or(false)
}

fn note_local_daemon(version: Option<DaemonVersion>) {
    if let Ok(mut slot) = LOCAL_DAEMON.lock() {
        *slot = version;
    }
}

#[derive(Debug, PartialEq, Eq)]
enum VersionProbe {
    Speaks(DaemonVersion),
    Legacy,
    Unresponsive,
}

fn local_runtime_ready(pane_protocol_current: bool, control_endpoint_matches: bool) -> bool {
    pane_protocol_current && control_endpoint_matches
}

/// The single local-runtime readiness authority used by GUI and CLI callers.
/// A pane-only process is deliberately not ready because it cannot serve Host
/// RPCs such as session discovery or machine-tree synchronization.
pub fn is_ready() -> bool {
    let Ok(mut stream) = transport::connect() else {
        return false;
    };
    matches!(
        query_daemon_version(&mut stream),
        VersionProbe::Speaks(version)
            if local_runtime_ready(
                version.protocol == PROTOCOL_VERSION,
                control_endpoint_matches_recorded_daemon(&version),
            )
    )
}

/// Whether any pane daemon currently accepts a connection. This is weaker than
/// readiness and exists only so lifecycle commands can accurately report that
/// they cleaned up a degraded process.
pub fn is_reachable() -> bool {
    transport::connect().is_ok()
}

fn control_endpoint_matches_recorded_daemon(version: &DaemonVersion) -> bool {
    use crate::daemon::control::{ControlClient, ControlHello, ControlRequest, ReplyOk};

    let Some(expected_pid) = pidfile::read() else {
        return false;
    };
    let hello = ControlHello::host_rpc("agentty-readiness-probe", "this computer");
    let events: crate::daemon::control::EventSink = Box::new(|_| {});
    #[cfg(unix)]
    let client = crate::host::server::control_socket_path()
        .and_then(std::os::unix::net::UnixStream::connect)
        .and_then(|socket| ControlClient::over_unix(socket, &hello, events));
    #[cfg(windows)]
    let client = crate::host::server::connect_control()
        .and_then(|socket| ControlClient::over_tcp(socket, &hello, events));
    #[cfg(not(any(unix, windows)))]
    let client: io::Result<ControlClient> = Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "local Host control endpoint is unavailable on this platform",
    ));
    let Ok(client) = client else {
        return false;
    };
    if client.hello().protocol_version != version.protocol || client.hello().build != version.build
    {
        return false;
    }
    matches!(
        client.call(ControlRequest::Status),
        Ok(ReplyOk::Status(status)) if status.pid == expected_pid
    )
}

const DAEMON_EXE_STEMS: [&str; 3] = ["agentty-app", "agentty-server", "agentty"];

fn strip_exe_suffix(name: &str) -> &str {
    match name.len().checked_sub(4) {
        Some(i) if name.is_char_boundary(i) && name[i..].eq_ignore_ascii_case(".exe") => &name[..i],
        _ => name,
    }
}

fn exe_names_equal(a: &str, b: &str) -> bool {
    let a = strip_exe_suffix(a);
    let b = strip_exe_suffix(b);
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn is_reapable_daemon_name(name: &str) -> bool {
    let own = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    own.as_deref().is_some_and(|own| exe_names_equal(own, name))
        || DAEMON_EXE_STEMS
            .iter()
            .any(|stem| exe_names_equal(stem, name))
}

pub fn ensure_running() -> anyhow::Result<()> {
    with_startup_gate(ensure_running_locked)
}

fn ensure_running_locked() -> anyhow::Result<()> {
    if let Ok(mut stream) = transport::connect() {
        match query_daemon_version(&mut stream) {
            VersionProbe::Speaks(v) if v.protocol == PROTOCOL_VERSION => {
                if local_runtime_ready(true, control_endpoint_matches_recorded_daemon(&v)) {
                    note_local_daemon(Some(v));
                    return Ok(());
                }
                log::warn!(
                    "pane daemon is reachable but its Host control endpoint is absent or owned by another process; restarting the degraded local runtime"
                );
                note_local_daemon(None);
                drop(stream);
                stop_locked();
            }
            VersionProbe::Speaks(v) => {
                log::warn!(
                    "daemon (build {}) speaks protocol {}, this build needs {}; \
                     keeping it and deferring to the user",
                    v.build,
                    v.protocol,
                    PROTOCOL_VERSION
                );
                note_local_daemon(Some(v.clone()));
                if let Ok(mut slot) = MISMATCHED_DAEMON.lock() {
                    *slot = Some(MismatchedDaemon { version: Some(v) });
                }
                return Ok(());
            }
            VersionProbe::Legacy => {
                log::warn!(
                    "daemon predates protocol versioning; keeping it and deferring to the user"
                );
                note_local_daemon(None);
                if let Ok(mut slot) = MISMATCHED_DAEMON.lock() {
                    *slot = Some(MismatchedDaemon { version: None });
                }
                return Ok(());
            }
            VersionProbe::Unresponsive => {
                log::info!("daemon did not answer the version handshake; restarting it");
                note_local_daemon(None);
                drop(stream);
                stop_locked();
            }
        }
    } else {
        reap_recorded_daemon();

        if transport::endpoint_exists() {
            transport::remove_stale_endpoint();
        }
    }

    let (daemon_executable, mut child) = spawn_detached()?;

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Ok(mut stream) = transport::connect() {
            match query_daemon_version(&mut stream) {
                VersionProbe::Speaks(v)
                    if local_runtime_ready(
                        v.protocol == PROTOCOL_VERSION,
                        control_endpoint_matches_recorded_daemon(&v),
                    ) =>
                {
                    note_local_daemon(Some(v));
                    return Ok(());
                }
                _ => {}
            }
        }
        if let Some(status) = child.try_wait().map_err(|error| {
            anyhow::anyhow!(
                "could not inspect detached daemon {}: {error}",
                daemon_executable.display()
            )
        })? {
            anyhow::bail!(
                "local runtime {} exited during startup with {status}; inspect {}",
                daemon_executable.display(),
                daemon_log_display()
            );
        }
        if Instant::now() >= deadline {
            stop_locked();
            anyhow::bail!(
                "local runtime {} did not make both {} and the Host control endpoint ready within {:?}; inspect {}",
                daemon_executable.display(),
                transport::endpoint_display(),
                STARTUP_TIMEOUT,
                daemon_log_display()
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn query_daemon_version(stream: &mut transport::Stream) -> VersionProbe {
    use std::io::Write as _;

    let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    if ClientMsg::Version
        .encode(stream)
        .and_then(|()| stream.flush())
        .is_err()
    {
        return VersionProbe::Unresponsive;
    }
    match DaemonMsg::read(stream) {
        Ok(DaemonMsg::Version(v)) => VersionProbe::Speaks(v),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) =>
        {
            VersionProbe::Unresponsive
        }
        _ => VersionProbe::Legacy,
    }
}

pub fn restart() -> anyhow::Result<()> {
    with_startup_gate(|| {
        stop_locked();
        ensure_running_locked()
    })
}

pub fn stop() {
    if let Err(error) = with_startup_gate(|| {
        stop_locked();
        Ok(())
    }) {
        log::error!("could not acquire the local runtime lifecycle gate for stop: {error}");
    }
}

fn stop_locked() {
    use std::io::Write as _;

    if let Ok(mut stream) = transport::connect() {
        if ClientMsg::Shutdown.encode(&mut stream).is_ok() {
            let _ = stream.flush();
            let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
            while Instant::now() < deadline && transport::connect().is_ok() {
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    }

    reap_recorded_daemon();

    if transport::endpoint_exists() {
        transport::remove_stale_endpoint();
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn reap_recorded_daemon() {
    let Some(pid) = pidfile::read() else { return };
    if pid <= 1 || pid == std::process::id() {
        pidfile::remove();
        return;
    }
    if process_matches_daemon_exe(pid as libc::pid_t) {
        log::warn!("reaping unreachable daemon (pid {pid}); its sessions will be hung up");
        reap_process(pid as libc::pid_t);
    }
    pidfile::remove();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn process_matches_daemon_exe(pid: libc::pid_t) -> bool {
    process_path(pid)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .is_some_and(|name| is_reapable_daemon_name(&name))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn reap_process(pid: libc::pid_t) {
    if signal_and_await_exit(pid, libc::SIGTERM, REAP_TERM_TIMEOUT) {
        return;
    }
    if !signal_and_await_exit(pid, libc::SIGKILL, REAP_KILL_TIMEOUT) {
        log::error!("daemon pid {pid} survived SIGKILL; leaving it behind");
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn signal_and_await_exit(pid: libc::pid_t, sig: libc::c_int, timeout: Duration) -> bool {
    unsafe { libc::kill(pid, sig) };
    let deadline = Instant::now() + timeout;
    while process_alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    true
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn process_alive(pid: libc::pid_t) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(windows)]
fn reap_recorded_daemon() {
    use crate::daemon::winproc;

    let Some(pid) = pidfile::read() else { return };
    if pid <= 4 || pid == std::process::id() {
        pidfile::remove();
        return;
    }
    let procs = winproc::snapshot();
    let matches = procs
        .iter()
        .find(|p| p.pid == pid)
        .is_some_and(|entry| is_reapable_daemon_name(&entry.name));
    if matches {
        log::warn!("reaping unreachable daemon (pid {pid}); its sessions will be hung up");
        for descendant in winproc::descendants(&procs, pid) {
            winproc::terminate(descendant);
        }
        winproc::terminate(pid);
    }
    pidfile::remove();
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn reap_recorded_daemon() {}

fn dedicated_daemon_executable(current_exe: &Path) -> PathBuf {
    let mut executable = current_exe.to_path_buf();
    executable.set_file_name(if cfg!(windows) {
        "agentty-server.exe"
    } else {
        "agentty-server"
    });
    executable
}

pub fn daemon_executable() -> anyhow::Result<PathBuf> {
    if let Some(explicit) = std::env::var_os(SERVER_EXE_ENV).filter(|value| !value.is_empty()) {
        let executable = PathBuf::from(explicit);
        if executable.is_file() {
            return Ok(executable);
        }
        anyhow::bail!(
            "{SERVER_EXE_ENV} points to missing local runtime {}",
            executable.display()
        );
    }

    let current_exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("could not locate own executable: {e}"))?;
    for candidate in sibling_daemon_candidates(&current_exe) {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    let executable = dedicated_daemon_executable(&current_exe);
    anyhow::bail!(
        "dedicated local runtime is missing at {}; install or build agentty-server beside {} \
         (dev: `cargo build -p agentty-server --locked` or `cargo app`)",
        executable.display(),
        current_exe.display()
    );
}

fn sibling_daemon_candidates(current_exe: &Path) -> Vec<PathBuf> {
    let sibling = dedicated_daemon_executable(current_exe);
    let mut candidates = vec![sibling.clone()];
    let Some(profile_dir) = sibling.parent() else {
        return candidates;
    };
    if profile_dir.file_name().is_some_and(|name| name == "debug") {
        if let Some(target_dir) = profile_dir.parent() {
            if let Some(name) = sibling.file_name() {
                candidates.push(target_dir.join("release").join(name));
            }
        }
    }
    candidates
}

fn daemon_log_display() -> String {
    config::config_path("agentty.log")
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "the Agentty config directory's agentty.log".to_string())
}

fn spawn_detached() -> anyhow::Result<(PathBuf, Child)> {
    let exe = daemon_executable()?;

    let mut cmd = Command::new(&exe);
    cmd.arg("--daemon");

    if let Some(dir) = config::config_dir_path() {
        cmd.arg("--config-dir").arg(dir);
    }

    if let Some(shell) = detect_parent_shell() {
        cmd.env(crate::daemon::DETECTED_SHELL_ENV, shell);
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    detach(&mut cmd);

    match cmd.spawn() {
        Ok(child) => Ok((exe, child)),
        Err(e) => Err(anyhow::anyhow!("failed to spawn daemon process: {e}")),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn detect_parent_shell() -> Option<PathBuf> {
    process_path(unsafe { libc::getppid() }).filter(|path| is_supported_shell(path))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn detect_parent_shell() -> Option<PathBuf> {
    None
}

fn is_supported_shell(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "zsh" | "bash" | "fish" | "pwsh" | "powershell" | "powershell.exe" | "pwsh.exe"
    )
}

#[cfg(target_os = "macos")]
fn process_path(pid: libc::pid_t) -> Option<PathBuf> {
    if pid <= 0 {
        return None;
    }
    let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let len =
        unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
    if len <= 0 {
        return None;
    }
    Some(PathBuf::from(
        String::from_utf8_lossy(&buf[..len as usize]).into_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn process_path(pid: libc::pid_t) -> Option<PathBuf> {
    if pid <= 0 {
        return None;
    }
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;

    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(test)]
mod exe_name_tests {
    use super::*;

    #[test]
    fn every_legitimate_daemon_name_is_reapable_with_and_without_exe() {
        for name in [
            "agentty-app",
            "agentty-server",
            "agentty",
            "agentty-app.exe",
            "agentty-server.exe",
            "agentty.exe",
        ] {
            assert!(is_reapable_daemon_name(name), "{name} is a daemon of ours");
        }
    }

    #[test]
    fn the_current_executable_name_remains_reapable() {
        let own = std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            is_reapable_daemon_name(&own),
            "{own} launched this process and must stay in the set"
        );
    }

    #[test]
    fn foreign_process_names_are_never_reapable() {
        for name in [
            "explorer.exe",
            "sleep",
            "agenttyd",
            "notagentty",
            "agentty-app2",
            "agentty.",
            "",
        ] {
            assert!(
                !is_reapable_daemon_name(name),
                "{name:?} must be protected from the reap"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_matches_daemon_names_case_insensitively() {
        assert!(is_reapable_daemon_name("AGENTTY-APP.EXE"));
        assert!(is_reapable_daemon_name("Agentty-Server"));
        assert!(is_reapable_daemon_name("AGENTTY"));
    }

    #[test]
    fn strip_exe_suffix_only_strips_a_trailing_exe() {
        assert_eq!(strip_exe_suffix("agentty-app.exe"), "agentty-app");
        assert_eq!(strip_exe_suffix("agentty-app.EXE"), "agentty-app");
        assert_eq!(strip_exe_suffix("agentty-app"), "agentty-app");
        assert_eq!(strip_exe_suffix(".exe"), "");
        assert_eq!(strip_exe_suffix("exe"), "exe");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    #[test]
    fn pane_endpoint_without_control_endpoint_is_degraded() {
        assert!(!local_runtime_ready(true, false));
        assert!(!local_runtime_ready(false, true));
        assert!(local_runtime_ready(true, true));
    }

    #[test]
    fn concurrent_lifecycle_callers_share_one_startup_gate() {
        let dir = tempfile::TempDir::new().unwrap();
        let gate = Arc::new(dir.path().join("daemon.start.lock"));
        let barrier = Arc::new(Barrier::new(5));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut callers = Vec::new();
        for _ in 0..4 {
            let barrier = barrier.clone();
            let gate = gate.clone();
            let active = active.clone();
            let maximum = maximum.clone();
            callers.push(std::thread::spawn(move || {
                barrier.wait();
                with_startup_gate_at(&gate, || {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(10));
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .unwrap();
            }));
        }
        barrier.wait();
        for caller in callers {
            caller.join().unwrap();
        }
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn daemon_executable_is_the_headless_sibling() {
        let gui = Path::new("/Applications/Agentty.app/Contents/MacOS/agentty-app");
        assert_eq!(
            dedicated_daemon_executable(gui),
            Path::new("/Applications/Agentty.app/Contents/MacOS/agentty-server")
        );
        #[cfg(windows)]
        {
            let windows = Path::new(r"C:\\Program Files\\Agentty\\agentty-app.exe");
            assert_eq!(
                dedicated_daemon_executable(windows),
                Path::new(r"C:\\Program Files\\Agentty\\agentty-server.exe")
            );
        }
    }

    #[test]
    fn sibling_daemon_candidates_include_release_fallback_for_debug_gui() {
        let gui = Path::new("/repo/target/debug/agentty-app");
        let candidates = sibling_daemon_candidates(gui);
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/repo/target/debug/agentty-server"),
                PathBuf::from("/repo/target/release/agentty-server"),
            ]
        );
    }

    #[test]
    fn supported_shell_detection_matches_shell_basenames_only() {
        assert!(is_supported_shell(Path::new("/opt/homebrew/bin/fish")));
        assert!(is_supported_shell(Path::new("/bin/zsh")));
        assert!(is_supported_shell(Path::new("/usr/bin/bash")));
        assert!(!is_supported_shell(Path::new(
            "/Applications/kitty.app/kitty"
        )));
        assert!(!is_supported_shell(Path::new("/usr/bin/omp")));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn reap_guard_rejects_a_live_process_of_another_executable() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as libc::pid_t;

        assert!(process_alive(pid), "the sleep child is alive and ours");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let basename = process_path(pid).and_then(|p| p.file_name().map(|n| n.to_os_string()));
            if basename == Some("sleep".into()) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "process_path resolves an arbitrary pid, not just our parent \
                 (still {basename:?} after 5s)"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !process_matches_daemon_exe(pid),
            "sleep must not match any daemon name; matching here would mean the reap could kill it"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn signal_and_await_exit_observes_the_death_it_caused() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as libc::pid_t;
        let reaper = std::thread::spawn(move || {
            let _ = child.wait();
        });

        assert!(
            signal_and_await_exit(pid, libc::SIGTERM, std::time::Duration::from_secs(5)),
            "the child must be seen exiting within the grace window"
        );
        assert!(!process_alive(pid), "and be gone afterwards");
        reaper.join().unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn process_alive_is_false_once_the_process_is_gone() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as libc::pid_t;
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!process_alive(pid));
    }

    #[test]
    fn version_handshake_reads_a_matching_reply() {
        use crate::daemon::protocol::{ClientMsg, DaemonMsg, DaemonVersion, PROTOCOL_VERSION};

        let (mut client, mut daemon) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let msg = ClientMsg::read(&mut daemon).unwrap();
            assert_eq!(msg, ClientMsg::Version);
            DaemonMsg::Version(DaemonVersion {
                protocol: PROTOCOL_VERSION,
                build: "test".into(),
                features: Vec::new(),
                instance: "inst-test".into(),
            })
            .encode(&mut daemon)
            .unwrap();
        });

        match query_daemon_version(&mut client) {
            VersionProbe::Speaks(got) => {
                assert_eq!(got.protocol, PROTOCOL_VERSION);
                assert_eq!(got.build, "test");
            }
            other => panic!("a live daemon must answer, got {other:?}"),
        }
        server.join().unwrap();
    }

    #[test]
    fn version_handshake_treats_a_hangup_as_legacy() {
        use crate::daemon::protocol::ClientMsg;

        let (mut client, mut daemon) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let _ = ClientMsg::read(&mut daemon);
            drop(daemon);
        });

        assert_eq!(query_daemon_version(&mut client), VersionProbe::Legacy);
        server.join().unwrap();
    }

    #[test]
    fn version_handshake_treats_silence_as_unresponsive() {
        let (mut client, daemon) = UnixStream::pair().unwrap();
        let start = Instant::now();
        assert_eq!(
            query_daemon_version(&mut client),
            VersionProbe::Unresponsive
        );
        assert!(start.elapsed() >= HANDSHAKE_TIMEOUT);
        drop(daemon);
    }

    #[test]
    fn connect_to_stale_socket_path_fails() {
        let dir = std::env::temp_dir().join(format!("agentty-spawn-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("daemon.sock");
        let err = UnixStream::connect(&path).unwrap_err();
        assert!(matches!(
            err.kind(),
            ErrorKind::NotFound | ErrorKind::ConnectionRefused
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
