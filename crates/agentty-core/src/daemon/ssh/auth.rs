#[cfg(unix)]
use std::net::IpAddr;
use std::sync::Arc;

use russh::client::{AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{Algorithm, HashAlg, PrivateKeyWithHashAlg, PublicKey};
#[cfg(all(unix, feature = "gssapi"))]
use russh::{GssapiAuthenticator, GssapiStep};
use russh::{MethodKind, MethodSet};

use crate::daemon::protocol::{AuthPromptKind, AuthResponse, KiPrompt, NativeSshSpec, SshAuthMode};

use super::broker::PromptBroker;
use super::handler::ClientHandler;

pub async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    spec: &NativeSshSpec,
    broker: &Arc<PromptBroker>,
) -> Result<(), String> {
    let user = spec.user.clone();

    let mut remaining = match handle
        .authenticate_none(&user)
        .await
        .map_err(|e| format!("auth (none) failed: {e}"))?
    {
        AuthResult::Success => return Ok(()),
        AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods,
    };

    let mut last_reason = "authentication failed".to_string();

    for family in method_order(spec.auth_mode) {
        if !remaining.is_empty() && !remaining.contains(&family) {
            continue;
        }
        let outcome = match family {
            MethodKind::GssapiWithMic => try_gssapi(handle, spec).await,
            MethodKind::PublicKey => try_publickeys(handle, spec, broker).await,
            MethodKind::KeyboardInteractive => try_keyboard_interactive(handle, spec, broker).await,
            MethodKind::Password => try_password(handle, spec, broker).await,
            _ => Outcome::Skipped,
        };
        match outcome {
            Outcome::Authenticated => return Ok(()),
            Outcome::Failed {
                remaining_methods,
                reason,
            } => {
                if let Some(m) = remaining_methods
                    && !m.is_empty()
                {
                    remaining = m;
                }
                if let Some(r) = reason {
                    last_reason = r;
                }
            }
            Outcome::Skipped => {}
        }
    }

    Err(last_reason)
}

fn method_order(mode: SshAuthMode) -> Vec<MethodKind> {
    match mode {
        SshAuthMode::Auto => vec![
            MethodKind::GssapiWithMic,
            MethodKind::PublicKey,
            MethodKind::Password,
            MethodKind::KeyboardInteractive,
        ],
        SshAuthMode::Gssapi => vec![MethodKind::GssapiWithMic],
        SshAuthMode::PublicKey | SshAuthMode::Agent => vec![MethodKind::PublicKey],
        SshAuthMode::Password => vec![MethodKind::Password],
        SshAuthMode::KeyboardInteractive => vec![MethodKind::KeyboardInteractive],
    }
}

enum Outcome {
    Authenticated,
    Failed {
        remaining_methods: Option<MethodSet>,
        reason: Option<String>,
    },
    Skipped,
}

fn failed(reason: impl Into<String>) -> Outcome {
    Outcome::Failed {
        remaining_methods: None,
        reason: Some(reason.into()),
    }
}

#[cfg(all(unix, feature = "gssapi"))]
const KRB5_DER_OID: &[u8] = b"\x06\x09\x2a\x86\x48\x86\xf7\x12\x01\x02\x02";

#[cfg(all(unix, feature = "gssapi"))]
struct GssapiClient {
    ctx: libgssapi::context::ClientCtx,
}

#[cfg(all(unix, feature = "gssapi"))]
#[derive(Debug)]
enum GssapiAuthError {
    Send(russh::SendError),
    Gssapi(libgssapi::error::Error),
    Other(String),
}

#[cfg(all(unix, feature = "gssapi"))]
impl std::fmt::Display for GssapiAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GssapiAuthError::Send(_) => write!(f, "send error"),
            GssapiAuthError::Gssapi(e) => write!(f, "{e}"),
            GssapiAuthError::Other(e) => write!(f, "{e}"),
        }
    }
}

#[cfg(all(unix, feature = "gssapi"))]
impl From<russh::SendError> for GssapiAuthError {
    fn from(value: russh::SendError) -> Self {
        GssapiAuthError::Send(value)
    }
}

