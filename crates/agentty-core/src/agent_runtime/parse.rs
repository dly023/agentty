use serde_json::Value;

use super::title::{
    SessionTitleCandidates, first_user_title_candidate, is_absent_session_title,
    resolve_title_candidates,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionVisibility {
    #[default]
    UserVisible,
    Internal,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionMetadata {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub first_user_message: Option<String>,
    pub created_at_unix_ms: Option<u64>,
    pub visibility: SessionVisibility,
}

impl SessionMetadata {
    pub fn resolved_title(&self) -> Option<&str> {
        resolve_title_candidates(self.title.as_deref(), self.first_user_message.as_deref())
    }

    pub fn title_candidates(&self) -> SessionTitleCandidates {
        SessionTitleCandidates::from_raw(self.title.as_deref(), self.first_user_message.as_deref())
    }

    pub fn is_user_visible(&self) -> bool {
        self.visibility == SessionVisibility::UserVisible
    }
}

pub fn parse_jsonl_strict(bytes: &[u8], physical_line_limit: usize) -> Result<Vec<Value>, String> {
    let text =
        std::str::from_utf8(bytes).map_err(|e| format!("session source is not UTF-8: {e}"))?;
    let mut values = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if index >= physical_line_limit {
            return Err(format!(
                "session source exceeds physical line limit {physical_line_limit}"
            ));
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value = serde_json::from_str(line)
            .map_err(|e| format!("invalid JSONL at physical line {}: {e}", index + 1))?;
        values.push(value);
    }
    Ok(values)
}

pub fn canonical_codex_session_id(candidate: &str) -> Option<String> {
    fn uuid_like(value: &str) -> bool {
        value.len() == 36
            && value.bytes().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            })
    }
    let candidate = candidate.trim();
    if uuid_like(candidate) {
        return Some(candidate.to_string());
    }
    let suffix = candidate.get(candidate.len().checked_sub(36)?..)?;
    uuid_like(suffix).then(|| suffix.to_string())
}

pub fn codex_index_metadata(value: &Value) -> Option<SessionMetadata> {
    let id = first_canonical(&[
        string(value, "id"),
        string(value, "session_id"),
        string(value, "sessionId"),
    ])?;
    Some(SessionMetadata {
        session_id: Some(id),
        cwd: cwd(value),
        title: title(value),
        first_user_message: None,
        created_at_unix_ms: None,
        visibility: codex_origin_is_internal(value)
            .then_some(SessionVisibility::Internal)
            .unwrap_or_default(),
    })
}

pub fn codex_transcript_metadata(values: &[Value]) -> SessionMetadata {
    let session_id = values
        .iter()
        .filter(|value| string(value, "type") == Some("session_meta"))
        .find_map(|value| {
            first_canonical(&[
                nested(value, &["payload", "id"]),
                nested(value, &["payload", "session_id"]),
                nested(value, &["payload", "sessionId"]),
                string(value, "session_id"),
                string(value, "sessionId"),
            ])
        });
    let mut metadata = SessionMetadata {
        session_id,
        created_at_unix_ms: None,
        ..Default::default()
    };
    let mut has_internal_origin = false;
    let mut has_real_user_message = false;
    for value in values {
        has_internal_origin |= codex_origin_is_internal(value);
        metadata.cwd = metadata.cwd.or_else(|| cwd(value));
        if let Some(candidate) = title(value) {
            let replace_title = metadata
                .title
                .as_deref()
                .is_none_or(is_absent_session_title);
            if replace_title {
                metadata.title = Some(candidate);
            }
        }
        if let Some(message) = codex_user_message(value) {
            has_real_user_message = true;
            let replace_first_user = metadata
                .first_user_message
                .as_deref()
                .is_none_or(|current| first_user_title_candidate(current).is_none());
            if replace_first_user {
                metadata.first_user_message = Some(message);
            }
        }
    }
    let has_meaningful_provider_title = metadata
        .title
        .as_deref()
        .is_some_and(|title| !is_absent_session_title(title));
    metadata.visibility =
        if has_internal_origin || (!has_real_user_message && !has_meaningful_provider_title) {
            SessionVisibility::Internal
        } else {
            SessionVisibility::UserVisible
        };
    metadata
}

