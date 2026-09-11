//! Target-owned, read-only native agent history discovery.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant, UNIX_EPOCH},
};

const HEAD_BYTES: usize = 256 * 1024;
const HEAD_LINES: usize = 400;
pub mod live_titles;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Codex,
    Claude,
}

/// A retained identity, not a trusted source path or a running-process claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIdentity {
    pub provider: Provider,
    pub id: String,
    pub cwd: String,
}

impl SessionIdentity {
    pub fn canonical_id(&self) -> io::Result<String> {
        if self.cwd.is_empty() || self.cwd.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid session directory",
            ));
        }
        uuid::Uuid::parse_str(&self.id)
            .map(|id| id.to_string())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid session UUID"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySession {
    pub provider: Provider,
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub source_path: String,
    pub modified_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySnapshot {
    pub sessions: Vec<HistorySession>,
    pub missing: Vec<Provider>,
    /// The recent candidate window or returned row limit was reached.
    pub limited: bool,
}

#[derive(Debug)]
pub struct StoreRoots {
    pub codex: PathBuf,
    pub claude: PathBuf,
}

impl StoreRoots {
    pub fn from_environment(
        home: &Path,
        mut get: impl FnMut(&str) -> Option<String>,
    ) -> io::Result<Self> {
        if !home.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "target home is not absolute",
            ));
        }
        let resolve = |value: Option<String>, default: &str| {
            let value = value.filter(|v| !v.trim().is_empty());
            match value.as_deref() {
                None => home.join(default),
                Some("~") => home.to_path_buf(),
                Some(v) if v.starts_with("~/") => home.join(&v[2..]),
                Some(v) => {
                    let p = PathBuf::from(v);
                    if p.is_absolute() { p } else { home.join(p) }
                }
            }
        };
        Ok(Self {
            codex: resolve(get("CODEX_HOME"), ".codex"),
            claude: resolve(get("CLAUDE_CONFIG_DIR"), ".claude"),
        })
    }
}

#[derive(Clone, Copy)]
pub struct ScanLimits {
    pub max_entries: usize,
    pub timeout: Duration,
}
impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            timeout: Duration::from_secs(10),
        }
    }
}

fn current_roots() -> io::Result<StoreRoots> {
    let home = std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|s| !s.is_empty()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "target home unavailable"))?;
    StoreRoots::from_environment(Path::new(&home), |key| std::env::var(key).ok())
}

pub fn scan_current_process() -> io::Result<HistorySnapshot> {
    scan(&current_roots()?, ScanLimits::default())
}

pub fn validate_current_process(session: &HistorySession) -> io::Result<()> {
    validate(&current_roots()?, session)
}

pub fn resolve_current_process(identity: &SessionIdentity) -> io::Result<HistorySession> {
    resolve(&current_roots()?, identity, ScanLimits::default())
}

pub fn resolve(
    roots: &StoreRoots,
    identity: &SessionIdentity,
    limits: ScanLimits,
) -> io::Result<HistorySession> {
    let id = identity.canonical_id()?;
    if !Path::new(&identity.cwd).is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session directory is not absolute",
        ));
    }
    let start = Instant::now();
    let root = match identity.provider {
        Provider::Codex => roots.codex.join("sessions"),
        Provider::Claude => roots.claude.join("projects"),
    };
    let mut entries = 0;
    let files = collect_candidates(identity.provider, root, start, limits, &mut entries)?;
    let mut found = None;
    for file in files {
        check_deadline(start, limits)?;
        let Some(meta) = read_metadata(file.provider, &file.path)? else {
            continue;
        };
        if meta.id != id {
            continue;
        }
        if found.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ambiguous session identity",
            ));
        }
        found = Some(HistorySession {
            provider: file.provider,
            title: meta.title.unwrap_or_else(|| id.clone()),
            id: meta.id,
            cwd: meta.cwd,
            source_path: file.path.to_string_lossy().into_owned(),
            modified_ms: file.modified_ms,
        });
    }
    check_deadline(start, limits)?;
    let found = found.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "retained session not found on target",
        )
    })?;
    if found.cwd != identity.cwd {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained session directory changed",
        ));
    }
    validate(roots, &found)?;
    check_deadline(start, limits)?;
    Ok(found)
}