#[cfg(all(unix, feature = "gssapi"))]
impl From<libgssapi::error::Error> for GssapiAuthError {
    fn from(value: libgssapi::error::Error) -> Self {
        GssapiAuthError::Gssapi(value)
    }
}

#[cfg(all(unix, feature = "gssapi"))]
impl GssapiAuthenticator for GssapiClient {
    type Error = GssapiAuthError;

    async fn gssapi_step(
        &mut self,
        selected_mechanism: Vec<u8>,
        input_token: Option<Vec<u8>>,
        mic_data: Vec<u8>,
    ) -> Result<GssapiStep, Self::Error> {
        use libgssapi::context::SecurityContext;

        if input_token.is_none() && selected_mechanism != KRB5_DER_OID {
            return Err(GssapiAuthError::Other(
                "server selected an unsupported gssapi mechanism".to_string(),
            ));
        }
        let output = self.ctx.step(input_token.as_deref(), None)?;
        if self.ctx.is_complete() {
            let mic = self.ctx.get_mic(&mic_data)?;
            Ok(GssapiStep::Complete {
                token: output.map(|buf| buf.to_vec()),
                mic: Some(mic.to_vec()),
            })
        } else {
            let Some(token) = output else {
                return Err(GssapiAuthError::Other(
                    "gssapi context stalled: incomplete with no output token".to_string(),
                ));
            };
            Ok(GssapiStep::Continue {
                token: token.to_vec(),
            })
        }
    }
}

async fn try_gssapi(handle: &mut Handle<ClientHandler>, spec: &NativeSshSpec) -> Outcome {
    #[cfg(all(unix, feature = "gssapi"))]
    {
        use libgssapi::context::{ClientCtx, CtxFlags};
        use libgssapi::name::Name;
        use libgssapi::oid::{GSS_MECH_KRB5, GSS_NT_HOSTBASED_SERVICE};

        let service_hosts = gssapi_service_hosts(&spec.host).await;
        let mut tried = Vec::new();
        let mut errors = Vec::new();
        let mut last_remaining = None;
        let mut saw_rejection = false;

        for service_host in service_hosts {
            let service = format!("host@{service_host}");
            tried.push(service.clone());
            let name = match Name::new(service.as_bytes(), Some(GSS_NT_HOSTBASED_SERVICE)) {
                Ok(name) => name,
                Err(e) => {
                    errors.push(format!("{service}: target name error: {e}"));
                    continue;
                }
            };
            let mut client = GssapiClient {
                ctx: ClientCtx::new(
                    None,
                    name,
                    CtxFlags::GSS_C_MUTUAL_FLAG | CtxFlags::GSS_C_INTEG_FLAG,
                    Some(GSS_MECH_KRB5),
                ),
            };

            match handle
                .authenticate_gssapi_with_mic(&spec.user, vec![KRB5_DER_OID.to_vec()], &mut client)
                .await
            {
                Ok(AuthResult::Success) => return Outcome::Authenticated,
                Ok(AuthResult::Failure {
                    remaining_methods, ..
                }) => {
                    saw_rejection = true;
                    let can_retry = remaining_methods.is_empty()
                        || remaining_methods.contains(&MethodKind::GssapiWithMic);
                    last_remaining = Some(remaining_methods);
                    if !can_retry {
                        break;
                    }
                }
                Err(e) => {
                    errors.push(format!("{service}: {e}"));
                    break;
                }
            }
        }

        let tried = tried.join(", ");
        if saw_rejection {
            return Outcome::Failed {
                remaining_methods: last_remaining,
                reason: Some(format!("gssapi rejected (tried {tried})")),
            };
        }
        if errors.is_empty() {
            failed(format!("gssapi auth error (tried {tried})"))
        } else {
            failed(format!(
                "gssapi auth error (tried {tried}): {}",
                errors.join("; ")
            ))
        }
    }
    #[cfg(not(all(unix, feature = "gssapi")))]
    {
        let _ = (handle, spec);
        failed("gssapi auth is not available in this build")
    }
}