pub fn claude_transcript_metadata(values: &[Value]) -> SessionMetadata {
    let mut metadata = SessionMetadata::default();
    for value in values {
        if metadata.session_id.is_none() {
            metadata.session_id = string(value, "sessionId").map(str::to_string);
        }
        metadata.cwd = metadata.cwd.or_else(|| cwd(value));
        if metadata
            .title
            .as_deref()
            .is_none_or(is_absent_session_title)
        {
            if let Some(candidate) = claude_provider_title(value) {
                metadata.title = Some(candidate);
            }
        }
        if metadata
            .first_user_message
            .as_deref()
            .is_none_or(|current| first_user_title_candidate(current).is_none())
        {
            if let Some(message) = claude_user_message(value) {
                metadata.first_user_message = Some(message);
            }
        }
    }
    metadata
}

pub fn omp_transcript_metadata(values: &[Value]) -> SessionMetadata {
    let mut title_slot = None;
    let mut header = None;
    for value in values {
        match string(value, "type") {
            Some("title") if title_slot.is_none() && header.is_none() => {
                title_slot = string(value, "title").and_then(excerpt);
            }
            Some("session") if header.is_none() => header = Some(value),
            Some(_) | None => {}
        }
    }
    let Some(header) = header else {
        return SessionMetadata::default();
    };
    SessionMetadata {
        session_id: string(header, "id").map(str::to_string),
        cwd: cwd(header),
        title: title_slot.or_else(|| title(header)),
        created_at_unix_ms: string(header, "timestamp").and_then(parse_iso8601_millis),
        first_user_message: None,
        visibility: SessionVisibility::UserVisible,
    }
}

/// Grok Build persists one `summary.json` per session directory under
/// `~/.grok/sessions/<urlencode(cwd)>/<session-id>/`.
pub fn grok_summary_metadata(value: &Value) -> SessionMetadata {
    let info = value.get("info");
    let session_id = info
        .and_then(|info| string(info, "id"))
        .or_else(|| string(value, "id"))
        .map(str::to_string);
    let cwd = info
        .and_then(|info| string(info, "cwd"))
        .or_else(|| string(value, "cwd"))
        .map(str::to_string);
    let title = [
        string(value, "generated_title"),
        string(value, "session_summary"),
        string(value, "title"),
    ]
    .into_iter()
    .flatten()
    .find_map(|text| {
        let candidate = excerpt(text)?;
        (!is_absent_session_title(&candidate)).then_some(candidate)
    });
    SessionMetadata {
        session_id,
        cwd,
        title,
        first_user_message: None,
        created_at_unix_ms: string(value, "created_at").and_then(parse_iso8601_millis),
        visibility: SessionVisibility::UserVisible,
    }
}

pub fn grok_summary_updated_at_unix_ms(value: &Value) -> Option<u64> {
    string(value, "updated_at")
        .or_else(|| string(value, "last_active_at"))
        .and_then(parse_iso8601_millis)
}

/// Gemini CLI persists chats under `~/.gemini/tmp/<slug>/chats/session-*.jsonl`.
pub fn gemini_header_metadata(value: &Value) -> SessionMetadata {
    SessionMetadata {
        session_id: string(value, "sessionId").map(str::to_string),
        cwd: None,
        title: None,
        first_user_message: None,
        created_at_unix_ms: string(value, "startTime").and_then(parse_iso8601_millis),
        visibility: SessionVisibility::UserVisible,
    }
}

pub fn gemini_updated_at_unix_ms(value: &Value) -> Option<u64> {
    string(value, "lastUpdated")
        .or_else(|| value.get("$set").and_then(|set| string(set, "lastUpdated")))
        .and_then(parse_iso8601_millis)
}