/// Recheck exactly the selected target record, without rediscovering history.
pub fn validate(roots: &StoreRoots, session: &HistorySession) -> io::Result<()> {
    let root = match session.provider {
        Provider::Codex => roots.codex.join("sessions"),
        Provider::Claude => roots.claude.join("projects"),
    }
    .canonicalize()?;
    let source = Path::new(&session.source_path).canonicalize()?;
    let relative = source.strip_prefix(&root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "history source is outside target provider store",
        )
    })?;
    if source.extension().is_none_or(|e| e != "jsonl")
        || relative.components().any(|c| c.as_os_str() == "subagents")
        || (session.provider == Provider::Claude
            && source
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("agent-")))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported history source",
        ));
    }
    let metadata = read_metadata(session.provider, &source)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "history source is no longer resumable",
        )
    })?;
    if metadata.id != session.id || metadata.cwd != session.cwd {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "history session identity changed",
        ));
    }
    if !fs::metadata(&session.cwd)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "history working directory is not a directory",
        ));
    }
    Ok(())
}

fn read_metadata(provider: Provider, path: &Path) -> io::Result<Option<Metadata>> {
    let handle = open_history_file(path)?;
    let mut bytes = Vec::new();
    handle
        .take((HEAD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > HEAD_BYTES;
    bytes.truncate(HEAD_BYTES);
    parse_head(provider, &bytes, truncated)
}

fn open_history_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let handle = options.open(path)?;
    if !handle.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "history source changed file type",
        ));
    }
    Ok(handle)
}

fn read_codex_titles(
    roots: &StoreRoots,
    start: Instant,
    limits: ScanLimits,
) -> io::Result<HashMap<String, String>> {
    const INDEX_BYTES: u64 = 32 * 1024 * 1024;
    let file = match open_history_file(&roots.codex.join("session_index.jsonl")) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(INDEX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > INDEX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Codex title index byte limit exceeded",
        ));
    }
    let mut titles = HashMap::new();
    let mut records = 0;
    for line in bytes
        .split(|b| *b == b'\n')
        .filter(|line| line.iter().any(|b| !b.is_ascii_whitespace()))
    {
        check_deadline(start, limits)?;
        records += 1;
        if records > 10_000 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Codex title index record limit exceeded",
            ));
        }
        let value: Value = serde_json::from_slice(line)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let Some(id) = str_at(&value, "id").and_then(|id| uuid::Uuid::parse_str(id).ok()) else {
            continue;
        };
        if let Some(title) = str_at(&value, "thread_name").and_then(title_text) {
            // Codex appends renames. Wall-clock timestamps can move backwards.
            titles.insert(id.to_string(), title);
        }
    }
    Ok(titles)
}

struct Candidate {
    provider: Provider,
    path: PathBuf,
    modified_ms: u64,
}

fn check_deadline(start: Instant, limits: ScanLimits) -> io::Result<()> {
    if start.elapsed() >= limits.timeout {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "agent history scan timed out",
        ))
    } else {
        Ok(())
    }
}

fn collect_candidates(
    provider: Provider,
    root: PathBuf,
    start: Instant,
    limits: ScanLimits,
    entries: &mut usize,
) -> io::Result<Vec<Candidate>> {
    let mut files = Vec::new();
    let mut pending = vec![(root, 0)];
    while let Some((dir, depth)) = pending.pop() {
        check_deadline(start, limits)?;
        if depth > 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "agent history directory depth limit exceeded",
            ));
        }
        for entry in fs::read_dir(&dir)? {
            check_deadline(start, limits)?;
            *entries += 1;
            if *entries > limits.max_entries {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "agent history source entry limit exceeded",
                ));
            }
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if entry.file_name() != "subagents" {
                    pending.push((entry.path(), depth + 1));
                }
            } else if kind.is_file() && entry.path().extension().is_some_and(|e| e == "jsonl") {
                if provider == Provider::Claude
                    && entry.file_name().to_string_lossy().starts_with("agent-")
                {
                    continue;
                }
                let modified_ms = entry
                    .metadata()?
                    .modified()?
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u64::MAX as u128) as u64;
                files.push(Candidate {
                    provider,
                    path: entry.path(),
                    modified_ms,
                });
            }
        }
    }
    check_deadline(start, limits)?;
    Ok(files)
}