#[cfg(all(unix, feature = "gssapi"))]
async fn gssapi_service_hosts(host: &str) -> Vec<String> {
    let host = host.to_string();
    let fallback = host.clone();
    tokio::task::spawn_blocking(move || gssapi_service_hosts_blocking(&host))
        .await
        .unwrap_or_else(|_| vec![fallback])
}

#[cfg(all(unix, feature = "gssapi"))]
fn gssapi_service_hosts_blocking(host: &str) -> Vec<String> {
    gssapi_service_hosts_with_lookup(host, reverse_lookup_addr)
}

#[cfg(unix)]
#[cfg_attr(not(feature = "gssapi"), allow(dead_code))]
fn gssapi_service_hosts_with_lookup(
    host: &str,
    reverse_lookup: impl FnOnce(IpAddr) -> Option<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    out.push(host.to_string());
    if let Ok(ip) = host.parse::<IpAddr>()
        && let Some(name) = reverse_lookup(ip).map(|name| name.trim_end_matches('.').to_string())
        && !name.is_empty()
    {
        out.push(name);
    }
    out.dedup();
    out
}

#[cfg(all(unix, feature = "gssapi"))]
fn reverse_lookup_addr(ip: IpAddr) -> Option<String> {
    match ip {
        IpAddr::V4(ip) => reverse_lookup_v4(ip),
        IpAddr::V6(ip) => reverse_lookup_v6(ip),
    }
}

#[cfg(all(unix, feature = "gssapi"))]
fn reverse_lookup_v4(ip: std::net::Ipv4Addr) -> Option<String> {
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    set_sockaddr_in_len(&mut addr);
    addr.sin_family = libc::AF_INET as _;
    addr.sin_addr = libc::in_addr {
        s_addr: u32::from_ne_bytes(ip.octets()),
    };
    reverse_lookup_sockaddr(
        &addr as *const libc::sockaddr_in as *const libc::sockaddr,
        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
    )
}

#[cfg(all(unix, feature = "gssapi"))]
fn reverse_lookup_v6(ip: std::net::Ipv6Addr) -> Option<String> {
    let mut addr: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
    set_sockaddr_in6_len(&mut addr);
    addr.sin6_family = libc::AF_INET6 as _;
    addr.sin6_addr = libc::in6_addr {
        s6_addr: ip.octets(),
    };
    reverse_lookup_sockaddr(
        &addr as *const libc::sockaddr_in6 as *const libc::sockaddr,
        std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
    )
}

