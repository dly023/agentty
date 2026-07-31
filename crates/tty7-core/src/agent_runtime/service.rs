use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};

use crate::core::cli_agent::CLIAgent;
use crate::host::{Host, MTime};

use super::discovery::{AgentSessionKey, AgentSessionRecord, DiscoveryOutcome};
use super::parse::{
    claude_transcript_metadata, codex_index_metadata, codex_transcript_metadata, parse_jsonl_strict,
};
use super::stores::AgentStoreRoots;

pub const DEFAULT_LOGICAL_LIMIT: usize = 40;
pub const DEFAULT_PHYSICAL_SOURCE_LIMIT: usize = 2_000;
pub const DEFAULT_FILE_BYTES: u64 = 4 * 1024 * 1024;
pub const DEFAULT_LINE_LIMIT: usize = 400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryRequest {
    pub roots: AgentStoreRoots,
    pub providers: Vec<String>,
    pub logical_limit: usize,
    pub physical_source_limit: usize,
}

impl DiscoveryRequest {
    pub fn codex_and_claude(roots: AgentStoreRoots) -> Self {
        Self {
            roots,
            providers: vec!["codex".into(), "claude".into()],
            logical_limit: DEFAULT_LOGICAL_LIMIT,
            physical_source_limit: DEFAULT_PHYSICAL_SOURCE_LIMIT,
        }
    }
}

pub fn discover(host: &dyn Host, request: &DiscoveryRequest) -> DiscoveryOutcome {
    let mut all = Vec::new();
    let mut failed = Vec::new();
    let mut missing = Vec::new();
    for provider in &request.providers {
        let result = match provider.as_str() {
            "codex" => discover_codex(host, request),
            "claude" => discover_claude(host, request),
            other => DiscoveryOutcome::Failed {
                message: format!("unknown session provider: {other}"),
            },
        };
        match result {
            DiscoveryOutcome::Complete(mut rows) => all.append(&mut rows),
            DiscoveryOutcome::SourceMissing { source } => missing.push(source),
            DiscoveryOutcome::SourceLimitExceeded { source, limit } => {
                return DiscoveryOutcome::SourceLimitExceeded { source, limit };
            }
            DiscoveryOutcome::Failed { .. }
            | DiscoveryOutcome::Partial { .. }
            | DiscoveryOutcome::Cancelled => failed.push(provider.clone()),
        }
    }
    if !failed.is_empty() {
        return DiscoveryOutcome::Partial {
            failed_providers: failed,
        };
    }
    if !missing.is_empty() {
        return DiscoveryOutcome::SourceMissing {
            source: missing.join(", "),
        };
    }
    DiscoveryOutcome::Complete(canonicalize(all, request.logical_limit))
}

fn discover_codex(host: &dyn Host, request: &DiscoveryRequest) -> DiscoveryOutcome {
    let index = request.roots.codex_index();
    let sessions = request.roots.codex_sessions();
    let mut rows = Vec::new();
    let mut found_source = false;

    if host.exists(&index) {
        found_source = true;
        match host
            .read_file(&index, DEFAULT_FILE_BYTES)
            .and_then(|bytes| {
                parse_jsonl_strict(&bytes, request.physical_source_limit)
                    .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))
            }) {
            Ok(values) => {
                for value in values {
                    if let Some(metadata) = codex_index_metadata(&value) {
                        let id = metadata
                            .session_id
                            .expect("Codex index parser returned identity");
                        rows.push(record(
                            CLIAgent::Codex,
                            "codex",
                            id,
                            metadata.title,
                            metadata.cwd,
                            None,
                            vec![],
                        ));
                    }
                }
            }
            Err(e) if e.to_string().contains("physical line limit") => {
                return DiscoveryOutcome::SourceLimitExceeded {
                    source: index.to_string_lossy().into_owned(),
                    limit: request.physical_source_limit as u64,
                };
            }
            Err(e) => return failed(&index, e),
        }
    }

    match collect_jsonl(host, &sessions, request.physical_source_limit) {
        Ok(files) if !files.is_empty() => {
            found_source = true;
            for file in files {
                let bytes = match host.read_file(&file.path, DEFAULT_FILE_BYTES) {
                    Ok(bytes) => bytes,
                    Err(e) => return failed(&file.path, e),
                };
                let values = match parse_jsonl_strict(&bytes, DEFAULT_LINE_LIMIT) {
                    Ok(values) => values,
                    Err(e) => {
                        return DiscoveryOutcome::Failed {
                            message: format!("{}: {e}", file.path.display()),
                        };
                    }
                };
                let metadata = codex_transcript_metadata(&values);
                if let Some(id) = metadata.session_id {
                    rows.push(record(
                        CLIAgent::Codex,
                        "codex",
                        id,
                        metadata.title,
                        metadata.cwd,
                        file.mtime,
                        vec![],
                    ));
                }
            }
        }
        Ok(_) => {}
        Err(CollectError::Missing) => {}
        Err(CollectError::Limit) => {
            return DiscoveryOutcome::SourceLimitExceeded {
                source: sessions.to_string_lossy().into_owned(),
                limit: request.physical_source_limit as u64,
            };
        }
        Err(CollectError::Io(e)) => return failed(&sessions, e),
    }

    if found_source {
        DiscoveryOutcome::Complete(rows)
    } else {
        DiscoveryOutcome::SourceMissing {
            source: sessions.to_string_lossy().into_owned(),
        }
    }
}

