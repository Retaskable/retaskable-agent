//! The per-session `EngineReader` for Cursor: the final message's own fenced `json` block (or
//! last top-level object) becomes the structured result, checked against the session's schema
//! (`cursor.reader(spec, sessionId)` in Retaskable.com).
use serde_json::Value;

use crate::engines::engine::{EngineReader, SessionResult};

use super::result::{LIMIT_TEXT, assistant_text, check_schema, extract_json, limit_text, result_or, tail, tokens_of};

pub(super) struct CursorReader {
    schema: Value,
    last_text: String,
    /// The latest `session_id` seen on any event, so a crash before a `result` event still
    /// reports the chat Cursor actually opened.
    chat: String,
}

impl CursorReader {
    pub(super) fn new(schema: Value) -> Self {
        Self {
            schema,
            last_text: String::new(),
            chat: String::new(),
        }
    }

    fn session_id(&self, known_session_id: &str) -> String {
        if self.chat.is_empty() {
            known_session_id.to_string()
        } else {
            self.chat.clone()
        }
    }
}

impl EngineReader for CursorReader {
    fn line(&mut self, line: &str, known_session_id: &str) -> Option<SessionResult> {
        let event: Value = serde_json::from_str(line).ok()?;
        if let Some(session_id) = event.get("session_id").and_then(Value::as_str) {
            self.chat = session_id.to_string();
        }
        if event.get("type").and_then(Value::as_str) == Some("assistant") {
            let text = assistant_text(&event);
            if !text.is_empty() {
                self.last_text = text;
            }
        }
        if event.get("type").and_then(Value::as_str) != Some("result") {
            return None;
        }

        let is_error = event.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        let raw_subtype = event.get("subtype").and_then(Value::as_str).unwrap_or("").to_string();
        let raw = event
            .get("result")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.last_text.clone());
        let structured = if is_error { None } else { extract_json(&raw) };
        let errors = match &structured {
            Some(v) => check_schema(&self.schema, v, ""),
            None => Vec::new(),
        };

        let api_429 = event.get("api_error_status").and_then(Value::as_u64) == Some(429);
        let limit_hit = api_429 || (is_error && limit_text(&event));

        let ok = raw_subtype == "success" && !is_error && structured.is_some() && errors.is_empty();
        let subtype = if limit_hit || is_error {
            "error".to_string()
        } else if !errors.is_empty() || structured.is_none() {
            "invalid_result".to_string()
        } else {
            raw_subtype.clone()
        };
        let error = if limit_hit {
            Some(result_or(&event, "usage limit"))
        } else if is_error {
            Some(result_or(&event, &raw_subtype))
        } else if !errors.is_empty() {
            Some(errors.join("; "))
        } else if structured.is_none() {
            Some("No structured output returned".to_string())
        } else {
            None
        };

        Some(SessionResult {
            ok,
            subtype,
            structured: if errors.is_empty() { structured } else { None },
            error,
            cost_usd: None,
            session_id: self.session_id(known_session_id),
            limit_hit,
            signed_out: false,
            context_tokens: None,
            usage: None,
            tokens: tokens_of(event.get("usage")),
        })
    }

    fn no_result(&self, exit_code: Option<i32>, stderr: &str, known_session_id: &str) -> SessionResult {
        let code = exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let error = format!(
            "cursor-agent exited with code {code} and no result. {}",
            tail(stderr.trim(), 1500)
        );
        SessionResult {
            ok: false,
            subtype: "exited".to_string(),
            structured: None,
            error: Some(error),
            cost_usd: None,
            session_id: self.session_id(known_session_id),
            limit_hit: LIMIT_TEXT.is_match(stderr),
            signed_out: false,
            context_tokens: None,
            usage: None,
            tokens: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::engines::engine::Tokens;

    fn schema() -> Value {
        json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]})
    }

    #[test]
    fn reader_returns_the_structured_result_from_the_final_fenced_block() {
        let mut r = CursorReader::new(schema());
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"```json\n{\"ok\":true}\n```\nall done","session_id":"chat-1"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(result.ok);
        assert_eq!(result.structured, Some(json!({"ok": true})));
        assert_eq!(result.cost_usd, None);
        assert_eq!(result.session_id, "chat-1");
    }

    #[test]
    fn reader_names_a_schema_miss_as_invalid_result() {
        let mut r = CursorReader::new(schema());
        let line = r#"{"type":"result","subtype":"success","result":"```json\n{\"ok\":\"yes\"}\n```"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(!result.ok);
        assert_eq!(result.subtype, "invalid_result");
        assert!(result.error.unwrap().contains("ok: must be a boolean"));
    }

    #[test]
    fn reader_reports_no_structured_output_when_the_final_message_has_none() {
        let mut r = CursorReader::new(schema());
        let line = r#"{"type":"result","subtype":"success","result":"I forgot"}"#;
        let result = r.line(line, "known").unwrap();
        assert_eq!(result.error, Some("No structured output returned".to_string()));
    }

    #[test]
    fn reader_marks_limit_hit_from_the_error_text_or_a_429() {
        let mut r = CursorReader::new(schema());
        let line = r#"{"type":"result","subtype":"error","is_error":true,"result":"usage limit reached"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(result.limit_hit);
        assert!(!result.ok);

        let mut r = CursorReader::new(schema());
        let line = r#"{"type":"result","subtype":"error","api_error_status":429,"result":"no"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(result.limit_hit);
    }

    #[test]
    fn reader_falls_back_to_the_last_assistant_text_when_the_result_has_none() {
        let mut r = CursorReader::new(schema());
        r.line(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"```json\n{\"ok\":true}\n```"}]}}"#,
            "known",
        );
        let result = r.line(r#"{"type":"result","subtype":"success"}"#, "known").unwrap();
        assert!(result.ok);
        assert_eq!(result.structured, Some(json!({"ok": true})));
    }

    #[test]
    fn reader_maps_usage_to_tokens_and_never_sets_a_cost() {
        let mut r = CursorReader::new(schema());
        let line = r#"{"type":"result","subtype":"success","result":"```json\n{\"ok\":true}\n```","usage":{"inputTokens":8948,"outputTokens":24,"cacheReadTokens":3968}}"#;
        let result = r.line(line, "known").unwrap();
        assert_eq!(
            result.tokens,
            Some(Tokens {
                input: 8948,
                output: 24
            })
        );
        assert_eq!(result.cost_usd, None);

        let mut r = CursorReader::new(schema());
        let result = r
            .line(r#"{"type":"result","subtype":"success","result":"x"}"#, "known")
            .unwrap();
        assert_eq!(result.tokens, None);
    }

    #[test]
    fn no_result_tails_stderr_on_a_char_boundary_and_flags_a_limit_from_its_text() {
        let r = CursorReader::new(schema());
        let stderr = format!("{}{}", "😀", "a".repeat(1499));
        let result = r.no_result(Some(1), &stderr, "known");
        let err = result.error.unwrap();
        assert!(err.starts_with("cursor-agent exited with code 1 and no result."));
        assert!(err.ends_with(&"a".repeat(1499)), "no half emoji kept");
        assert!(!result.limit_hit);

        let limited = r.no_result(Some(1), "rate limit exceeded", "known");
        assert!(limited.limit_hit);
    }
}