/// First meaningful Gemini user text, skipping `<session_context>` bootstrap blobs.
pub fn gemini_first_user_excerpt(values: &[Value]) -> Option<String> {
    for value in values {
        if let Some(excerpt) = gemini_user_excerpt(value) {
            return Some(excerpt);
        }
        if let Some(messages) = value
            .get("$set")
            .and_then(|set| set.get("messages"))
            .and_then(|messages| messages.as_array())
        {
            for message in messages {
                if let Some(excerpt) = gemini_user_excerpt(message) {
                    return Some(excerpt);
                }
            }
        }
    }
    None
}

fn gemini_user_excerpt(value: &Value) -> Option<String> {
    let kind = string(value, "type").unwrap_or("");
    if !kind.eq_ignore_ascii_case("user") {
        return None;
    }
    let text = gemini_content_text(value)?;
    if is_gemini_session_context(&text) {
        return None;
    }
    excerpt(&text)
}

fn gemini_content_text(value: &Value) -> Option<String> {
    if let Some(text) = string(value, "content") {
        return Some(text.to_owned());
    }
    let parts = value.get("content")?.as_array()?;
    let mut joined = String::new();
    for part in parts {
        if let Some(text) = string(part, "text") {
            if !joined.is_empty() {
                joined.push(' ');
            }
            joined.push_str(text);
        }
    }
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn is_gemini_session_context(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("<session_context>")
        || trimmed.contains("This is the Gemini CLI. We are setting up the context")
}

pub(super) fn parse_iso8601_millis(timestamp: &str) -> Option<u64> {
    let bytes = timestamp.as_bytes();
    if bytes.len() < 20 || bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    let year: i32 = timestamp.get(0..4)?.parse().ok()?;
    let month: u32 = timestamp.get(5..7)?.parse().ok()?;
    let day: u32 = timestamp.get(8..10)?.parse().ok()?;
    let hour: u32 = timestamp.get(11..13)?.parse().ok()?;
    let minute: u32 = timestamp.get(14..16)?.parse().ok()?;
    let second: u32 = timestamp.get(17..19)?.parse().ok()?;
    let millis = timestamp
        .get(19..)?
        .strip_prefix('.')
        .and_then(|tail| tail.strip_suffix('Z'))
        .map(|fraction| {
            let digits: String = fraction.chars().take(3).collect();
            format!("{digits:0<3}").parse::<u32>().ok()
        })
        .flatten()
        .unwrap_or(0);
    let days = days_from_civil(year, month, day)?;
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second))?;
    u64::try_from(seconds)
        .ok()?
        .checked_mul(1_000)?
        .checked_add(u64::from(millis))
}

fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let adjusted_year = year - i32::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = i64::from(year_of_era) * 365 + i64::from(year_of_era) / 4
        - i64::from(year_of_era) / 100
        + day_of_year;
    Some(i64::from(era) * 146_097 + day_of_era - 719_468)
}
fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str().filter(|s| !s.trim().is_empty())
}

fn nested<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut value = value;
    for key in path {
        value = value.get(*key)?;
    }
    value.as_str().filter(|s| !s.trim().is_empty())
}

fn first(candidates: &[Option<&str>]) -> Option<String> {
    candidates
        .iter()
        .flatten()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .map(str::to_string)
}

fn first_canonical(candidates: &[Option<&str>]) -> Option<String> {
    candidates
        .iter()
        .flatten()
        .find_map(|id| canonical_codex_session_id(id))
}

fn cwd(value: &Value) -> Option<String> {
    first(&[
        string(value, "cwd"),
        string(value, "working_dir"),
        string(value, "workingDirectory"),
        nested(value, &["turn_context", "cwd"]),
        nested(value, &["payload", "cwd"]),
        nested(value, &["metadata", "cwd"]),
    ])
}

fn title(value: &Value) -> Option<String> {
    first(&[
        string(value, "thread_name"),
        string(value, "title"),
        nested(value, &["turn_context", "title"]),
        nested(value, &["payload", "title"]),
        nested(value, &["metadata", "title"]),
    ])
    .and_then(|s| excerpt(&s))
}