pub fn scan(roots: &StoreRoots, limits: ScanLimits) -> io::Result<HistorySnapshot> {
    let start = Instant::now();
    let mut files = Vec::new();
    let mut missing = Vec::new();
    let mut entries = 0;
    for (provider, root) in [
        (Provider::Codex, roots.codex.join("sessions")),
        (Provider::Claude, roots.claude.join("projects")),
    ] {
        check_deadline(start, limits)?;
        match fs::metadata(&root) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                missing.push(provider);
                continue;
            }
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "provider store is not a directory",
                ));
            }
            _ => {}
        }
        files.extend(collect_candidates(
            provider,
            root,
            start,
            limits,
            &mut entries,
        )?);
    }
    if missing.len() == 2 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no Codex or Claude history store on target",
        ));
    }
    files.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms).then(a.path.cmp(&b.path)));
    let mut limited = files.len() > 400;
    files.truncate(400);
    let mut sessions = Vec::new();
    let mut seen = HashSet::new();
    let native_titles = if missing.contains(&Provider::Codex) {
        HashMap::new()
    } else {
        read_codex_titles(roots, start, limits)?
    };
    for file in files {
        check_deadline(start, limits)?;
        let parsed = read_metadata(file.provider, &file.path)?;
        if let Some(meta) = parsed {
            let native = (file.provider == Provider::Codex)
                .then(|| native_titles.get(&meta.id))
                .flatten();
            let Some(title) = native.cloned().or(meta.title) else {
                continue;
            };
            if seen.insert((file.provider, meta.id.clone())) {
                sessions.push(HistorySession {
                    provider: file.provider,
                    id: meta.id,
                    title,
                    cwd: meta.cwd,
                    source_path: file.path.to_string_lossy().into_owned(),
                    modified_ms: file.modified_ms,
                });
            }
        }
    }
    check_deadline(start, limits)?;
    limited |= sessions.len() > 100;
    sessions.truncate(100);
    Ok(HistorySnapshot {
        sessions,
        missing,
        limited,
    })
}

#[derive(Debug)]
struct Metadata {
    id: String,
    title: Option<String>,
    cwd: String,
}

fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str().filter(|v| !v.trim().is_empty())
}

fn title_text(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty()
        || [
            "# AGENTS.md instructions",
            "<INSTRUCTIONS>",
            "<environment_context>",
            "<recommended_plugins>",
            "<codex_internal_context",
            "<system-reminder>",
        ]
        .iter()
        .any(|prefix| text.starts_with(prefix))
        || text.contains("\n<INSTRUCTIONS>")
    {
        return None;
    }
    let line = text.lines().find(|l| !l.trim().is_empty())?;
    Some(line.chars().filter(|c| !c.is_control()).take(120).collect())
}

fn content_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return title_text(text);
    }
    content
        .as_array()?
        .iter()
        .filter(|p| matches!(str_at(p, "type"), Some("text" | "input_text")))
        .find_map(|p| str_at(p, "text").and_then(title_text))
}

