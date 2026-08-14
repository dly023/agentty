use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};

use crate::core::cli_agent::CLIAgent;
use crate::host::{Host, MTime};

use super::discovery::{AgentSessionKey, AgentSessionRecord, DiscoveryOutcome};
use super::parse::{
    claude_transcript_metadata, codex_index_metadata, codex_transcript_metadata,
    gemini_first_user_excerpt, gemini_header_metadata, gemini_updated_at_unix_ms,
    grok_summary_metadata, grok_summary_updated_at_unix_ms, omp_transcript_metadata,
    first_user_title_candidate, parse_jsonl_strict,
};
use super::provider::{
    PERSISTED_PROVIDER_DESCRIPTORS, ProviderDescriptor, ProviderId, ProviderScanner,
    descriptor_for_id,
};
use super::stores::AgentStoreRoots;

pub const DEFAULT_LOGICAL_LIMIT: usize = 40;
pub const DEFAULT_PHYSICAL_SOURCE_LIMIT: usize = 2_000;
/// Transcripts are read as a bounded head prefix: session metadata lives in
/// the first lines, and a multi-GB rollout must never fail the whole scan.
pub const DEFAULT_HEAD_BYTES: u64 = 256 * 1024;
pub const DEFAULT_LINE_LIMIT: usize = 400;
/// Only the most recently touched transcripts are parsed; canonicalize would
/// truncate to `logical_limit` rows sorted by mtime anyway.
pub const PARSE_CANDIDATE_FACTOR: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryRequest {
    pub roots: AgentStoreRoots,
    pub providers: Vec<ProviderId>,
    pub logical_limit: usize,
    pub physical_source_limit: usize,
}

impl DiscoveryRequest {
    pub fn standard(roots: AgentStoreRoots) -> Self {
        Self {
            roots,
            providers: PERSISTED_PROVIDER_DESCRIPTORS
                .iter()
                .map(|descriptor| descriptor.id)
                .collect(),
            logical_limit: DEFAULT_LOGICAL_LIMIT,
            physical_source_limit: DEFAULT_PHYSICAL_SOURCE_LIMIT,
        }
    }