fn claude_provider_title(value: &Value) -> Option<String> {
    [
        string(value, "aiTitle"),
        string(value, "title"),
        nested(value, &["turn_context", "title"]),
        nested(value, &["payload", "title"]),
        nested(value, &["metadata", "title"]),
    ]
    .into_iter()
    .flatten()
    .find_map(|text| {
        let candidate = excerpt(text)?;
        (!is_absent_session_title(&candidate)).then_some(candidate)
    })
}

/// Codex writes origin metadata in `session_meta.payload`, while older and
/// index-shaped records may put the same fields at the top level.  These
/// markers are identity/routing evidence, not title candidates.  A subagent
/// marker is authoritative even if the transcript later contains role=user
/// messages generated by the harness.
fn codex_origin_is_internal(value: &Value) -> bool {
    let thread_source = [
        string(value, "thread_source"),
        nested(value, &["payload", "thread_source"]),
    ]
    .into_iter()
    .flatten()
    .any(|source| {
        source.eq_ignore_ascii_case("subagent") || source.eq_ignore_ascii_case("internal")
    });
    if thread_source {
        return true;
    }

    let parent_link = [
        string(value, "parent_thread_id"),
        string(value, "forked_from_id"),
        nested(value, &["payload", "parent_thread_id"]),
        nested(value, &["payload", "forked_from_id"]),
        nested(
            value,
            &[
                "payload",
                "source",
                "subagent",
                "thread_spawn",
                "parent_thread_id",
            ],
        ),
        nested(
            value,
            &["source", "subagent", "thread_spawn", "parent_thread_id"],
        ),
    ]
    .into_iter()
    .flatten()
    .any(|parent| !parent.trim().is_empty());
    if parent_link {
        return true;
    }

    // Some Codex builds omit parent_thread_id but retain the structured
    // subagent spawn envelope.  Presence of that object is sufficient to
    // classify the transcript as an internal execution artifact.
    [
        value.pointer("/payload/source/subagent/thread_spawn"),
        value.pointer("/source/subagent/thread_spawn"),
        value.pointer("/payload/source/subagent"),
        value.pointer("/source/subagent"),
    ]
    .into_iter()
    .flatten()
    .any(Value::is_object)
}

fn codex_user_message(value: &Value) -> Option<String> {
    let raw = if string(value, "type") == Some("event_msg")
        && nested(value, &["payload", "type"]) == Some("user_message")
    {
        nested(value, &["payload", "message"]).map(str::to_owned)
    } else if string(value, "type") == Some("response_item")
        && nested(value, &["payload", "type"]) == Some("message")
        && nested(value, &["payload", "role"]) == Some("user")
    {
        codex_response_item_user_text(value)
    } else {
        None
    };
    raw.filter(|text| !is_injected_codex_user_payload(text))
        .and_then(|text| excerpt(&text))
}

