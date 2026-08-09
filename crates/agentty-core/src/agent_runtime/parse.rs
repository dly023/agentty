use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionMetadata {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub first_user_message: Option<String>,
    pub created_at_unix_ms: Option<u64>,
}

impl SessionMetadata {
    pub fn resolved_title(&self) -> Option<&str> {
        meaningful_title(self.title.as_deref())
            .or_else(|| meaningful_title(self.first_user_message.as_deref()))
    }
}

/// Titles that must not beat a first User message or stick as the sole name.
pub fn is_absent_session_title(title: &str) -> bool {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    matches!(trimmed, "Agent 会话" | "新会话" | "未命名会话" | "未命名")
        || matches!(
            lower.as_str(),
            "agent session"
                | "agentty"
                | "untitled"
                | "new session"
                | "unnamed"
                | "unnamed session"
        )
        || lower.starts_with("agentty —")
        || lower.starts_with("agentty -")
}

fn meaningful_title(value: Option<&str>) -> Option<&str> {
    value
        .map(str::trim)
        .filter(|value| !is_absent_session_title(value))
}

/// First-line excerpt suitable as a live/history display title candidate.
/// Returns None for blank or catch-all placeholder prompts.
pub fn first_user_title_candidate(text: &str) -> Option<String> {
    excerpt(text).filter(|value| !is_absent_session_title(value))
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
    for value in values {
        metadata.cwd = metadata.cwd.or_else(|| cwd(value));
        metadata.title = metadata.title.or_else(|| title(value));
        metadata.first_user_message = metadata
            .first_user_message
            .or_else(|| codex_user_message(value));
    }
    metadata
}

pub fn claude_transcript_metadata(values: &[Value]) -> SessionMetadata {
    let mut metadata = SessionMetadata::default();
    for value in values {
        if metadata.session_id.is_none() {
            metadata.session_id = string(value, "sessionId").map(str::to_string);
        }
        metadata.cwd = metadata.cwd.or_else(|| cwd(value));
        metadata.title = metadata
            .title
            .or_else(|| string(value, "aiTitle").and_then(excerpt))
            .or_else(|| title(value));
        metadata.first_user_message = metadata
            .first_user_message
            .or_else(|| claude_user_message(value));
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
    let title = first(&[
        string(value, "generated_title"),
        string(value, "session_summary"),
        string(value, "title"),
    ])
    .and_then(|text| excerpt(&text));
    SessionMetadata {
        session_id,
        cwd,
        title,
        first_user_message: None,
        created_at_unix_ms: string(value, "created_at").and_then(parse_iso8601_millis),
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

fn parse_iso8601_millis(timestamp: &str) -> Option<u64> {
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
                texts.push(text);
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
        || trimmed.contains("\n<INSTRUCTIONS>")
}

fn claude_user_message(value: &Value) -> Option<String> {
    (string(value, "type") == Some("user"))
        .then(|| nested(value, &["message", "content"]))
        .flatten()
        .and_then(excerpt)
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
    fn provider_title_wins_even_when_it_appears_after_first_user_message() {
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
}