#[cfg(all(unix, feature = "gssapi"))]
fn reverse_lookup_sockaddr(addr: *const libc::sockaddr, len: libc::socklen_t) -> Option<String> {
    const NI_MAXHOST_FALLBACK: usize = 1025;
    let mut host = [0 as libc::c_char; NI_MAXHOST_FALLBACK];
    let rc = unsafe {
        libc::getnameinfo(
            addr,
            len,
            host.as_mut_ptr(),
            host.len() as libc::socklen_t,
            std::ptr::null_mut(),
            0,
            libc::NI_NAMEREQD,
        )
    };
    if rc != 0 {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(host.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    (!name.is_empty()).then_some(name)
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
#[cfg(all(unix, feature = "gssapi"))]
fn set_sockaddr_in_len(addr: &mut libc::sockaddr_in) {
    addr.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
#[cfg(all(unix, feature = "gssapi"))]
fn set_sockaddr_in_len(_addr: &mut libc::sockaddr_in) {}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
#[cfg(all(unix, feature = "gssapi"))]
fn set_sockaddr_in6_len(addr: &mut libc::sockaddr_in6) {
    addr.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
#[cfg(all(unix, feature = "gssapi"))]
fn set_sockaddr_in6_len(_addr: &mut libc::sockaddr_in6) {}

async fn try_publickeys(
    handle: &mut Handle<ClientHandler>,
    spec: &NativeSshSpec,
    broker: &Arc<PromptBroker>,
) -> Outcome {
    let identities = identity_files_to_try(spec);
    let mut diagnostic = PublicKeyDiagnostic::new(spec, &identities);
    let mut last: Option<MethodSet> = None;

    if spec.auth_mode != SshAuthMode::Agent {
        for path in &identities {
            match try_identity_file(handle, spec, broker, path).await {
                Outcome::Authenticated => return Outcome::Authenticated,
                Outcome::Failed {
                    remaining_methods,
                    reason,
                } => {
                    diagnostic.note_identity_failure(path, reason.as_deref());
                    if remaining_methods.is_some() {
                        last = remaining_methods;
                    }
                }
                Outcome::Skipped => diagnostic.note_identity_skipped(path),
            }
        }
    }

    if spec.auth_mode != SshAuthMode::PublicKey {
        let agent = try_agent(handle, spec).await;
        diagnostic.note_agent_outcome(&agent);
        match agent {
            Outcome::Authenticated => return Outcome::Authenticated,
            Outcome::Failed {
                remaining_methods, ..
            } => {
                if remaining_methods.is_some() {
                    last = remaining_methods;
                }
            }
            Outcome::Skipped => {}
        }
    }

    Outcome::Failed {
        remaining_methods: last,
        reason: Some(diagnostic.message()),
    }
}

#[derive(Debug)]
struct PublicKeyDiagnostic {
    explicit_identities: bool,
    identity_candidates: Vec<String>,
    identity_failures: Vec<String>,
    identity_skipped: Vec<String>,
    agent_attempted: bool,
    agent_reason: Option<String>,
    agent_unavailable: bool,
}

impl PublicKeyDiagnostic {
    fn new(spec: &NativeSshSpec, identities: &[String]) -> Self {
        Self {
            explicit_identities: !spec.identity_files.is_empty(),
            identity_candidates: identities.to_vec(),
            identity_failures: Vec::new(),
            identity_skipped: Vec::new(),
            agent_attempted: false,
            agent_reason: None,
            agent_unavailable: false,
        }
    }

    fn note_identity_failure(&mut self, path: &str, reason: Option<&str>) {
        self.identity_failures.push(match reason {
            Some(reason) => format!("{path}: {reason}"),
            None => path.to_string(),
        });
    }

    fn note_identity_skipped(&mut self, path: &str) {
        self.identity_skipped.push(path.to_string());
    }

    fn note_agent_outcome(&mut self, outcome: &Outcome) {
        self.agent_attempted = true;
        match outcome {
            Outcome::Authenticated => {}
            Outcome::Failed { reason, .. } => self.agent_reason = reason.clone(),
            Outcome::Skipped => self.agent_unavailable = true,
        }
    }

    fn message(&self) -> String {
        let mut parts = Vec::new();
        if self.identity_candidates.is_empty() {
            if self.explicit_identities {
                parts.push("no usable IdentityFile candidate was configured".to_string());
            } else {
                parts.push(
                    "no OpenSSH default identity file exists (~/.ssh/id_rsa, id_ecdsa, id_ecdsa_sk, id_ed25519, id_ed25519_sk, or id_dsa)"
                        .to_string(),
                );
            }
        } else if !self.identity_failures.is_empty() {
            parts.push(format!(
                "identity files tried: {}",
                self.identity_failures.join("; ")
            ));
        } else if !self.identity_skipped.is_empty() {
            parts.push(format!(
                "identity files were unusable or cancelled: {}",
                self.identity_skipped.join(", ")
            ));
        }

        if self.agent_attempted {
            if let Some(reason) = &self.agent_reason {
                parts.push(format!("ssh-agent: {reason}"));
            } else if self.agent_unavailable {
                parts.push("ssh-agent is unavailable or contains no usable identities".to_string());
            }
        }

        if parts.is_empty() {
            parts.push("the server rejected every public-key identity".to_string());
        }
        parts.push(
            "Add the correct IdentityFile to the SSH profile/config, or load a key with `ssh-add` and retry"
                .to_string(),
        );
        format!("public-key authentication failed: {}", parts.join(". "))
    }
}

const OPENSSH_DEFAULT_IDENTITY_FILES: &[&str] = &[
    "~/.ssh/id_rsa",
    "~/.ssh/id_ecdsa",
    "~/.ssh/id_ecdsa_sk",
    "~/.ssh/id_ed25519",
    "~/.ssh/id_ed25519_sk",
    "~/.ssh/id_dsa",
];

fn identity_files_to_try(spec: &NativeSshSpec) -> Vec<String> {
    identity_files_to_try_with_home(spec, home_dir().as_deref())
}

fn identity_files_to_try_with_home(spec: &NativeSshSpec, home: Option<&str>) -> Vec<String> {
    if !spec.identity_files.is_empty() {
        return spec.identity_files.clone();
    }

    // OpenSSH supplies this default identity set when no IdentityFile option is
    // present. Imported profiles previously lost that implicit behaviour, so a
    // host that worked with `ssh alias` had no key candidates in agentty unless an
    // ssh-agent happened to be populated. Preserve OpenSSH semantics at the
    // authentication boundary without persisting machine-specific defaults.
    OPENSSH_DEFAULT_IDENTITY_FILES
        .iter()
        .filter(|path| identity_path_with_home(path, &spec.host, &spec.user, home).is_file())
        .map(|path| (*path).to_string())
        .collect()
}

fn identity_path_with_home(
    path: &str,
    host: &str,
    user: &str,
    home: Option<&str>,
) -> std::path::PathBuf {
    let substituted = path.replace("%h", host).replace("%r", user);
    if let Some(rest) = substituted.strip_prefix("~/")
        && let Some(home) = home
    {
        return std::path::Path::new(home).join(rest);
    }
    std::path::PathBuf::from(substituted)
}

async fn try_identity_file(
    handle: &mut Handle<ClientHandler>,
    spec: &NativeSshSpec,
    broker: &Arc<PromptBroker>,
    raw_path: &str,
) -> Outcome {
    let path = expand_identity_path(raw_path, &spec.host, &spec.user);
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return failed(format!("cannot read identity file {path}: {e}")),
    };

    if PublicKey::from_openssh(contents.trim()).is_ok() {
        log::warn!("identity file {path} is a public key; skipping");
        return Outcome::Skipped;
    }

    let key = match russh::keys::decode_secret_key(&contents, None) {
        Ok(k) => k,
        Err(russh::keys::Error::KeyIsEncrypted) => {
            let provided = spec
                .key_passphrases
                .as_ref()
                .and_then(|m| m.get(raw_path))
                .cloned();
            let passphrase = match provided {
                Some(p) => p,
                None => {
                    let resp = broker
                        .prompt(AuthPromptKind::KeyPassphrase {
                            key_path: raw_path.to_string(),
                            comment: String::new(),
                        })
                        .await;
                    match resp {
                        AuthResponse::Secret(p) => p,
                        _ => return Outcome::Skipped,
                    }
                }
            };
            match russh::keys::decode_secret_key(&contents, Some(&passphrase)) {
                Ok(k) => k,
                Err(e) => {
                    log::warn!("could not decrypt identity file {path}: {e}");
                    return failed(format!("could not decrypt identity file {path}"));
                }
            }
        }
        Err(e) => {
            log::warn!("could not read identity file {path}: {e}");
            return failed(format!("could not read identity file {path}"));
        }
    };

    let hash_alg = rsa_hash_alg(&key.algorithm());
    let pk = PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg);
    match handle.authenticate_publickey(&spec.user, pk).await {
        Ok(AuthResult::Success) => Outcome::Authenticated,
        Ok(AuthResult::Failure {
            remaining_methods, ..
        }) => Outcome::Failed {
            remaining_methods: Some(remaining_methods),
            reason: Some(format!("server rejected key {raw_path}")),
        },
        Err(e) => failed(format!("public-key auth error: {e}")),
    }
}

async fn try_agent(handle: &mut Handle<ClientHandler>, spec: &NativeSshSpec) -> Outcome {
    #[cfg(unix)]
    {
        let agent = match AgentClient::connect_env().await {
            Ok(a) => a,
            Err(_) => return Outcome::Skipped,
        };
        try_agent_identities(handle, spec, agent).await
    }
    #[cfg(windows)]
    {
        let pipe = std::env::var("SSH_AUTH_SOCK")
            .unwrap_or_else(|_| r"\\.\pipe\openssh-ssh-agent".to_string());
        let agent = match AgentClient::connect_named_pipe(&pipe).await {
            Ok(a) => a,
            Err(_) => return Outcome::Skipped,
        };
        try_agent_identities(handle, spec, agent).await
    }
}

async fn try_agent_identities<S>(
    handle: &mut Handle<ClientHandler>,
    spec: &NativeSshSpec,
    mut agent: AgentClient<S>,
) -> Outcome
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    let identities = match agent.request_identities().await {
        Ok(ids) => ids,
        Err(_) => return Outcome::Skipped,
    };
    let mut last: Option<MethodSet> = None;
    for identity in identities {
        let pubkey: PublicKey = match &identity {
            AgentIdentity::PublicKey { key, .. } => key.clone(),
            AgentIdentity::Certificate { .. } => continue,
        };
        let hash_alg = rsa_hash_alg(&pubkey.algorithm());
        match handle
            .authenticate_publickey_with(&spec.user, pubkey, hash_alg, &mut agent)
            .await
        {
            Ok(AuthResult::Success) => return Outcome::Authenticated,
            Ok(AuthResult::Failure {
                remaining_methods, ..
            }) => last = Some(remaining_methods),
            Err(_) => continue,
        }
    }
    Outcome::Failed {
        remaining_methods: last,
        reason: Some("no agent key was accepted".to_string()),
    }
}

async fn try_password(
    handle: &mut Handle<ClientHandler>,
    spec: &NativeSshSpec,
    broker: &Arc<PromptBroker>,
) -> Outcome {
    if let Some(pw) = &spec.password {
        match handle.authenticate_password(&spec.user, pw.clone()).await {
            Ok(AuthResult::Success) => return Outcome::Authenticated,
            Ok(AuthResult::Failure { .. }) => {}
            Err(e) => return failed(format!("password auth error: {e}")),
        }
    }

    let resp = broker
        .prompt(AuthPromptKind::Password {
            user: spec.user.clone(),
            host: spec.host.clone(),
        })
        .await;
    let pw = match resp {
        AuthResponse::Secret(p) => p,
        _ => return failed("password entry cancelled"),
    };
    match handle.authenticate_password(&spec.user, pw).await {
        Ok(AuthResult::Success) => Outcome::Authenticated,
        Ok(AuthResult::Failure {
            remaining_methods, ..
        }) => Outcome::Failed {
            remaining_methods: Some(remaining_methods),
            reason: Some("password rejected".to_string()),
        },
        Err(e) => failed(format!("password auth error: {e}")),
    }
}

async fn try_keyboard_interactive(
    handle: &mut Handle<ClientHandler>,
    spec: &NativeSshSpec,
    broker: &Arc<PromptBroker>,
) -> Outcome {
    let mut resp = match handle
        .authenticate_keyboard_interactive_start(&spec.user, None)
        .await
    {
        Ok(r) => r,
        Err(e) => return failed(format!("keyboard-interactive start error: {e}")),
    };

    const MAX_ROUNDS: u32 = 16;
    let mut rounds = 0u32;
    let mut stored_password_used = false;
    loop {
        rounds += 1;
        if rounds > MAX_ROUNDS {
            return failed("keyboard-interactive gave up after too many rounds");
        }
        match resp {
            KeyboardInteractiveAuthResponse::Success => return Outcome::Authenticated,
            KeyboardInteractiveAuthResponse::Failure {
                remaining_methods, ..
            } => {
                return Outcome::Failed {
                    remaining_methods: Some(remaining_methods),
                    reason: Some("keyboard-interactive rejected".to_string()),
                };
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                if prompts.is_empty() {
                    resp = match handle
                        .authenticate_keyboard_interactive_respond(Vec::new())
                        .await
                    {
                        Ok(r) => r,
                        Err(e) => return failed(format!("keyboard-interactive error: {e}")),
                    };
                    continue;
                }

                let allow_stored = !stored_password_used;
                stored_password_used = true;
                let answers = match collect_ki_answers(
                    spec,
                    broker,
                    &name,
                    &instructions,
                    &prompts,
                    allow_stored,
                )
                .await
                {
                    Some(a) => a,
                    None => return failed("keyboard-interactive cancelled"),
                };
                resp = match handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                {
                    Ok(r) => r,
                    Err(e) => return failed(format!("keyboard-interactive error: {e}")),
                };
            }
        }
    }
}

async fn collect_ki_answers(
    spec: &NativeSshSpec,
    broker: &Arc<PromptBroker>,
    name: &str,
    instructions: &str,
    prompts: &[russh::client::Prompt],
    allow_stored: bool,
) -> Option<Vec<String>> {
    let all_password_type = prompts
        .iter()
        .all(|p| !p.echo && p.prompt.to_lowercase().contains("password"));
    if all_password_type && allow_stored {
        if let Some(pw) = &spec.password {
            return Some(prompts.iter().map(|_| pw.clone()).collect());
        }
    }

    let ki_prompts: Vec<KiPrompt> = prompts
        .iter()
        .map(|p| KiPrompt {
            text: p.prompt.clone(),
            echo: p.echo,
        })
        .collect();
    let resp = broker
        .prompt(AuthPromptKind::KeyboardInteractive {
            name: name.to_string(),
            instructions: instructions.to_string(),
            prompts: ki_prompts,
        })
        .await;
    match resp {
        AuthResponse::Secrets(v) if v.len() == prompts.len() => Some(v),
        AuthResponse::Secret(s) if prompts.len() == 1 => Some(vec![s]),
        _ => None,
    }
}

fn rsa_hash_alg(algorithm: &Algorithm) -> Option<HashAlg> {
    if matches!(algorithm, Algorithm::Rsa { .. }) {
        Some(HashAlg::Sha256)
    } else {
        None
    }
}

fn expand_identity_path(path: &str, host: &str, user: &str) -> String {
    identity_path_with_home(path, host, user, home_dir().as_deref())
        .to_string_lossy()
        .into_owned()
}

#[cfg(unix)]
fn home_dir() -> Option<String> {
    std::env::var("HOME").ok().filter(|h| !h.is_empty())
}

#[cfg(not(unix))]
fn home_dir() -> Option<String> {
    std::env::var("USERPROFILE").ok().filter(|h| !h.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_path_expands_tokens_and_tilde() {
        let p = expand_identity_path("/keys/%r@%h/id", "example.com", "deploy");
        assert_eq!(p, "/keys/deploy@example.com/id");
    }

    #[test]
    fn explicit_identity_files_take_precedence_over_openssh_defaults() {
        let spec = test_spec(vec!["~/.ssh/custom".to_string()]);
        assert_eq!(identity_files_to_try(&spec), vec!["~/.ssh/custom"]);
    }

    #[test]
    fn openssh_defaults_discover_existing_keys_when_identityfile_is_absent() {
        let root =
            std::env::temp_dir().join(format!("agentty-default-identities-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".ssh")).unwrap();
        std::fs::write(root.join(".ssh/id_ed25519"), "fixture").unwrap();
        std::fs::write(root.join(".ssh/id_rsa"), "fixture").unwrap();

        let spec = test_spec(Vec::new());
        assert_eq!(
            identity_files_to_try_with_home(&spec, root.to_str()),
            vec!["~/.ssh/id_rsa", "~/.ssh/id_ed25519"]
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn diagnostic_explains_missing_default_keys_and_empty_agent() {
        let spec = test_spec(Vec::new());
        let mut diagnostic = PublicKeyDiagnostic::new(&spec, &[]);
        diagnostic.note_agent_outcome(&Outcome::Skipped);
        let message = diagnostic.message();
        assert!(message.contains("no OpenSSH default identity file exists"));
        assert!(message.contains("ssh-agent is unavailable or contains no usable identities"));
        assert!(message.contains("IdentityFile"));
        assert!(message.contains("ssh-add"));
    }

    #[test]
    fn diagnostic_reports_rejected_identity_without_key_material() {
        let spec = test_spec(vec!["~/.ssh/work".into()]);
        let mut diagnostic = PublicKeyDiagnostic::new(&spec, &["~/.ssh/work".into()]);
        diagnostic.note_identity_failure("~/.ssh/work", Some("server rejected key ~/.ssh/work"));
        diagnostic.note_agent_outcome(&Outcome::Failed {
            remaining_methods: None,
            reason: Some("no agent key was accepted".into()),
        });
        let message = diagnostic.message();
        assert!(message.contains("~/.ssh/work"));
        assert!(message.contains("server rejected key"));
        assert!(message.contains("no agent key was accepted"));
        assert!(!message.contains("PRIVATE KEY"));
    }

    #[test]
    fn openssh_default_identity_order_matches_client_defaults() {
        assert_eq!(
            OPENSSH_DEFAULT_IDENTITY_FILES,
            &[
                "~/.ssh/id_rsa",
                "~/.ssh/id_ecdsa",
                "~/.ssh/id_ecdsa_sk",
                "~/.ssh/id_ed25519",
                "~/.ssh/id_ed25519_sk",
                "~/.ssh/id_dsa",
            ]
        );
    }

    fn test_spec(identity_files: Vec<String>) -> NativeSshSpec {
        NativeSshSpec {
            host: "example.com".to_string(),
            port: 22,
            user: "deploy".to_string(),
            auth_mode: SshAuthMode::Auto,
            identity_files,
            agent_forward: false,
            password: None,
            key_passphrases: None,
            proxy: crate::daemon::protocol::SshProxy::None,
            jump: None,
            forwards: Vec::new(),
            keepalive_interval_s: None,
            keepalive_count_max: None,
            connect_timeout_s: None,
            algorithms: Default::default(),
            x11: false,
            term: "xterm-256color".to_string(),
            verify_host_keys: true,
            skip_banner: false,
            shell_integration: true,
            login_script: Vec::new(),
            display_name: None,
            profile_id: None,
        }
    }

    #[test]
    fn method_order_restricts_by_mode() {
        assert_eq!(
            method_order(SshAuthMode::Password),
            vec![MethodKind::Password]
        );
        assert_eq!(
            method_order(SshAuthMode::KeyboardInteractive),
            vec![MethodKind::KeyboardInteractive]
        );
        assert_eq!(
            method_order(SshAuthMode::Gssapi),
            vec![MethodKind::GssapiWithMic]
        );
        assert_eq!(
            method_order(SshAuthMode::Auto),
            vec![
                MethodKind::GssapiWithMic,
                MethodKind::PublicKey,
                MethodKind::Password,
                MethodKind::KeyboardInteractive
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn gssapi_service_hosts_keep_original_host_before_reverse_dns() {
        let hosts = gssapi_service_hosts_with_lookup("10.37.108.28", |_| {
            Some("n37-108-028.byted.org.".into())
        });
        assert_eq!(
            hosts,
            vec![
                "10.37.108.28".to_string(),
                "n37-108-028.byted.org".to_string()
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn gssapi_service_hosts_dedup_reverse_dns() {
        let hosts = gssapi_service_hosts_with_lookup("example.com", |_| {
            panic!("non-ip hosts should not trigger reverse lookup")
        });
        assert_eq!(hosts, vec!["example.com".to_string()]);

        let hosts = gssapi_service_hosts_with_lookup("10.0.0.1", |_| Some("10.0.0.1".into()));
        assert_eq!(hosts, vec!["10.0.0.1".to_string()]);
    }

    #[test]
    fn rsa_gets_sha256_others_none() {
        assert_eq!(
            rsa_hash_alg(&Algorithm::Rsa { hash: None }),
            Some(HashAlg::Sha256)
        );
        assert_eq!(rsa_hash_alg(&Algorithm::Ed25519), None);
    }
}