fn codex_response_item_user_text(value: &Value) -> Option<String> {
    let content = value.pointer("/payload/content")?;
    let Value::Array(parts) = content else {
        return None;
    };
    let mut texts = Vec::new();
    for part in parts {
        let kind = string(part, "type");
        if matches!(
            kind,
            Some("input_text") | Some("text") | Some("output_text")
        ) {
            if let Some(text) = string(part, "text") {
                if !is_injected_codex_user_payload(text) {
                    texts.push(text);
                }
            }
        }
    }
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

/// Codex often injects AGENTS.md / environment blobs as role=user before the
/// real turn. Those must not become session titles.
fn is_injected_codex_user_payload(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("# AGENTS.md instructions")
        || trimmed.starts_with("<INSTRUCTIONS>")
        || trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("<recommended_plugins>")
        || trimmed.starts_with("<codex_internal_context")
        || trimmed.starts_with("<system-reminder>")
        || trimmed.contains("\n<INSTRUCTIONS>")
}

fn claude_user_message(value: &Value) -> Option<String> {
    if string(value, "type") != Some("user") {
        return None;
    }
    let content = value
        .pointer("/message/content")
        .or_else(|| value.get("content"))?;
    claude_content_text(content).and_then(|text| excerpt(&text))
}

fn claude_content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => (!text.trim().is_empty()).then(|| text.to_owned()),
        Value::Array(parts) => {
            let mut texts = Vec::new();
            for part in parts {
                let Value::Object(object) = part else {
                    continue;
                };
                let kind = object.get("type").and_then(Value::as_str);
                if kind.is_some_and(|kind| kind != "text") {
                    continue;
                }
                if let Some(text) = object.get("text").and_then(Value::as_str) {
                    if !text.trim().is_empty()
                        && excerpt(text)
                            .is_some_and(|candidate| !is_absent_session_title(&candidate))
                    {
                        texts.push(text.trim());
                    }
                }
            }
            (!texts.is_empty()).then(|| texts.join("\n"))
        }
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned),
        _ => None,
    }
}

fn excerpt(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut chars = line.chars();
    let head: String = chars.by_ref().take(80).collect();
    Some(if chars.next().is_some() {
        format!("{}…", head.trim_end())
    } else {
        head
    })
}

/// Cursor CLI stores one main transcript at
/// `~/.cursor/projects/<slug>/agent-transcripts/<uuid>/<uuid>.jsonl`.
/// Subagent transcripts live under `subagents/` and must not become rows.
pub fn is_cursor_main_transcript(path: &std::path::Path) -> bool {
    if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
        return false;
    }
    let components: Vec<_> = path
        .components()
        .map(|component| component.as_os_str())
        .collect();
    if !components
        .iter()
        .any(|component| *component == "agent-transcripts")
    {
        return false;
    }
    if components.iter().any(|component| *component == "subagents") {
        return false;
    }
    let Some(file_stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    path.parent()
        .and_then(std::path::Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|parent| parent == file_stem)
}

pub fn cursor_session_id_from_path(path: &std::path::Path) -> Option<String> {
    if !is_cursor_main_transcript(path) {
        return None;
    }
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
}

pub fn cursor_project_cwd_from_path(
    projects_root: &std::path::Path,
    transcript_path: &std::path::Path,
) -> Option<String> {
    let relative = transcript_path.strip_prefix(projects_root).ok()?;
    let slug = relative.components().next()?.as_os_str().to_str()?;
    cursor_slug_to_unix_path(slug)
}

fn cursor_slug_to_unix_path(slug: &str) -> Option<String> {
    let slug = slug.trim();
    if slug.is_empty() {
        return None;
    }
    let parts: Vec<&str> = slug.split('-').collect();
    let path = match parts.first().copied()? {
        "Users" if parts.len() > 1 => format!("/Users/{}", parts[1..].join("/")),
        "home" if parts.len() > 1 => format!("/home/{}", parts[1..].join("/")),
        _ => format!("/{}", parts.join("/")),
    };
    Some(path)
}