fn discover_claude(host: &dyn Host, request: &DiscoveryRequest) -> DiscoveryOutcome {
    let projects = request.roots.claude_projects();
    let files = match collect_jsonl(host, &projects, request.physical_source_limit) {
        Ok(files) if files.is_empty() => {
            return DiscoveryOutcome::SourceMissing {
                source: projects.to_string_lossy().into_owned(),
            };
        }
        Ok(files) => files,
        Err(CollectError::Missing) => {
            return DiscoveryOutcome::SourceMissing {
                source: projects.to_string_lossy().into_owned(),
            };
        }
        Err(CollectError::Limit) => {
            return DiscoveryOutcome::SourceLimitExceeded {
                source: projects.to_string_lossy().into_owned(),
                limit: request.physical_source_limit as u64,
            };
        }
        Err(CollectError::Io(e)) => return failed(&projects, e),
    };
    let mut rows = Vec::new();
    for file in files {
        let bytes = match host.read_file(&file.path, DEFAULT_FILE_BYTES) {
            Ok(bytes) => bytes,
            Err(e) => return failed(&file.path, e),
        };
        let values = match parse_jsonl_strict(&bytes, DEFAULT_LINE_LIMIT) {
            Ok(values) => values,
            Err(e) => {
                return DiscoveryOutcome::Failed {
                    message: format!("{}: {e}", file.path.display()),
                };
            }
        };
        let metadata = claude_transcript_metadata(&values);
        if let Some(id) = metadata.session_id {
            rows.push(record(
                CLIAgent::Claude,
                "claude",
                id,
                metadata.title,
                metadata.cwd,
                file.mtime,
                vec![],
            ));
        }
    }
    DiscoveryOutcome::Complete(rows)
}

fn record(
    agent: CLIAgent,
    provider: &str,
    session_id: String,
    title: Option<String>,
    cwd: Option<String>,
    mtime: Option<MTime>,
    launch_argv: Vec<String>,
) -> AgentSessionRecord {
    AgentSessionRecord {
        key: AgentSessionKey {
            provider: provider.into(),
            session_id,
        },
        agent,
        title,
        cwd,
        updated_at_unix_ms: mtime.and_then(mtime_ms),
        launch_argv,
    }
}

fn mtime_ms(mtime: MTime) -> Option<u64> {
    if mtime.secs < 0 {
        return None;
    }
    (mtime.secs as u64)
        .checked_mul(1000)?
        .checked_add(u64::from(mtime.nanos / 1_000_000))
}

fn failed(path: &Path, error: io::Error) -> DiscoveryOutcome {
    DiscoveryOutcome::Failed {
        message: format!("{}: {error}", path.display()),
    }
}

struct SourceFile {
    path: PathBuf,
    mtime: Option<MTime>,
}
enum CollectError {
    Missing,
    Limit,
    Io(io::Error),
}