fn parse_head(provider: Provider, bytes: &[u8], truncated: bool) -> io::Result<Option<Metadata>> {
    let bytes = if truncated {
        let end = bytes.iter().rposition(|b| *b == b'\n').ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "history header exceeds byte limit",
            )
        })?;
        &bytes[..=end]
    } else {
        bytes
    };
    let text = std::str::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "history header is not UTF-8"))?;
    let (mut id, mut cwd, mut title, mut first_user) = (None, None, None, None);
    let mut internal = false;
    for (line_no, line) in text.lines().take(HEAD_LINES).enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid history JSONL at line {}", line_no + 1),
            )
        })?;
        if !value.is_object() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "history record is not an object",
            ));
        }
        let payload = value.get("payload").unwrap_or(&value);
        match provider {
            Provider::Codex => {
                if str_at(&value, "type") == Some("session_meta") {
                    id = str_at(payload, "id").map(str::to_owned);
                }
                for origin in [&value, payload] {
                    internal |= str_at(origin, "thread_source")
                        .is_some_and(|s| matches!(s, "internal" | "subagent"))
                        || str_at(origin, "parent_thread_id").is_some()
                        || origin.pointer("/source/subagent").is_some();
                }
                if first_user.is_none() {
                    first_user = match (str_at(&value, "type"), str_at(payload, "type")) {
                        (Some("event_msg"), Some("user_message")) => {
                            str_at(payload, "message").and_then(title_text)
                        }
                        (Some("response_item"), Some("message"))
                            if str_at(payload, "role") == Some("user") =>
                        {
                            payload.get("content").and_then(content_text)
                        }
                        _ => None,
                    };
                }
            }
            Provider::Claude => {
                if id.is_none() {
                    id = str_at(&value, "sessionId").map(str::to_owned);
                }
                internal |= value.get("isSidechain").and_then(Value::as_bool) == Some(true);
                if first_user.is_none() && str_at(&value, "type") == Some("user") {
                    first_user = value
                        .pointer("/message/content")
                        .or_else(|| value.get("content"))
                        .and_then(content_text);
                }
            }
        }
        if cwd.is_none() {
            cwd = str_at(&value, "cwd")
                .or_else(|| str_at(payload, "cwd"))
                .map(str::to_owned);
        }
        if title.is_none() {
            title = [
                str_at(&value, "thread_name"),
                str_at(&value, "customTitle"),
                str_at(&value, "aiTitle"),
                str_at(&value, "title"),
                str_at(payload, "title"),
            ]
            .into_iter()
            .flatten()
            .find_map(title_text);
        }
    }
    if internal {
        return Ok(None);
    }
    let Some(id) = id
        .and_then(|id| uuid::Uuid::parse_str(&id).ok())
        .map(|id| id.to_string())
    else {
        return Ok(None);
    };
    let Some(cwd) = cwd.filter(|cwd| Path::new(cwd).is_absolute() && !cwd.contains('\0')) else {
        return Ok(None);
    };
    let title = title.or(first_user);
    Ok(Some(Metadata { id, title, cwd }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};
    const ID: &str = "01900000-0000-7000-8000-000000000001";

    #[test]
    fn retained_session_resolution_is_not_a_recent_history_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let roots = roots(dir.path());
        let cwd = dir.path().to_string_lossy().into_owned();
        fs::create_dir_all(roots.codex.join("sessions")).unwrap();
        // More than both discovery quotas, with the retained session oldest.
        for n in 1..=402u128 {
            let id = uuid::Uuid::from_u128(n).to_string();
            let path = roots.codex.join("sessions").join(format!("{n:04}.jsonl"));
            fs::write(&path, format!("{}\n", serde_json::json!({"type":"session_meta", "payload": {"id":id,"cwd":cwd,"source":"cli", "title":"fixture"}}))).unwrap();
            let file = fs::OpenOptions::new().write(true).open(path).unwrap();
            file.set_times(
                fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(n as u64)),
            )
            .unwrap();
        }
        let id = uuid::Uuid::from_u128(1).to_string();
        let query = SessionIdentity {
            provider: Provider::Codex,
            id: id.clone(),
            cwd: cwd.clone(),
        };
        let recent = scan(&roots, ScanLimits::default()).unwrap();
        assert!(recent.limited);
        assert_eq!(recent.sessions.len(), 100);
        assert!(!recent.sessions.iter().any(|session| session.id == id));
        // A broken naming source must not prevent recovering a known session.
        fs::write(
            roots.codex.join("session_index.jsonl"),
            "broken title index",
        )
        .unwrap();
        let found = resolve(&roots, &query, ScanLimits::default()).unwrap();
        assert_eq!(found.id, id);
        assert_eq!(found.cwd, cwd);
        assert_eq!(found.title, "fixture");
        assert!(found.source_path.ends_with("0001.jsonl"));
        validate(&roots, &found).unwrap();
        let alias = SessionIdentity {
            id: format!("{{{id}}}"),
            ..query
        };
        assert_eq!(
            resolve(&roots, &alias, ScanLimits::default()).unwrap().id,
            id
        );
    }

    #[test]
    fn retained_session_resolution_rejects_ambiguous_invalid_and_incomplete_sources() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), Provider::Claude, false);
        let roots = roots(dir.path());
        let mut query = SessionIdentity {
            provider: Provider::Claude,
            id: ID.into(),
            cwd: test_cwd(),
        };
        let found = resolve(&roots, &query, ScanLimits::default()).unwrap();
        query.cwd = dir.path().to_string_lossy().into_owned();
        assert!(
            resolve(&roots, &query, ScanLimits::default()).is_err(),
            "cwd replacement"
        );
        query.cwd = test_cwd();
        assert!(
            resolve(
                &roots,
                &query,
                ScanLimits {
                    max_entries: 0,
                    ..ScanLimits::default()
                }
            )
            .is_err()
        );
        assert_eq!(
            resolve(
                &roots,
                &query,
                ScanLimits {
                    timeout: Duration::ZERO,
                    ..ScanLimits::default()
                }
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::TimedOut
        );
        fs::copy(
            &found.source_path,
            Path::new(&found.source_path).with_file_name("duplicate.jsonl"),
        )
        .unwrap();
        assert!(
            resolve(&roots, &query, ScanLimits::default()).is_err(),
            "ambiguous sources"
        );
        let duplicate = Path::new(&found.source_path).with_file_name("duplicate.jsonl");
        let changed = fs::read_to_string(&found.source_path).unwrap().replace(
            &serde_json::to_string(&query.cwd).unwrap(),
            &serde_json::to_string(&dir.path().to_string_lossy()).unwrap(),
        );
        fs::write(&duplicate, changed).unwrap();
        assert!(
            resolve(&roots, &query, ScanLimits::default()).is_err(),
            "different cwd is still duplicate identity"
        );
        fs::write(&duplicate, "malformed header\n").unwrap();
        assert_eq!(
            resolve(&roots, &query, ScanLimits::default())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_file(&duplicate).unwrap();
        query.id = "01900000-0000-7000-8000-000000000002".into();
        assert_eq!(
            resolve(&roots, &query, ScanLimits::default())
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        query.id = "not a UUID".into();
        assert_eq!(
            resolve(&roots, &query, ScanLimits::default())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(
            serde_json::from_value::<SessionIdentity>(serde_json::json!({
                "provider":"claude", "id":ID, "cwd":test_cwd(), "roots":"/client-chosen"
            }))
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn retained_session_resolution_ignores_internal_and_symlink_sources() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fixture(dir.path(), Provider::Codex, true);
        fixture(outside.path(), Provider::Codex, false);
        let roots = roots(dir.path());
        std::os::unix::fs::symlink(
            outside.path().join("codex/sessions"),
            roots.codex.join("sessions/linked"),
        )
        .unwrap();
        let query = SessionIdentity {
            provider: Provider::Codex,
            id: ID.into(),
            cwd: test_cwd(),
        };
        assert_eq!(
            resolve(&roots, &query, ScanLimits::default())
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        // A native header without any title remains recoverable.
        let path = roots.codex.join("sessions/header-only.jsonl");
        fs::write(&path, format!("{}\n", serde_json::json!({"type":"session_meta", "payload":{"id":ID,"cwd":test_cwd(),"source":"cli"}}))).unwrap();
        let found = resolve(&roots, &query, ScanLimits::default()).unwrap();
        assert_eq!(found.title, ID);
        assert_eq!(Path::new(&found.source_path), path);
    }

    #[test]
    fn codex_native_title_overrides_excerpt_in_append_order() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), Provider::Codex, false);
        fs::write(dir.path().join("codex/session_index.jsonl"), format!(
            "{{\"id\":\"{ID}\",\"thread_name\":\"old\",\"updated_at\":\"2026-09-09\"}}\n{{\"id\":\"{ID}\",\"thread_name\":\"官方名称\",\"updated_at\":\"2020-01-01\"}}\n{{\"id\":\"{ID}\",\"thread_name\":\" \"}}\n"
        )).unwrap();
        let result = scan(&roots(dir.path()), ScanLimits::default()).unwrap();
        assert_eq!(result.sessions.len(), 1);
        assert_eq!(result.sessions[0].title, "官方名称");
    }

    #[test]
    fn codex_native_title_requires_rollout_and_accepts_header_only() {
        let dir = tempfile::tempdir().unwrap();
        let roots = roots(dir.path());
        fs::create_dir_all(roots.codex.join("sessions")).unwrap();
        fs::write(
            roots.codex.join("session_index.jsonl"),
            format!("{{\"id\":\"{ID}\",\"thread_name\":\"native\"}}\n"),
        )
        .unwrap();
        assert!(
            scan(&roots, ScanLimits::default())
                .unwrap()
                .sessions
                .is_empty()
        );
        fs::write(
            roots.codex.join("sessions/header.jsonl"),
            serde_json::json!({
                "type":"session_meta", "payload":{"id":ID,"cwd":dir.path()}
            })
            .to_string(),
        )
        .unwrap();
        let result = scan(&roots, ScanLimits::default()).unwrap();
        assert_eq!(result.sessions.len(), 1);
        assert_eq!(result.sessions[0].title, "native");
        validate(&roots, &result.sessions[0]).unwrap();
    }

    #[test]
    fn codex_native_title_bad_index_fails_generation() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), Provider::Codex, false);
        fs::write(dir.path().join("codex/session_index.jsonl"), "{\"id\":").unwrap();
        assert_eq!(
            scan(&roots(dir.path()), ScanLimits::default())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn codex_native_title_record_cap_never_publishes_partial_names() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), Provider::Codex, false);
        let record = format!("{{\"id\":\"{ID}\",\"thread_name\":\"native\"}}\n");
        fs::write(
            dir.path().join("codex/session_index.jsonl"),
            record.repeat(10_001),
        )
        .unwrap();
        assert_eq!(
            scan(&roots(dir.path()), ScanLimits::default())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[cfg(unix)]
    #[test]
    fn codex_native_title_does_not_follow_index_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fixture(dir.path(), Provider::Codex, false);
        let source = outside.path().join("index.jsonl");
        fs::write(
            &source,
            format!("{{\"id\":\"{ID}\",\"thread_name\":\"outside\"}}\n"),
        )
        .unwrap();
        std::os::unix::fs::symlink(&source, dir.path().join("codex/session_index.jsonl")).unwrap();
        assert!(scan(&roots(dir.path()), ScanLimits::default()).is_err());
    }

    fn test_cwd() -> String {
        std::env::temp_dir()
            .join("agentty-history-project")
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn history_resume_validation_rejects_changed_identity_and_foreign_source() {
        let target = tempfile::tempdir().unwrap();
        fixture(target.path(), Provider::Codex, false);
        // The cwd must be present on the target before any pane is created.
        fs::create_dir_all(test_cwd()).unwrap();
        let roots = roots(target.path());
        let snapshot = scan(&roots, ScanLimits::default()).unwrap();
        let row = &snapshot.sessions[0];
        validate(&roots, row).unwrap();
        let mut changed = row.clone();
        changed.id = "01900000-0000-7000-8000-000000000099".into();
        assert!(validate(&roots, &changed).is_err());
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("foreign.jsonl"), "{}").unwrap();
        changed.source_path = outside
            .path()
            .join("foreign.jsonl")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            validate(&roots, &changed).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn agent_sessions_roots_use_target_environment_overrides() {
        let home = tempfile::tempdir().unwrap();
        let roots = StoreRoots::from_environment(home.path(), |key| match key {
            "CODEX_HOME" => Some("custom-codex".into()),
            "CLAUDE_CONFIG_DIR" => Some("~/custom-claude".into()),
            _ => panic!("only provider root keys may be read"),
        })
        .unwrap();
        assert_eq!(roots.codex, home.path().join("custom-codex"));
        assert_eq!(roots.claude, home.path().join("custom-claude"));
    }

    #[cfg(unix)]
    #[test]
    fn agent_sessions_do_not_follow_nested_symlinks() {
        let target = tempfile::tempdir().unwrap();
        let unrelated = tempfile::tempdir().unwrap();
        fixture(unrelated.path(), Provider::Codex, false);
        fs::create_dir_all(target.path().join("codex/sessions")).unwrap();
        std::os::unix::fs::symlink(
            unrelated.path().join("codex/sessions"),
            target.path().join("codex/sessions/foreign"),
        )
        .unwrap();
        assert!(
            scan(&roots(target.path()), ScanLimits::default())
                .unwrap()
                .sessions
                .is_empty()
        );
    }

    fn fixture(root: &Path, provider: Provider, internal: bool) {
        let (path, lines) = match provider {
            Provider::Codex => (
                root.join("codex/sessions/2026/09/rollout.jsonl"),
                format!(
                    "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ID}\",\"cwd\":\"/project\",\"source\":{}}}}}\n{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"修复登录问题\"}}}}\n",
                    if internal {
                        "{\"subagent\":{\"thread_spawn\":{}}}"
                    } else {
                        "\"cli\""
                    }
                ),
            ),
            Provider::Claude => (
                root.join("claude/projects/project/session.jsonl"),
                format!(
                    "{{\"type\":\"user\",\"sessionId\":\"{ID}\",\"cwd\":\"/project\",\"isSidechain\":{internal},\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"解释这个仓库\"}}]}}}}\n"
                ),
            ),
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let lines = lines.replace("\"/project\"", &serde_json::to_string(&test_cwd()).unwrap());
        fs::write(path, lines).unwrap();
    }

    fn roots(root: &Path) -> StoreRoots {
        StoreRoots {
            codex: root.join("codex"),
            claude: root.join("claude"),
        }
    }

    #[test]
    fn agent_sessions_respect_explicit_target_roots() {
        let target = tempfile::tempdir().unwrap();
        let unrelated = tempfile::tempdir().unwrap();
        fixture(target.path(), Provider::Codex, false);
        fixture(target.path(), Provider::Claude, false);
        fixture(unrelated.path(), Provider::Codex, true);
        let result = scan(&roots(target.path()), ScanLimits::default()).unwrap();
        assert_eq!(result.sessions.len(), 2);
        assert!(result.missing.is_empty());
        assert!(result.sessions.iter().all(|r| r.id == ID
            && r.cwd == test_cwd()
            && Path::new(&r.source_path).starts_with(target.path())));
        assert_ne!(result.sessions[0].provider, result.sessions[1].provider);
    }

    #[test]
    fn agent_sessions_malformed_source_fails_whole_scan() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), Provider::Codex, false);
        fixture(dir.path(), Provider::Claude, false);
        fs::write(
            dir.path().join("claude/projects/project/broken.jsonl"),
            "not json\n",
        )
        .unwrap();
        assert_eq!(
            scan(&roots(dir.path()), ScanLimits::default())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn agent_sessions_filter_internal_and_injected_records() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), Provider::Codex, true);
        fixture(dir.path(), Provider::Claude, true);
        assert!(
            scan(&roots(dir.path()), ScanLimits::default())
                .unwrap()
                .sessions
                .is_empty()
        );
        let injected = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ID}\",\"cwd\":\"/project\"}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"# AGENTS.md instructions for project\"}}]}}}}\n"
        );
        assert!(
            parse_head(Provider::Codex, injected.as_bytes(), false)
                .unwrap()
                .unwrap()
                .title
                .is_none()
        );
    }

    #[test]
    fn agent_sessions_missing_and_limits_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            scan(&roots(dir.path()), ScanLimits::default())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        fixture(dir.path(), Provider::Codex, false);
        let result = scan(&roots(dir.path()), ScanLimits::default()).unwrap();
        assert_eq!(result.missing, vec![Provider::Claude]);
        let limits = ScanLimits {
            max_entries: 0,
            ..ScanLimits::default()
        };
        assert!(scan(&roots(dir.path()), limits).is_err());
        let limits = ScanLimits {
            timeout: std::time::Duration::ZERO,
            ..ScanLimits::default()
        };
        assert_eq!(
            scan(&roots(dir.path()), limits).unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn agent_sessions_bounded_head_ignores_only_truncated_tail() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), Provider::Claude, false);
        let bytes = fs::read(dir.path().join("claude/projects/project/session.jsonl")).unwrap();
        let mut partial = bytes.clone();
        partial.extend_from_slice(b"{\"truncated\":");
        assert!(
            parse_head(Provider::Claude, &partial, true)
                .unwrap()
                .is_some()
        );
        assert!(parse_head(Provider::Claude, &partial, false).is_err());
    }
}