pub fn strip_cursor_user_query(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix("<user_query>")
        .and_then(|rest| rest.strip_suffix("</user_query>"))
        .map(str::trim)
        .unwrap_or(trimmed);
    first_user_title_candidate(inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_identity_rejects_message_ids_and_accepts_rollout_suffix() {
        assert_eq!(canonical_codex_session_id("msg_123"), None);
        assert_eq!(
            canonical_codex_session_id("rollout-019fa76a-6276-7b03-b302-c640686b2033").as_deref(),
            Some("019fa76a-6276-7b03-b302-c640686b2033")
        );
    }

    #[test]
    fn malformed_jsonl_fails_the_provider_instead_of_returning_a_subset() {
        assert!(parse_jsonl_strict(b"{\"type\":\"ok\"}\nnot-json\n", 10).is_err());
    }

    #[test]
    fn provider_title_wins_over_first_user_message_when_named() {
        let values = vec![
            serde_json::json!({"type":"event_msg","payload":{"type":"user_message","message":"fallback user text"}}),
            serde_json::json!({"type":"response_item","title":"Provider title"}),
        ];
        let metadata = codex_transcript_metadata(&values);
        assert_eq!(metadata.resolved_title(), Some("Provider title"));
    }

    #[test]
    fn catch_all_provider_title_yields_to_first_user_message() {
        let values = vec![
            serde_json::json!({"type":"event_msg","payload":{"type":"user_message","message":"看看这台机器有没有 grok"}}),
            serde_json::json!({"type":"response_item","title":"Agent session"}),
        ];
        let metadata = codex_transcript_metadata(&values);
        assert_eq!(
            metadata.resolved_title(),
            Some("看看这台机器有没有 grok"),
            "catch-all provider titles must not beat first User message"
        );
        let zh = SessionMetadata {
            title: Some("Agent 会话".into()),
            first_user_message: Some("帮我修复 adb".into()),
            ..SessionMetadata::default()
        };
        assert_eq!(zh.resolved_title(), Some("帮我修复 adb"));
    }

    #[test]
    fn catch_all_title_alone_resolves_absent() {
        let metadata = SessionMetadata {
            title: Some("Agent 会话".into()),
            ..SessionMetadata::default()
        };
        assert_eq!(metadata.resolved_title(), None);
        assert!(is_absent_session_title("Agent session"));
        assert!(is_absent_session_title("  Untitled  "));
    }

    #[test]
    fn first_user_message_names_untitled_session() {
        let values = vec![
            serde_json::json!({"type":"event_msg","payload":{"type":"user_message","message":"  First useful request  "}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"user_message","message":"Second request"}}),
        ];
        let metadata = codex_transcript_metadata(&values);
        assert_eq!(metadata.resolved_title(), Some("First useful request"));
    }

    #[test]
    fn codex_response_item_user_message_names_session_after_skipping_injections() {
        // Newer Codex rollouts omit event_msg.user_message and only keep
        // response_item role=user turns; the first is often an AGENTS.md inject.
        let values = vec![
            serde_json::json!({
                "type":"session_meta",
                "payload":{"id":"019fdcef-a254-72e3-8b8f-32c0a443e689","cwd":"/work"}
            }),
            serde_json::json!({
                "type":"response_item",
                "payload":{
                    "type":"message",
                    "role":"user",
                    "content":[{"type":"input_text","text":"# AGENTS.md instructions\n\n<INSTRUCTIONS>\nYou are Codex…\n</INSTRUCTIONS>"}]
                }
            }),
            serde_json::json!({
                "type":"response_item",
                "payload":{
                    "type":"message",
                    "role":"user",
                    "content":[{"type":"input_text","text":"吧 coharu 自带的工作流搬到 ssh gpu 机器上去。并看看本目录最近的编辑记录，在做什么"}]
                }
            }),
            serde_json::json!({
                "type":"response_item",
                "payload":{
                    "type":"message",
                    "role":"user",
                    "content":[{"type":"input_text","text":"<environment_context>\n  <current_date>2026-08-08</current_date>\n</environment_context>"}]
                }
            }),
        ];
        let metadata = codex_transcript_metadata(&values);
        assert_eq!(
            metadata.resolved_title(),
            Some(
                "吧 coharu 自带的工作流搬到 ssh gpu 机器上去。并看看本目录最近的编辑记录，在做什么"
            )
        );
        assert!(
            metadata.title.is_none(),
            "agent never authored a title — user message must name the row"
        );
    }

    #[test]
    fn first_user_title_candidate_rejects_placeholders_and_excerpts_prompt() {
        assert_eq!(
            first_user_title_candidate("  Fix the flaky gate  "),
            Some("Fix the flaky gate".into())
        );
        assert_eq!(first_user_title_candidate("agentty"), None);
        assert_eq!(first_user_title_candidate("Agent 会话"), None);
        assert_eq!(first_user_title_candidate("   \n"), None);
    }

    #[test]
    fn provider_identity_is_never_a_session_alias() {
        let metadata = SessionMetadata::default();
        assert_eq!(metadata.resolved_title(), None);
    }

    #[test]
    fn codex_subagent_transcript_is_marked_internal() {
        let values = vec![
            serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": "01a0065b-122b-7e13-bc86-de099e0945ce",
                    "session_id": "01a000ec-0388-74e0-8bd5-73ad68b9e321",
                    "thread_source": "subagent",
                    "parent_thread_id": "01a005eb-5fc3-7781-9458-2f7625d0d729"
                }
            }),
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "<recommended_plugins>\n..."}]
                }
            }),
        ];
        let metadata = codex_transcript_metadata(&values);
        assert_eq!(metadata.visibility, SessionVisibility::Internal);
        assert!(!metadata.is_user_visible());
    }

    #[test]
    fn codex_nested_subagent_spawn_is_marked_internal() {
        let values = vec![serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": "01a0065b-122b-7e13-bc86-de099e0945ce",
                "source": {
                    "subagent": {
                        "thread_spawn": {
                            "parent_thread_id": "01a005eb-5fc3-7781-9458-2f7625d0d729"
                        }
                    }
                }
            }
        })];
        assert_eq!(
            codex_transcript_metadata(&values).visibility,
            SessionVisibility::Internal
        );
    }

    #[test]
    fn codex_top_level_user_transcript_remains_visible() {
        let values = vec![
            serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": "01a000ec-0388-74e0-8bd5-73ad68b9e321",
                    "thread_source": "user"
                }
            }),
            serde_json::json!({
                "type": "event_msg",
                "payload": {"type": "user_message", "message": "继续修复发现链"}
            }),
        ];
        let metadata = codex_transcript_metadata(&values);
        assert_eq!(metadata.visibility, SessionVisibility::UserVisible);
        assert_eq!(metadata.resolved_title(), Some("继续修复发现链"));
    }

    #[test]
    fn codex_injected_context_is_not_a_first_user_title() {
        let values = vec![
            serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": "01a000ec-0388-74e0-8bd5-73ad68b9e321",
                    "thread_source": "user"
                }
            }),
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "<recommended_plugins>\n..."},
                        {"type": "input_text", "text": "# AGENTS.md instructions\n..."},
                        {"type": "input_text", "text": "<environment_context>\n..."},
                        {"type": "input_text", "text": "<codex_internal_context source=\"goal\">\n..."}
                    ]
                }
            }),
        ];
        let metadata = codex_transcript_metadata(&values);
        assert_eq!(metadata.first_user_message, None);
        assert_eq!(metadata.resolved_title(), None);
        assert_eq!(metadata.visibility, SessionVisibility::Internal);
    }

    #[test]
    fn codex_explicit_provider_title_keeps_markerless_transcript_visible() {
        let values = vec![
            serde_json::json!({
                "type": "session_meta",
                "payload": {"id": "019fa76a-6276-7b03-b302-c640686b2033"}
            }),
            serde_json::json!({"type": "response_item", "title": "Named rollout"}),
        ];
        let metadata = codex_transcript_metadata(&values);
        assert_eq!(metadata.visibility, SessionVisibility::UserVisible);
        assert_eq!(metadata.resolved_title(), Some("Named rollout"));
    }

    #[test]
    fn codex_later_meaningful_title_replaces_placeholder_title() {
        let values = vec![
            serde_json::json!({"type": "response_item", "title": "Agent 会话"}),
            serde_json::json!({"type": "response_item", "title": "Named rollout"}),
        ];
        let metadata = codex_transcript_metadata(&values);
        assert_eq!(metadata.resolved_title(), Some("Named rollout"));
        assert_eq!(metadata.visibility, SessionVisibility::UserVisible);
    }

    #[test]
    fn codex_later_real_user_replaces_placeholder_prompt() {
        let values = vec![
            serde_json::json!({
                "type": "event_msg",
                "payload": {"type": "user_message", "message": "Agent 会话"}
            }),
            serde_json::json!({
                "type": "event_msg",
                "payload": {"type": "user_message", "message": "真实的后续请求"}
            }),
        ];
        let metadata = codex_transcript_metadata(&values);
        assert_eq!(
            metadata.first_user_message.as_deref(),
            Some("真实的后续请求")
        );
        assert_eq!(metadata.resolved_title(), Some("真实的后续请求"));
    }

    #[test]
    fn claude_array_text_blocks_name_session() {
        let values = vec![serde_json::json!({
            "type": "user",
            "sessionId": "claude-array",
            "message": {
                "content": [
                    {"type": "tool_result", "content": "internal tool output"},
                    {"type": "text", "text": "Review the array parser"},
                    {"type": "image", "source": {"media_type": "image/png"}}
                ]
            }
        })];
        let metadata = claude_transcript_metadata(&values);
        assert_eq!(
            metadata.first_user_message.as_deref(),
            Some("Review the array parser")
        );
        assert_eq!(metadata.resolved_title(), Some("Review the array parser"));
    }

    #[test]
    fn claude_placeholder_user_does_not_block_later_real_prompt() {
        let values = vec![
            serde_json::json!({
                "type": "user",
                "sessionId": "claude-placeholder-user",
                "message": {"content": "Agent 会话"}
            }),
            serde_json::json!({
                "type": "user",
                "message": {"content": [{"type": "text", "text": "真实 Claude 请求"}]}
            }),
        ];
        let metadata = claude_transcript_metadata(&values);
        assert_eq!(metadata.resolved_title(), Some("真实 Claude 请求"));
        assert_eq!(
            metadata.first_user_message.as_deref(),
            Some("真实 Claude 请求")
        );
    }

    #[test]
    fn claude_placeholder_provider_title_does_not_block_meaningful_title() {
        let values = vec![
            serde_json::json!({
                "type": "assistant",
                "sessionId": "claude-placeholder-provider",
                "aiTitle": "Agent session"
            }),
            serde_json::json!({
                "type": "assistant",
                "title": "Investigate Claude parser"
            }),
        ];
        let metadata = claude_transcript_metadata(&values);
        assert_eq!(metadata.resolved_title(), Some("Investigate Claude parser"));
    }

    #[test]
    fn grok_placeholder_title_does_not_block_meaningful_summary() {
        let metadata = grok_summary_metadata(&serde_json::json!({
            "info": {"id": "grok-placeholder"},
            "generated_title": "Agent session",
            "session_summary": "Investigate the remote reconnect"
        }));
        assert_eq!(
            metadata.resolved_title(),
            Some("Investigate the remote reconnect")
        );
    }

    #[test]
    fn cursor_main_transcript_identity_comes_from_path_and_user_query_text() {
        let path = std::path::Path::new(
            "/home/alice/.cursor/projects/Users-admin-agentty/agent-transcripts/d40014ce-6a82-443d-ab66-5018cd63e4d6/d40014ce-6a82-443d-ab66-5018cd63e4d6.jsonl",
        );
        assert!(is_cursor_main_transcript(path));
        assert_eq!(
            cursor_session_id_from_path(path).as_deref(),
            Some("d40014ce-6a82-443d-ab66-5018cd63e4d6")
        );
        assert_eq!(
            cursor_project_cwd_from_path(
                std::path::Path::new("/home/alice/.cursor/projects"),
                path,
            )
            .as_deref(),
            Some("/Users/admin/agentty")
        );
        assert_eq!(
            strip_cursor_user_query("<user_query>\nFix the flaky gate\n</user_query>").as_deref(),
            Some("Fix the flaky gate")
        );
        let subagent = std::path::Path::new(
            "/home/alice/.cursor/projects/repo/agent-transcripts/parent/subagents/child.jsonl",
        );
        assert!(!is_cursor_main_transcript(subagent));
    }
}
