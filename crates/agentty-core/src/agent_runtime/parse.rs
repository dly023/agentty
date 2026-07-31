use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionMetadata {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub title: Option<String>,
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
        ..Default::default()
    };
    for value in values {
        metadata.cwd = metadata.cwd.or_else(|| cwd(value));
        metadata.title = metadata
            .title
            .or_else(|| title(value))
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
            .or_else(|| string(value, "aiTitle").map(str::to_string))
            .or_else(|| claude_user_message(value));
    }
    metadata
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
    (string(value, "type") == Some("event_msg")
        && nested(value, &["payload", "type"]) == Some("user_message"))
    .then(|| nested(value, &["payload", "message"]))
    .flatten()
    .and_then(excerpt)
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
}