    pub fn codex_and_claude(roots: AgentStoreRoots) -> Self {
        Self {
            roots,
            providers: vec![ProviderId::Codex, ProviderId::Claude],
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
        let descriptor = descriptor_for_id(*provider);
        let result = discover_provider(host, request, descriptor);
        match result {
            DiscoveryOutcome::Complete(mut rows) => all.append(&mut rows),
            DiscoveryOutcome::SourceMissing { source } => missing.push(source),
            DiscoveryOutcome::SourceLimitExceeded { source, limit } => {
                return DiscoveryOutcome::SourceLimitExceeded { source, limit };
            }
            DiscoveryOutcome::Failed { .. }
            | DiscoveryOutcome::Partial { .. }
            | DiscoveryOutcome::Cancelled => failed.push(provider.slug().to_string()),
        }
    }
    if !failed.is_empty() {
        return DiscoveryOutcome::Partial {
            failed_providers: failed,
        };
    }
    if all.is_empty() && !missing.is_empty() {
        return DiscoveryOutcome::SourceMissing {
            source: missing.join(", "),
        };
    }
    DiscoveryOutcome::Complete(canonicalize(all, request.logical_limit))
}

fn discover_provider(
    host: &dyn Host,
    request: &DiscoveryRequest,
    descriptor: &ProviderDescriptor,
) -> DiscoveryOutcome {
    match descriptor.scanner {
        ProviderScanner::CodexJsonlAndIndex => discover_codex(host, request),
        ProviderScanner::ClaudeJsonl => discover_claude(host, request),
        ProviderScanner::OmpJsonl => discover_omp(host, request),
        ProviderScanner::GrokSummaryJson => discover_grok(host, request),
        ProviderScanner::GeminiTmpJsonl => discover_gemini(host, request),
        ProviderScanner::JcodeHarnessApi => discover_jcode(host, request),
        ProviderScanner::OpenCodeLegacyJson => discover_opencode(host, request, descriptor),
        ProviderScanner::DroidJsonl
        | ProviderScanner::CopilotJsonl
        | ProviderScanner::PiJsonl
        | ProviderScanner::CursorJsonl
        | ProviderScanner::AntigravityJsonl => discover_generic_jsonl(host, request, descriptor),
    }
}

#[cfg(unix)]
fn discover_jcode(host: &dyn Host, request: &DiscoveryRequest) -> DiscoveryOutcome {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let file_result = discover_jcode_session_files(host, request);
    if matches!(&file_result, DiscoveryOutcome::Complete(rows) if !rows.is_empty()) {
        return file_result;
    }
    if !host.id().is_local() {
        return file_result;
    }
    let socket = request.roots.jcode_api_socket();
    let Ok(mut stream) = UnixStream::connect(&socket) else {
        return discover_jcode_session_files(host, request);
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
    let hello = serde_json::json!({
        "v": 1,
        "id": 1,
        "req": "hello",
        "min_version": 1,
        "max_version": 1,
        "client": "agentty/agent-runtime"
    });
    let list = serde_json::json!({"v": 1, "id": 2, "req": "list_sessions"});
    if writeln!(stream, "{hello}").is_err() || writeln!(stream, "{list}").is_err() {
        return DiscoveryOutcome::Failed { message: "jcode API write failed".into() };
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let _ = reader.read_line(&mut line);
    line.clear();
    if reader.read_line(&mut line).is_err() {
        return DiscoveryOutcome::Failed { message: "jcode API read failed".into() };
    }
    let Ok(frame) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return DiscoveryOutcome::Failed { message: "jcode API returned invalid JSON".into() };
    };
    let Some(sessions) = frame.get("sessions").and_then(|v| v.as_array()) else {
        return DiscoveryOutcome::Failed { message: "jcode API did not return sessions".into() };
    };
    let rows = sessions.iter().filter_map(|session| {
        let id = session.get("session_id")?.as_str()?.to_owned();
        if session
            .get("parent_id")
            .and_then(serde_json::Value::as_str)
            .is_some()
            || session
                .get("is_debug")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        {
            return None;
        }
        Some(record(
            CLIAgent::Jcode,
            "jcode",
            id,
            session.get("title").and_then(|v| v.as_str()).map(str::to_owned),
            session.get("working_dir").and_then(|v| v.as_str()).map(str::to_owned),
            None,
            vec![],
            Some(socket.to_string_lossy().into_owned()),
            None,
        ))
    }).collect();
    DiscoveryOutcome::Complete(rows)
}

fn discover_jcode_session_files(host: &dyn Host, request: &DiscoveryRequest) -> DiscoveryOutcome {
    #[derive(serde::Deserialize)]
    struct JcodeSessionHeader {
        id: String,
        #[serde(default)]
        parent_id: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        custom_title: Option<String>,
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        is_debug: bool,
        #[serde(default)]
        messages: Vec<serde_json::Value>,
    }

    let root = request.roots.home.join(".jcode/sessions");
    let entries = match host.read_dir(&root, Some(&root)) {
        Ok(entries) => entries,
        Err(_) => {
            return DiscoveryOutcome::SourceMissing {
                source: root.to_string_lossy().into_owned(),
            };
        }
    };
    let mut rows = Vec::new();
    for entry in entries.into_iter().filter(|entry| {
        !entry.is_dir
            && !entry.is_symlink
            && entry.name.ends_with(".json")
            && !entry.name.ends_with(".journal.json")
    }) {
        let path = host.join(&root, &entry.name);
        let Ok(bytes) = host.read_file_prefix(&path, DEFAULT_HEAD_BYTES) else {
            continue;
        };
        let Ok(header) = serde_json::from_slice::<JcodeSessionHeader>(&bytes) else {
            continue;
        };
        let first_user_message = header.messages.iter().find_map(|message| {
            (message.get("role").and_then(serde_json::Value::as_str) == Some("user"))
                .then(|| message.get("content"))
                .flatten()
                .and_then(jcode_message_text)
                .filter(|text| !is_jcode_internal_system_reminder(text))
                .and_then(|text| first_user_title_candidate(&text))
        });
        let has_user_message = first_user_message.is_some();
        if header.parent_id.is_some() || header.is_debug || !has_user_message {
            continue;
        }
        let title = header
            .custom_title
            .filter(|value| !value.trim().is_empty())
            .or(header.title)
            .or(first_user_message)
            .or_else(|| Some(format!("Jcode session {}", header.id)));
        let resume_id = header.id.clone();
        rows.push(record(
            CLIAgent::Jcode,
            "jcode",
            header.id,
            title,
            header.working_dir,
            host.stat(&path).ok().and_then(|meta| meta.mtime),
            vec!["jcode".into(), "--resume".into(), resume_id],
            Some(path.to_string_lossy().into_owned()),
            None,
        ));
    }
    DiscoveryOutcome::Complete(rows)
}

fn jcode_message_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| match part {
                    serde_json::Value::String(text) => Some(text.as_str()),
                    serde_json::Value::Object(object) => object
                        .get("text")
                        .and_then(serde_json::Value::as_str),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            (!text.trim().is_empty()).then_some(text)
        }
        serde_json::Value::Object(object) => object
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

fn is_jcode_internal_system_reminder(text: &str) -> bool {
    text.trim_start().starts_with("<system-reminder>")
}

#[cfg(not(unix))]
fn discover_jcode(_host: &dyn Host, _request: &DiscoveryRequest) -> DiscoveryOutcome {
    DiscoveryOutcome::SourceMissing { source: "jcode harness API requires a Unix socket".into() }
}

fn discover_codex(host: &dyn Host, request: &DiscoveryRequest) -> DiscoveryOutcome {
    let index = request.roots.codex_index();
    let sessions = request.roots.codex_sessions();
    let mut rows = Vec::new();
    let mut found_source = false;

    if host.exists(&index) {
        found_source = true;
        match read_jsonl_head(
            host,
            &index,
            DEFAULT_HEAD_BYTES,
            request.physical_source_limit,
        ) {
            Ok(values) => {
                for value in values {
                    if let Some(metadata) = codex_index_metadata(&value) {
                        let resolved_title = metadata.resolved_title().map(str::to_owned);
                        let id = metadata
                            .session_id
                            .expect("Codex index parser returned identity");
                        rows.push(record(
                            CLIAgent::Codex,
                            "codex",
                            id,
                            resolved_title,
                            metadata.cwd,
                            None,
                            vec![],
                            Some(index.to_string_lossy().into_owned()),
                            metadata.created_at_unix_ms,
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
            let mut files = files;
            files.truncate(parse_candidate_cut(request));
            for file in files {
                let values =
                    match read_jsonl_head(host, &file.path, DEFAULT_HEAD_BYTES, DEFAULT_LINE_LIMIT)
                    {
                        Ok(values) => values,
                        Err(e) => return failed(&file.path, e),
                    };
                let metadata = codex_transcript_metadata(&values);
                let resolved_title = metadata.resolved_title().map(str::to_owned);
                if let Some(id) = metadata.session_id {
                    rows.push(record(
                        CLIAgent::Codex,
                        "codex",
                        id,
                        resolved_title,
                        metadata.cwd,
                        file.mtime,
                        vec![],
                        Some(file.path.to_string_lossy().into_owned()),
                        metadata.created_at_unix_ms,
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
    let mut files = files;
    files.truncate(parse_candidate_cut(request));
    for file in files {
        let values = match read_jsonl_head(host, &file.path, DEFAULT_HEAD_BYTES, DEFAULT_LINE_LIMIT)
        {
            Ok(values) => values,
            Err(e) => return failed(&file.path, e),
        };
        let metadata = claude_transcript_metadata(&values);
        let resolved_title = metadata.resolved_title().map(str::to_owned);
        if let Some(id) = metadata.session_id {
            rows.push(record(
                CLIAgent::Claude,
                "claude",
                id,
                resolved_title,
                metadata.cwd,
                file.mtime,
                vec![],
                Some(file.path.to_string_lossy().into_owned()),
                metadata.created_at_unix_ms,
            ));
        }
    }
    DiscoveryOutcome::Complete(rows)
}

fn discover_omp(host: &dyn Host, request: &DiscoveryRequest) -> DiscoveryOutcome {
    let sessions = request.roots.omp_sessions();
    let files = match collect_omp_jsonl(host, &sessions, request.physical_source_limit) {
        Ok(files) if files.is_empty() => {
            return DiscoveryOutcome::SourceMissing {
                source: sessions.to_string_lossy().into_owned(),
            };
        }
        Ok(files) => files,
        Err(CollectError::Missing) => {
            return DiscoveryOutcome::SourceMissing {
                source: sessions.to_string_lossy().into_owned(),
            };
        }
        Err(CollectError::Limit) => {
            return DiscoveryOutcome::SourceLimitExceeded {
                source: sessions.to_string_lossy().into_owned(),
                limit: request.physical_source_limit as u64,
            };
        }
        Err(CollectError::Io(error)) => return failed(&sessions, error),
    };
    let mut rows = Vec::new();
    for file in files.into_iter().take(parse_candidate_cut(request)) {
        let values = match read_jsonl_head(host, &file.path, DEFAULT_HEAD_BYTES, DEFAULT_LINE_LIMIT)
        {
            Ok(values) => values,
            Err(error) => return failed(&file.path, error),
        };
        let metadata = omp_transcript_metadata(&values);
        let resolved_title = metadata.resolved_title().map(str::to_owned);
        let Some(id) = metadata.session_id else {
            continue;
        };
        let file_id = file
            .path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.rsplit_once('_').map(|(_, id)| id));
        if file_id != Some(id.as_str()) {
            return failed(
                &file.path,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Omp session id does not match its <timestamp>_<id>.jsonl filename",
                ),
            );
        }
        rows.push(record(
            CLIAgent::Omp,
            "omp",
            id,
            resolved_title,
            metadata.cwd,
            file.mtime,
            vec![],
            Some(file.path.to_string_lossy().into_owned()),
            metadata.created_at_unix_ms,
        ));
    }
    DiscoveryOutcome::Complete(rows)
}

fn discover_grok(host: &dyn Host, request: &DiscoveryRequest) -> DiscoveryOutcome {
    let sessions = request.roots.grok_sessions();
    let files = match collect_grok_summaries(host, &sessions, request.physical_source_limit) {
        Ok(files) if files.is_empty() => {
            return DiscoveryOutcome::SourceMissing {
                source: sessions.to_string_lossy().into_owned(),
            };
        }
        Ok(files) => files,
        Err(CollectError::Missing) => {
            return DiscoveryOutcome::SourceMissing {
                source: sessions.to_string_lossy().into_owned(),
            };
        }
        Err(CollectError::Limit) => {
            return DiscoveryOutcome::SourceLimitExceeded {
                source: sessions.to_string_lossy().into_owned(),
                limit: request.physical_source_limit as u64,
            };
        }
        Err(CollectError::Io(error)) => return failed(&sessions, error),
    };
    let mut rows = Vec::new();
    for file in files.into_iter().take(parse_candidate_cut(request)) {
        let bytes = match host.read_file_prefix(&file.path, DEFAULT_HEAD_BYTES) {
            Ok(bytes) => bytes,
            Err(error) => return failed(&file.path, error),
        };
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(error) => {
                return failed(
                    &file.path,
                    io::Error::new(io::ErrorKind::InvalidData, error),
                );
            }
        };
        let metadata = grok_summary_metadata(&value);
        let Some(id) = metadata.session_id.as_deref() else {
            continue;
        };
        let dir_id = file
            .path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());
        if dir_id != Some(id) {
            return failed(
                &file.path,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Grok session id does not match its session directory name",
                ),
            );
        }
        let mut row = record(
            CLIAgent::Grok,
            "grok",
            id.to_owned(),
            metadata.resolved_title().map(str::to_owned),
            metadata.cwd,
            file.mtime,
            vec![],
            Some(file.path.to_string_lossy().into_owned()),
            metadata.created_at_unix_ms,
        );
        if let Some(updated) = grok_summary_updated_at_unix_ms(&value) {
            row.updated_at_unix_ms = Some(updated);
        }
        rows.push(row);
    }
    DiscoveryOutcome::Complete(rows)
}

fn discover_gemini(host: &dyn Host, request: &DiscoveryRequest) -> DiscoveryOutcome {
    let root = request.roots.gemini_tmp();
    let files = match collect_gemini_sessions(host, &root, request.physical_source_limit) {
        Ok(files) if files.is_empty() => {
            return DiscoveryOutcome::SourceMissing {
                source: root.to_string_lossy().into_owned(),
            };
        }
        Ok(files) => files,
        Err(CollectError::Missing) => {
            return DiscoveryOutcome::SourceMissing {
                source: root.to_string_lossy().into_owned(),
            };
        }
        Err(CollectError::Limit) => {
            return DiscoveryOutcome::SourceLimitExceeded {
                source: root.to_string_lossy().into_owned(),
                limit: request.physical_source_limit as u64,
            };
        }
        Err(CollectError::Io(error)) => return failed(&root, error),
    };
    let mut rows = Vec::new();
    for file in files.into_iter().take(parse_candidate_cut(request)) {
        if !descriptor_for_id(ProviderId::Gemini).accepts_source(&request.roots, &file.path) {
            continue;
        }
        let values = match read_jsonl_head(host, &file.path, DEFAULT_HEAD_BYTES, DEFAULT_LINE_LIMIT)
        {
            Ok(values) => values,
            Err(error) => return failed(&file.path, error),
        };
        let Some(header) = values.first() else {
            continue;
        };
        let metadata = gemini_header_metadata(header);
        let Some(id) = metadata.session_id else {
            continue;
        };
        let cwd = gemini_project_root(host, &file.path);
        let title = gemini_first_user_excerpt(&values);
        let mut row = record(
            CLIAgent::Gemini,
            "gemini",
            id,
            title,
            cwd,
            file.mtime,
            vec![],
            Some(file.path.to_string_lossy().into_owned()),
            metadata.created_at_unix_ms,
        );
        let updated = values
            .iter()
            .rev()
            .find_map(gemini_updated_at_unix_ms)
            .or(metadata.created_at_unix_ms);
        if let Some(updated) = updated {
            row.updated_at_unix_ms = Some(updated);
        }
        rows.push(row);
    }
    DiscoveryOutcome::Complete(rows)
}

fn gemini_project_root(host: &dyn Host, session_path: &Path) -> Option<String> {
    let slug_dir = session_path.parent()?.parent()?;
    let project_root = host.join(slug_dir, ".project_root");
    let bytes = host.read_file_prefix(&project_root, 4 * 1024).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let cwd = text.lines().next().unwrap_or(text.as_ref()).trim();
    (!cwd.is_empty()).then(|| cwd.to_owned())
}

fn discover_generic_jsonl(
    host: &dyn Host,
    request: &DiscoveryRequest,
    descriptor: &ProviderDescriptor,
) -> DiscoveryOutcome {
    let mut files = Vec::new();
    let mut found_source = false;
    for root in descriptor.source_roots(&request.roots) {
        let collected = if descriptor.id == ProviderId::Cursor {
            collect_cursor_jsonl(host, &root, request.physical_source_limit)
        } else {
            collect_jsonl(host, &root, request.physical_source_limit)
        };
        match collected {
            Ok(mut discovered) => {
                found_source = true;
                files.append(&mut discovered);
            }
            Err(CollectError::Missing) => {}
            Err(CollectError::Limit) => {
                return DiscoveryOutcome::SourceLimitExceeded {
                    source: root.to_string_lossy().into_owned(),
                    limit: request.physical_source_limit as u64,
                };
            }
            Err(CollectError::Io(error)) => return failed(&root, error),
        }
    }
    if !found_source {
        return DiscoveryOutcome::SourceMissing {
            source: descriptor
                .source_roots(&request.roots)
                .iter()
                .map(|path| path.to_string_lossy())
                .collect::<Vec<_>>()
                .join(", "),
        };
    }
    files.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
    files.truncate(parse_candidate_cut(request));
    let mut rows = Vec::new();
    for file in files {
        if !descriptor.accepts_source(&request.roots, &file.path) {
            continue;
        }
        let values = match read_jsonl_head(host, &file.path, DEFAULT_HEAD_BYTES, DEFAULT_LINE_LIMIT)
        {
            Ok(values) => values,
            Err(error) => return failed(&file.path, error),
        };
        let Some((session_id, title, cwd)) =
            generic_jsonl_metadata(descriptor.id, &file.path, &values)
        else {
            continue;
        };
        rows.push(record(
            descriptor.agent,
            descriptor.id.slug(),
            session_id,
            title,
            cwd,
            file.mtime,
            Vec::new(),
            Some(file.path.to_string_lossy().into_owned()),
            None,
        ));
    }
    DiscoveryOutcome::Complete(rows)
}

fn discover_opencode(
    host: &dyn Host,
    request: &DiscoveryRequest,
    descriptor: &ProviderDescriptor,
) -> DiscoveryOutcome {
    let root = request.roots.opencode_legacy_sessions();
    let files = match collect_files(host, &root, request.physical_source_limit, &["json"]) {
        Ok(files) => files,
        Err(CollectError::Missing) => {
            return DiscoveryOutcome::SourceMissing {
                source: root.to_string_lossy().into_owned(),
            };
        }
        Err(CollectError::Limit) => {
            return DiscoveryOutcome::SourceLimitExceeded {
                source: root.to_string_lossy().into_owned(),
                limit: request.physical_source_limit as u64,
            };
        }
        Err(CollectError::Io(error)) => return failed(&root, error),
    };
    let mut rows = Vec::new();
    for file in files.into_iter().take(parse_candidate_cut(request)) {
        if !descriptor.accepts_source(&request.roots, &file.path) {
            continue;
        }
        let bytes = match host.read_file_prefix(&file.path, DEFAULT_HEAD_BYTES) {
            Ok(bytes) => bytes,
            Err(error) => return failed(&file.path, error),
        };
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(error) => {
                return failed(
                    &file.path,
                    io::Error::new(io::ErrorKind::InvalidData, error),
                );
            }
        };
        let session_id = string_at(&value, &["id"]).map(str::to_owned).or_else(|| {
            file.path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        });
        let Some(session_id) = session_id else {
            continue;
        };
        let title = string_at(&value, &["title"]).and_then(excerpt);
        let cwd = string_at(&value, &["directory"]).map(str::to_owned);
        rows.push(record(
            descriptor.agent,
            descriptor.id.slug(),
            session_id,
            title,
            cwd,
            file.mtime,
            Vec::new(),
            Some(file.path.to_string_lossy().into_owned()),
            None,
        ));
    }
    DiscoveryOutcome::Complete(rows)
}

fn generic_jsonl_metadata(
    provider: ProviderId,
    path: &Path,
    values: &[serde_json::Value],
) -> Option<(String, Option<String>, Option<String>)> {
    let mut session_id = None;
    let mut title = None;
    let mut cwd = None;
    for value in values {
        session_id = session_id.or_else(|| {
            [
                &["sessionId"][..],
                &["session_id"][..],
                &["id"][..],
                &["data", "sessionId"][..],
                &["data", "session_id"][..],
            ]
            .into_iter()
            .find_map(|path| string_at(value, path).map(str::to_owned))
        });
        cwd = cwd.or_else(|| {
            [&["cwd"][..], &["directory"][..], &["data", "cwd"][..]]
                .into_iter()
                .find_map(|path| string_at(value, path).map(str::to_owned))
        });
        title = title.or_else(|| {
            [
                &["title"][..],
                &["data", "title"][..],
                &["data", "content"][..],
                &["message", "content"][..],
                &["message", "text"][..],
                &["content"][..],
            ]
            .into_iter()
            .find_map(|path| string_at(value, path).and_then(excerpt))
        });
    }
    if provider == ProviderId::Antigravity && session_id.is_none() {
        session_id = path
            .strip_prefix(path.ancestors().last().unwrap_or(path))
            .ok()
            .and_then(|_| path.ancestors().nth(3))
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned());
    }
    Some((session_id?, title, cwd))
}

fn string_at<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a str> {
    let mut value = value;
    for key in path {
        value = value.get(*key)?;
    }
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn excerpt(value: &str) -> Option<String> {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.is_empty() {
        None
    } else {
        Some(value.chars().take(160).collect())
    }
}

fn record(
    agent: CLIAgent,
    provider: &str,
    session_id: String,
    title: Option<String>,
    cwd: Option<String>,
    mtime: Option<MTime>,
    launch_argv: Vec<String>,
    source_path: Option<String>,
    created_at_unix_ms: Option<u64>,
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
        source_path,
        created_at_unix_ms,
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

/// Read a JSONL source as a bounded head prefix and parse it strictly. The
/// final (possibly cut) line is dropped before parsing so a truncated tail
/// never turns into a malformed-line failure; genuinely malformed complete
/// lines still fail closed.
fn read_jsonl_head(
    host: &dyn Host,
    path: &Path,
    head_bytes: u64,
    line_limit: usize,
) -> Result<Vec<serde_json::Value>, io::Error> {
    let mut bytes = host.read_file_prefix(path, head_bytes)?;
    if bytes.len() as u64 == head_bytes {
        match bytes.iter().rposition(|b| *b == b'\n') {
            Some(last_newline) => bytes.truncate(last_newline + 1),
            None => bytes.clear(),
        }
    }
    parse_jsonl_strict(&bytes, line_limit)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))
}

fn parse_candidate_cut(request: &DiscoveryRequest) -> usize {
    (request.logical_limit * PARSE_CANDIDATE_FACTOR).max(DEFAULT_LOGICAL_LIMIT)
}
fn collect_omp_jsonl(
    host: &dyn Host,
    root: &Path,
    limit: usize,
) -> Result<Vec<SourceFile>, CollectError> {
    let root_meta = host.stat(root).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            CollectError::Missing
        } else {
            CollectError::Io(error)
        }
    })?;
    if !root_meta.is_dir {
        return Err(CollectError::Io(io::Error::new(
            io::ErrorKind::NotADirectory,
            "Omp session source is not a directory",
        )));
    }
    let mut files = Vec::new();
    let mut seen = 0usize;
    for bucket in host.read_dir(root, None).map_err(CollectError::Io)? {
        seen += 1;
        if seen > limit {
            return Err(CollectError::Limit);
        }
        if !bucket.is_dir || bucket.is_symlink {
            continue;
        }
        let bucket_path = host.join(root, &bucket.name);
        for entry in host
            .read_dir(&bucket_path, None)
            .map_err(CollectError::Io)?
        {
            seen += 1;
            if seen > limit {
                return Err(CollectError::Limit);
            }
            if entry.is_dir || entry.is_symlink || !entry.name.ends_with(".jsonl") {
                continue;
            }
            let path = host.join(&bucket_path, &entry.name);
            let mtime = host.stat(&path).ok().and_then(|meta| meta.mtime);
            files.push(SourceFile { path, mtime });
        }
    }
    files.sort_by(|left, right| {
        right
            .mtime
            .cmp(&left.mtime)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(files)
}

fn collect_grok_summaries(
    host: &dyn Host,
    root: &Path,
    limit: usize,
) -> Result<Vec<SourceFile>, CollectError> {
    let root_meta = host.stat(root).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            CollectError::Missing
        } else {
            CollectError::Io(error)
        }
    })?;
    if !root_meta.is_dir {
        return Err(CollectError::Io(io::Error::new(
            io::ErrorKind::NotADirectory,
            "Grok session source is not a directory",
        )));
    }
    let mut files = Vec::new();
    let mut seen = 0usize;
    for bucket in host.read_dir(root, None).map_err(CollectError::Io)? {
        seen += 1;
        if seen > limit {
            return Err(CollectError::Limit);
        }
        // Root siblings such as session_search.sqlite count toward the budget
        // but are never session sources.
        if !bucket.is_dir || bucket.is_symlink {
            continue;
        }
        let bucket_path = host.join(root, &bucket.name);
        for entry in host
            .read_dir(&bucket_path, None)
            .map_err(CollectError::Io)?
        {
            seen += 1;
            if seen > limit {
                return Err(CollectError::Limit);
            }
            if !entry.is_dir || entry.is_symlink {
                continue;
            }
            let session_dir = host.join(&bucket_path, &entry.name);
            let summary = host.join(&session_dir, "summary.json");
            match host.stat(&summary) {
                Ok(meta) if !meta.is_dir => {
                    files.push(SourceFile {
                        path: summary,
                        mtime: meta.mtime,
                    });
                }
                Ok(_) | Err(_) => {}
            }
        }
    }
    files.sort_by(|left, right| {
        right
            .mtime
            .cmp(&left.mtime)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(files)
}

fn collect_gemini_sessions(
    host: &dyn Host,
    root: &Path,
    limit: usize,
) -> Result<Vec<SourceFile>, CollectError> {
    let root_meta = host.stat(root).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            CollectError::Missing
        } else {
            CollectError::Io(error)
        }
    })?;
    if !root_meta.is_dir {
        return Err(CollectError::Io(io::Error::new(
            io::ErrorKind::NotADirectory,
            "Gemini tmp source is not a directory",
        )));
    }
    let mut files = Vec::new();
    let mut seen = 0usize;
    for slug in host.read_dir(root, None).map_err(CollectError::Io)? {
        seen += 1;
        if seen > limit {
            return Err(CollectError::Limit);
        }
        if !slug.is_dir || slug.is_symlink {
            continue;
        }
        let slug_path = host.join(root, &slug.name);
        let chats = host.join(&slug_path, "chats");
        let chat_entries = match host.read_dir(&chats, None) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(CollectError::Io(error)),
        };
        for entry in chat_entries {
            seen += 1;
            if seen > limit {
                return Err(CollectError::Limit);
            }
            if entry.is_dir || entry.is_symlink {
                continue;
            }
            let Some(name) = entry.name.strip_prefix("session-") else {
                continue;
            };
            if !name.ends_with(".jsonl") {
                continue;
            }
            let path = host.join(&chats, &entry.name);
            let mtime = host.stat(&path).ok().and_then(|meta| meta.mtime);
            files.push(SourceFile { path, mtime });
        }
    }
    files.sort_by(|left, right| {
        right
            .mtime
            .cmp(&left.mtime)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(files)
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
    collect_files(host, root, limit, &["jsonl"])
}

fn collect_cursor_jsonl(
    host: &dyn Host,
    root: &Path,
    limit: usize,
) -> Result<Vec<SourceFile>, CollectError> {
    let root_meta = host.stat(root).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            CollectError::Missing
        } else {
            CollectError::Io(error)
        }
    })?;
    if !root_meta.is_dir {
        return Err(CollectError::Io(io::Error::new(
            io::ErrorKind::NotADirectory,
            "Cursor projects source is not a directory",
        )));
    }

    let mut seen = 0usize;
    let mut dirs = VecDeque::new();
    for project in host.read_dir(root, None).map_err(CollectError::Io)? {
        seen += 1;
        if seen > limit {
            return Err(CollectError::Limit);
        }
        if !project.is_dir || project.is_symlink {
            continue;
        }
        let project = host.join(root, &project.name);
        let transcripts = host.join(&project, "agent-transcripts");
        match host.stat(&transcripts) {
            Ok(meta) if meta.is_dir => dirs.push_back(transcripts),
            Ok(_) => {
                return Err(CollectError::Io(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "Cursor agent-transcripts source is not a directory",
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(CollectError::Io(error)),
        }
    }

    let mut files = Vec::new();
    while let Some(dir) = dirs.pop_front() {
        for entry in host.read_dir(&dir, None).map_err(CollectError::Io)? {
            seen += 1;
            if seen > limit {
                return Err(CollectError::Limit);
            }
            let path = host.join(&dir, &entry.name);
            if entry.is_dir && !entry.is_symlink {
                dirs.push_back(path);
            } else if !entry.is_dir
                && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
            {
                let mtime = host.stat(&path).ok().and_then(|meta| meta.mtime);
                files.push(SourceFile { path, mtime });
            }
        }
    }
    files.sort_by(|left, right| {
        right
            .mtime
            .cmp(&left.mtime)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(files)
}

fn collect_files(
    host: &dyn Host,
    root: &Path,
    limit: usize,
    extensions: &[&str],
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
            } else if !entry.is_dir
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extensions.contains(&extension))
            {
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
    fn missing_optional_provider_does_not_hide_complete_rows() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        fs::create_dir_all(roots.codex_sessions()).unwrap();
        fs::write(
            roots.codex_sessions().join("rollout.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"019fa76a-6276-7b03-b302-c640686b2033","cwd":"/codex"}}
{"type":"event_msg","payload":{"type":"user_message","message":"Keep this row"}}
"#,
        )
        .unwrap();
        let outcome = discover(&*LocalHost::new(), &DiscoveryRequest::standard(roots));
        let DiscoveryOutcome::Complete(rows) = outcome else {
            panic!("missing Claude/Omp stores must not hide Codex rows: {outcome:?}")
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key.provider, "codex");
    }

    #[test]
    fn cursor_scanner_ignores_unrelated_project_cache_nodes() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        let noise = roots.cursor_projects().join("a-noise");
        for index in 0..12 {
            fs::create_dir_all(noise.join(format!("cache-{index}"))).unwrap();
        }
        let transcript = roots
            .cursor_projects()
            .join("repo/agent-transcripts/cursor-session/cursor.jsonl");
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        fs::write(
            &transcript,
            serde_json::json!({
                "sessionId": "cursor-1",
                "role": "user",
                "message": {"content": "Keep Cursor discoverable"},
                "cwd": "/work/cursor"
            })
            .to_string(),
        )
        .unwrap();

        let outcome = discover(
            &*LocalHost::new(),
            &DiscoveryRequest {
                providers: vec![ProviderId::Cursor],
                physical_source_limit: 8,
                ..DiscoveryRequest::standard(roots)
            },
        );
        let DiscoveryOutcome::Complete(rows) = outcome else {
            panic!("unrelated Cursor cache nodes must not consume transcript budget: {outcome:?}")
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key.session_id, "cursor-1");
    }

    #[test]
    fn omp_discovery_reads_one_project_level_and_ignores_diagnostic_only_files() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        let bucket = roots.omp_sessions().join("-agentty");
        fs::create_dir_all(bucket.join("tool-logs")).unwrap();
        let id = "019fa6d8-49ed-7000-b67f-09845d463582";
        let transcript = bucket.join(format!("2026-07-28T03-50-20-397Z_{id}.jsonl"));
        fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                serde_json::json!({"type":"title","title":"Omp session title"}),
                serde_json::json!({"type":"session","id":id,"cwd":"/work/omp","timestamp":"2026-07-28T03:50:20.397Z"})
            ),
        )
        .unwrap();
        fs::write(
            bucket.join("2026-07-28T04-00-00-000Z_diagnostic.jsonl"),
            serde_json::json!({"type":"custom","customType":"session_exit"}).to_string(),
        )
        .unwrap();
        fs::write(
            bucket.join("tool-logs/nested.jsonl"),
            serde_json::json!({"type":"session","id":"nested"}).to_string(),
        )
        .unwrap();

        let outcome = discover(
            &*LocalHost::new(),
            &DiscoveryRequest {
                providers: vec![ProviderId::Omp],
                ..DiscoveryRequest::standard(roots)
            },
        );
        let DiscoveryOutcome::Complete(rows) = outcome else {
            panic!("expected complete Omp discovery: {outcome:?}")
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agent, CLIAgent::Omp);
        assert_eq!(rows[0].key.session_id, id);
        assert_eq!(rows[0].title.as_deref(), Some("Omp session title"));
        assert_eq!(rows[0].cwd.as_deref(), Some("/work/omp"));
        assert_eq!(rows[0].source_path.as_deref(), transcript.to_str());
        assert_eq!(rows[0].created_at_unix_ms, Some(1_785_210_620_397));
    }

    #[test]
    fn omp_session_header_requires_filename_identity_match() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        let bucket = roots.omp_sessions().join("-agentty");
        fs::create_dir_all(&bucket).unwrap();
        fs::write(
            bucket.join("1000_filename.jsonl"),
            serde_json::json!({"type":"session","id":"payload"}).to_string(),
        )
        .unwrap();
        assert!(matches!(
            discover(
                &*LocalHost::new(),
                &DiscoveryRequest {
                    providers: vec![ProviderId::Omp],
                    ..DiscoveryRequest::standard(roots)
                }
            ),
            DiscoveryOutcome::Partial { .. }
        ));
    }

    #[test]
    fn grok_discovery_reads_summary_json_and_ignores_sibling_caches() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        let id = "019fd593-3979-7551-825d-bf5f8681a697";
        let bucket = roots.grok_sessions().join("%2Fwork%2Fgrok");
        let session_dir = bucket.join(id);
        fs::create_dir_all(&session_dir).unwrap();
        fs::create_dir_all(roots.grok_sessions()).unwrap();
        fs::write(roots.grok_sessions().join("session_search.sqlite"), "cache").unwrap();
        fs::write(bucket.join("prompt_history.jsonl"), "{}\n").unwrap();
        let summary = session_dir.join("summary.json");
        fs::write(
            &summary,
            serde_json::json!({
                "info": {"id": id, "cwd": "/work/grok"},
                "generated_title": "Grok session title",
                "session_summary": "ignored when generated_title present",
                "created_at": "2026-08-06T05:37:03.447Z",
                "updated_at": "2026-08-06T06:04:28.862Z",
            })
            .to_string(),
        )
        .unwrap();
        // Directory without summary.json must not become a row.
        fs::create_dir_all(bucket.join("019fd593-3979-7551-825d-bf5f8681a698")).unwrap();

        let outcome = discover(
            &*LocalHost::new(),
            &DiscoveryRequest {
                providers: vec![ProviderId::Grok],
                ..DiscoveryRequest::standard(roots)
            },
        );
        let DiscoveryOutcome::Complete(rows) = outcome else {
            panic!("expected complete Grok discovery: {outcome:?}")
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agent, CLIAgent::Grok);
        assert_eq!(rows[0].key.provider, "grok");
        assert_eq!(rows[0].key.session_id, id);
        assert_eq!(rows[0].title.as_deref(), Some("Grok session title"));
        assert_eq!(rows[0].cwd.as_deref(), Some("/work/grok"));
        assert_eq!(rows[0].source_path.as_deref(), summary.to_str());
        assert_eq!(rows[0].created_at_unix_ms, Some(1_785_994_623_447));
        assert_eq!(rows[0].updated_at_unix_ms, Some(1_785_996_268_862));
    }

    #[test]
    fn gemini_discovery_reads_tmp_chats_and_skips_session_context_titles() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        let slug = roots.gemini_tmp().join("repo");
        let chats = slug.join("chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(slug.join(".project_root"), "/work/gemini\n").unwrap();
        let session = chats.join("session-2026-08-07T12-00-abcdef12.jsonl");
        let id = "abcdef12-3456-7890-abcd-ef1234567890";
        fs::write(
            &session,
            format!(
                "{}\n{}\n{}\n",
                serde_json::json!({
                    "sessionId": id,
                    "projectHash": "hash",
                    "startTime": "2026-08-07T12:00:00.000Z",
                    "lastUpdated": "2026-08-07T12:00:00.000Z",
                    "kind": "main"
                }),
                serde_json::json!({
                    "$set": {
                        "messages": [{
                            "id": "m1",
                            "timestamp": "2026-08-07T12:00:01.000Z",
                            "type": "user",
                            "content": [{"text": "<session_context>\nThis is the Gemini CLI. We are setting up the context for our chat.\n"}]
                        }, {
                            "id": "m2",
                            "timestamp": "2026-08-07T12:01:00.000Z",
                            "type": "user",
                            "content": [{"text": "Explain the Gemini discovery path"}]
                        }],
                        "lastUpdated": "2026-08-07T12:05:00.000Z"
                    }
                }),
                serde_json::json!({"$set":{"lastUpdated":"2026-08-07T12:05:00.000Z"}})
            ),
        )
        .unwrap();
        // Non-session files and missing chats dirs must not become rows.
        fs::write(chats.join("notes.jsonl"), "{}\n").unwrap();
        fs::create_dir_all(roots.gemini_tmp().join("empty")).unwrap();

        let outcome = discover(
            &*LocalHost::new(),
            &DiscoveryRequest {
                providers: vec![ProviderId::Gemini],
                ..DiscoveryRequest::standard(roots)
            },
        );
        let DiscoveryOutcome::Complete(rows) = outcome else {
            panic!("expected complete Gemini discovery: {outcome:?}")
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agent, CLIAgent::Gemini);
        assert_eq!(rows[0].key.provider, "gemini");
        assert_eq!(rows[0].key.session_id, id);
        assert_eq!(
            rows[0].title.as_deref(),
            Some("Explain the Gemini discovery path")
        );
        assert_eq!(rows[0].cwd.as_deref(), Some("/work/gemini"));
        assert_eq!(rows[0].source_path.as_deref(), session.to_str());
        assert_eq!(rows[0].created_at_unix_ms, Some(1_786_104_000_000));
        assert_eq!(rows[0].updated_at_unix_ms, Some(1_786_104_300_000));
    }

    #[test]
    fn gemini_discovery_ignores_antigravity_cli_tree() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        let brain = roots.antigravity_brain().join("ag-1/conversations/main");
        fs::create_dir_all(&brain).unwrap();
        fs::write(
            brain.join("transcript.jsonl"),
            serde_json::json!({"source":"USER_EXPLICIT","type":"USER_INPUT","content":"Antigravity only"}).to_string(),
        )
        .unwrap();
        let outcome = discover(
            &*LocalHost::new(),
            &DiscoveryRequest {
                providers: vec![ProviderId::Gemini],
                ..DiscoveryRequest::standard(roots)
            },
        );
        assert!(
            matches!(outcome, DiscoveryOutcome::SourceMissing { .. }),
            "{outcome:?}"
        );
    }

    #[test]
    fn grok_summary_requires_directory_identity_match() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        let session_dir = roots
            .grok_sessions()
            .join("%2Fwork")
            .join("019fd593-3979-7551-825d-bf5f8681a697");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            session_dir.join("summary.json"),
            serde_json::json!({
                "info": {"id": "other-id", "cwd": "/work"},
                "generated_title": "mismatch",
            })
            .to_string(),
        )
        .unwrap();
        assert!(matches!(
            discover(
                &*LocalHost::new(),
                &DiscoveryRequest {
                    providers: vec![ProviderId::Grok],
                    ..DiscoveryRequest::standard(roots)
                }
            ),
            DiscoveryOutcome::Partial { .. }
        ));
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

    /// End-to-end proof against a real machine's agent stores. Opt-in via
    /// AGENTTY_E2E_HOME so CI stays hermetic; the test asserts the discovery
    /// pipeline (roots -> scan -> parse -> canonicalize) produces rows from
    /// whatever real stores exist there, or an explicit SourceMissing rather
    /// than a silent empty list.
    #[test]
    fn discovers_real_sessions_on_this_machine() {
        let Some(home) = std::env::var_os("AGENTTY_E2E_HOME") else {
            eprintln!("AGENTTY_E2E_HOME unset; skipping real-machine e2e");
            return;
        };
        let roots = AgentStoreRoots::for_home(PathBuf::from(home));
        let outcome = discover(&*LocalHost::new(), &DiscoveryRequest::standard(roots));
        match &outcome {
            DiscoveryOutcome::Complete(rows) => {
                assert!(
                    !rows.is_empty(),
                    "real stores exist but discovery returned zero rows"
                );
                for row in rows {
                    assert!(!row.key.session_id.is_empty());
                    let invocation =
                        row.agent
                            .resume_invocation(&row.key.session_id, None, row.cwd.clone());
                    assert!(
                        invocation.is_some(),
                        "row {:?} cannot produce a resume invocation",
                        row.key
                    );
                }
                eprintln!("e2e discovery rows: {}", rows.len());
                if std::path::Path::new(&std::env::var_os("AGENTTY_E2E_HOME").unwrap())
                    .join(".omp/agent/sessions")
                    .is_dir()
                {
                    assert!(
                        rows.iter().any(|row| row.key.provider == "omp"),
                        "real Omp store exists but discovery produced no Omp rows"
                    );
                }
            }
            DiscoveryOutcome::SourceMissing { source } => {
                eprintln!("no real stores present ({source}); nothing to prove");
            }
            other => panic!("real-machine discovery must not degrade: {other:?}"),
        }
    }

    #[test]
    fn huge_transcripts_are_scanned_via_bounded_head_read() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        fs::create_dir_all(roots.codex_sessions()).unwrap();
        // Well over the old 4 MB whole-file cap: metadata in the head, then a
        // long filler tail. The head cut lands mid-line, which must be dropped
        // rather than parsed as malformed JSON.
        let mut content = String::from(
            r#"{"type":"session_meta","payload":{"id":"019fa76a-6276-7b03-b302-c640686b2033","cwd":"/codex"}}
{"type":"event_msg","payload":{"type":"user_message","message":"Huge session"}}
"#,
        );
        // Valid-JSON filler lines (strict parse must accept every complete
        // line in the head); length chosen so the 256 KiB head cut lands
        // mid-line, exercising the truncated-tail drop.
        while content.len() < 5 * 1024 * 1024 {
            content.push_str(&format!(
                r#"{{"type":"event_msg","payload":{{"type":"token","text":"{}"}}}}
"#,
                "x".repeat(1000)
            ));
        }
        fs::write(roots.codex_sessions().join("huge.jsonl"), content).unwrap();
        let outcome = discover(
            &*LocalHost::new(),
            &DiscoveryRequest {
                providers: vec![ProviderId::Codex],
                ..DiscoveryRequest::codex_and_claude(roots)
            },
        );
        let DiscoveryOutcome::Complete(rows) = outcome else {
            panic!("huge transcript must not fail the provider: {outcome:?}")
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title.as_deref(), Some("Huge session"));
    }

    #[test]
    fn only_the_most_recent_candidates_are_parsed() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        fs::create_dir_all(roots.claude_projects().join("repo")).unwrap();
        let dir = roots.claude_projects().join("repo");
        // logical_limit 1 -> parse cut is max(1*4, 40) = 40 files; write 50.
        for index in 0..50 {
            let path = dir.join(format!("s{index:02}.jsonl"));
            fs::write(
                &path,
                format!(
                    r#"{{"type":"user","sessionId":"s{index:02}","cwd":"/c","message":{{"content":"row {index:02}"}}}}
"#
                ),
            )
            .unwrap();
            // Monotonically increasing mtimes: s00 oldest, s49 newest.
            let mtime = filetime::FileTime::from_unix_time(1_700_000_000 + index, 0);
            filetime::set_file_mtime(&path, mtime).unwrap();
        }
        let outcome = discover(
            &*LocalHost::new(),
            &DiscoveryRequest {
                providers: vec![ProviderId::Claude],
                logical_limit: 1,
                ..DiscoveryRequest::codex_and_claude(roots)
            },
        );
        let DiscoveryOutcome::Complete(rows) = outcome else {
            panic!("expected complete: {outcome:?}")
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key.session_id, "s49");
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
                providers: vec![ProviderId::Codex],
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
                providers: vec![ProviderId::Codex],
                physical_source_limit: 1,
                ..DiscoveryRequest::codex_and_claude(roots)
            },
        );
        assert!(matches!(
            outcome,
            DiscoveryOutcome::SourceLimitExceeded { limit: 1, .. }
        ));
    }

    #[test]
    fn standard_discovery_covers_registered_persisted_providers() {
        let request = DiscoveryRequest::standard(AgentStoreRoots::for_home("/home/alice".into()));
        assert_eq!(
            request.providers,
            super::super::provider::PERSISTED_PROVIDER_DESCRIPTORS
                .iter()
                .map(|descriptor| descriptor.id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn custom_codex_and_claude_roots_apply_to_scan_and_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let codex_home = temp.path().join("custom-codex");
        let claude_config = temp.path().join("custom-claude");
        let roots = AgentStoreRoots::from_target_environment(&home, |name| match name {
            "CODEX_HOME" => Some(codex_home.as_os_str().to_owned()),
            "CLAUDE_CONFIG_DIR" => Some(claude_config.as_os_str().to_owned()),
            _ => None,
        });
        fs::create_dir_all(roots.codex_sessions()).unwrap();
        let source = roots.codex_sessions().join("custom.jsonl");
        fs::write(
            &source,
            r#"{"type":"session_meta","payload":{"id":"019fa76a-6276-7b03-b302-c640686b2033","cwd":"/custom"}}
"#,
        )
        .unwrap();
        let outcome = discover(
            &*LocalHost::new(),
            &DiscoveryRequest {
                providers: vec![ProviderId::Codex],
                ..DiscoveryRequest::standard(roots.clone())
            },
        );
        assert!(matches!(outcome, DiscoveryOutcome::Complete(rows) if rows.len() == 1));

        let mut navigator = super::super::navigator::SessionNavigator::default();
        navigator.refresh(
            &[AgentSessionRecord {
                key: AgentSessionKey {
                    provider: "codex".into(),
                    session_id: "019fa76a-6276-7b03-b302-c640686b2033".into(),
                },
                agent: CLIAgent::Codex,
                title: None,
                cwd: None,
                updated_at_unix_ms: None,
                launch_argv: Vec::new(),
                source_path: Some(source.to_string_lossy().into_owned()),
                created_at_unix_ms: None,
            }],
            &[],
        );
        let plan = navigator.plan_delete(&navigator.rows()[0].row_id).unwrap();
        assert!(super::super::delete::plan_session_delete_source(&plan, &roots).is_ok());
        assert!(
            super::super::delete::plan_session_delete_source(
                &plan,
                &AgentStoreRoots::for_home(home),
            )
            .is_err()
        );
    }

    #[test]
    fn expanded_provider_parsers_extract_native_identity_title_and_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let roots = AgentStoreRoots::for_home(temp.path().to_path_buf());
        let project = temp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let fixtures = [
            (
                ProviderId::Droid,
                roots.droid_sessions().join("droid-1.jsonl"),
                serde_json::json!({"type":"session_start","id":"droid-1","title":"Droid task","cwd":project}).to_string(),
                "droid-1",
                "Droid task",
            ),
            (
                ProviderId::Copilot,
                roots.copilot_sessions().join("copilot.jsonl"),
                format!("{}\n{}", serde_json::json!({"type":"session.start","data":{"sessionId":"copilot-1"}}), serde_json::json!({"type":"user.message","data":{"content":"Copilot task"}})),
                "copilot-1",
                "Copilot task",
            ),
            (
                ProviderId::Pi,
                roots.pi_sessions().join("pi.jsonl"),
                format!("{}\n{}", serde_json::json!({"type":"session","id":"pi-1","cwd":project}), serde_json::json!({"type":"message","message":{"role":"user","content":"Pi task"}})),
                "pi-1",
                "Pi task",
            ),
            (
                ProviderId::Cursor,
                roots.cursor_projects().join("repo/agent-transcripts/cursor.jsonl"),
                serde_json::json!({"sessionId":"cursor-1","role":"user","message":{"content":"Cursor task"},"cwd":project}).to_string(),
                "cursor-1",
                "Cursor task",
            ),
            (
                ProviderId::Antigravity,
                roots.antigravity_brain().join("antigravity-1/conversations/main/transcript.jsonl"),
                serde_json::json!({"source":"USER_EXPLICIT","type":"USER_INPUT","content":"Antigravity task"}).to_string(),
                "antigravity-1",
                "Antigravity task",
            ),
            (
                ProviderId::OpenCode,
                roots.opencode_legacy_sessions().join("project/ses-open.json"),
                serde_json::json!({"id":"ses-open","title":"OpenCode task","directory":project,"time":{"updated":10}}).to_string(),
                "ses-open",
                "OpenCode task",
            ),
            (
                ProviderId::Grok,
                roots.grok_sessions().join("%2Fwork/019fd593-3979-7551-825d-bf5f8681a697/summary.json"),
                serde_json::json!({
                    "info": {"id":"019fd593-3979-7551-825d-bf5f8681a697","cwd":project},
                    "generated_title":"Grok task",
                    "updated_at":"2026-08-06T06:04:28.862Z"
                }).to_string(),
                "019fd593-3979-7551-825d-bf5f8681a697",
                "Grok task",
            ),
            (
                ProviderId::Gemini,
                roots.gemini_tmp().join("repo/chats/session-2026-08-07T12-00-abcdef12.jsonl"),
                format!(
                    "{}\n{}\n",
                    serde_json::json!({
                        "sessionId":"abcdef12-3456-7890-abcd-ef1234567890",
                        "startTime":"2026-08-07T12:00:00.000Z",
                        "lastUpdated":"2026-08-07T12:00:00.000Z",
                        "kind":"main"
                    }),
                    serde_json::json!({
                        "type":"user",
                        "content":[{"text":"Gemini task"}]
                    })
                ),
                "abcdef12-3456-7890-abcd-ef1234567890",
                "Gemini task",
            ),
        ];
        for (provider, path, content, expected_id, expected_title) in fixtures {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            if provider == ProviderId::Gemini {
                fs::write(
                    path.parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join(".project_root"),
                    project.to_string_lossy().as_bytes(),
                )
                .unwrap();
            }
            fs::write(&path, content).unwrap();
            let outcome = discover(
                &*LocalHost::new(),
                &DiscoveryRequest {
                    providers: vec![provider],
                    ..DiscoveryRequest::standard(roots.clone())
                },
            );
            let DiscoveryOutcome::Complete(rows) = outcome else {
                panic!("provider {provider:?} failed: {outcome:?}");
            };
            assert_eq!(rows.len(), 1, "provider {provider:?}");
            assert_eq!(rows[0].key.session_id, expected_id);
            assert_eq!(rows[0].title.as_deref(), Some(expected_title));
            if provider == ProviderId::Gemini {
                assert_eq!(rows[0].cwd.as_deref(), project.to_str());
            }
        }
    }
}