fn collect_jsonl(
    host: &dyn Host,
    root: &Path,
    limit: usize,
) -> Result<Vec<SourceFile>, CollectError> {
    let root_meta = host.stat(root).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            CollectError::Missing
        } else {
            CollectError::Io(e)
        }
    })?;
    if !root_meta.is_dir {
        return Err(CollectError::Io(io::Error::new(
            io::ErrorKind::NotADirectory,
            "provider source is not a directory",
        )));
    }
    let mut dirs = VecDeque::from([root.to_path_buf()]);
    let mut files = Vec::new();
    let mut seen = 0usize;
    while let Some(dir) = dirs.pop_front() {
        let entries = host.read_dir(&dir, None).map_err(CollectError::Io)?;
        for entry in entries {
            seen += 1;
            if seen > limit {
                return Err(CollectError::Limit);
            }
            let path = host.join(&dir, &entry.name);
            if entry.is_dir && !entry.is_symlink {
                dirs.push_back(path);
            } else if !entry.is_dir && entry.name.ends_with(".jsonl") {
                let mtime = host.stat(&path).ok().and_then(|meta| meta.mtime);
                files.push(SourceFile { path, mtime });
            }
        }
    }
    files.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
    Ok(files)
}

fn canonicalize(rows: Vec<AgentSessionRecord>, logical_limit: usize) -> Vec<AgentSessionRecord> {
    let mut unique = HashMap::new();
    for row in rows {
        let replace = unique.get(&row.key).is_none_or(|old: &AgentSessionRecord| {
            row.updated_at_unix_ms >= old.updated_at_unix_ms
        });
        if replace {
            unique.insert(row.key.clone(), row);
        }
    }
    let mut rows: Vec<_> = unique.into_values().collect();
    rows.sort_by(|a, b| {
        b.updated_at_unix_ms
            .cmp(&a.updated_at_unix_ms)
            .then_with(|| a.key.provider.cmp(&b.key.provider))
            .then_with(|| a.key.session_id.cmp(&b.key.session_id))
    });
    rows.truncate(logical_limit);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::local::LocalHost;
    use std::fs;

    fn roots(path: &Path) -> AgentStoreRoots {
        AgentStoreRoots::for_home(path.to_path_buf())
    }

    #[test]
    fn codex_and_claude_discovery_uses_one_shared_host_core() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        fs::create_dir_all(roots.codex_sessions().join("2026/07/31")).unwrap();
        fs::write(roots.codex_sessions().join("2026/07/31/rollout.jsonl"), r#"{"type":"session_meta","payload":{"id":"019fa76a-6276-7b03-b302-c640686b2033","cwd":"/codex"}}
{"type":"event_msg","payload":{"type":"user_message","message":"Fix the tests"}}
"#).unwrap();
        fs::create_dir_all(roots.claude_projects().join("repo")).unwrap();
        fs::write(roots.claude_projects().join("repo/session.jsonl"), r#"{"type":"user","sessionId":"claude-1","cwd":"/claude","message":{"content":"Review code"}}
"#).unwrap();
        let outcome = discover(
            &*LocalHost::new(),
            &DiscoveryRequest::codex_and_claude(roots),
        );
        let DiscoveryOutcome::Complete(rows) = outcome else {
            panic!("expected complete")
        };
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(
            |row| row.key.provider == "codex" && row.title.as_deref() == Some("Fix the tests")
        ));
        assert!(
            rows.iter()
                .any(|row| row.key.provider == "claude"
                    && row.title.as_deref() == Some("Review code"))
        );
    }

    #[test]
    fn malformed_provider_source_fails_closed_without_publishing_other_rows() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        fs::create_dir_all(roots.codex_sessions()).unwrap();
        fs::write(roots.codex_sessions().join("bad.jsonl"), "not-json\n").unwrap();
        fs::create_dir_all(roots.claude_projects()).unwrap();
        let outcome = discover(
            &*LocalHost::new(),
            &DiscoveryRequest {
                providers: vec!["codex".into()],
                ..DiscoveryRequest::codex_and_claude(roots)
            },
        );
        assert!(matches!(outcome, DiscoveryOutcome::Partial { .. }));
    }

    #[test]
    fn physical_source_limit_is_explicit_not_enumeration_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        fs::create_dir_all(roots.codex_sessions()).unwrap();
        fs::write(roots.codex_sessions().join("one.jsonl"), "{}\n").unwrap();
        fs::write(roots.codex_sessions().join("two.jsonl"), "{}\n").unwrap();
        let outcome = discover(
            &*LocalHost::new(),
            &DiscoveryRequest {
                providers: vec!["codex".into()],
                physical_source_limit: 1,
                ..DiscoveryRequest::codex_and_claude(roots)
            },
        );
        assert!(matches!(
            outcome,
            DiscoveryOutcome::SourceLimitExceeded { limit: 1, .. }
        ));
    }
}
