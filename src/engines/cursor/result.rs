//! Turning `cursor-agent`'s own stream-json lines into either a session's structured result
//! (`extractJson`/`checkSchema`, `jsonResult.ts` in Retaskable.com) or a live-transcript line
//! (`summarizeCursor`, engines.md section 8), plus the small text helpers (`clip`/`tail`,
//! `shared/text.ts`) both that and [`super::reader`] need.
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::engines::engine::{EventKind, SessionEvent, Tokens};

const TOOLS: [(&str, &str); 6] = [
    ("shellToolCall", "Bash"),
    ("readToolCall", "Read"),
    ("editToolCall", "Edit"),
    ("writeToolCall", "Write"),
    ("grepToolCall", "Grep"),
    ("globToolCall", "Glob"),
];

/// Why a completed Shell tool call was refused, or none when it ran (`shellDenial`, engines.md
/// section 8). Cursor's allowlist refusals often carry only an empty `reason`.
const EMPTY_DENIAL: &str =
    "not on the allowlist (Shell matches the command's first word only; use a bare allowed program, never a wrapper)";

static FENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)```json\s*(.*?)```").unwrap());

/// The last ` ```json ` block, or the last top-level `{…}` that parses. `None` when there's
/// neither (`extractJson`).
pub(super) fn extract_json(text: &str) -> Option<Value> {
    let fences: Vec<&str> = FENCE.captures_iter(text).map(|c| c.get(1).unwrap().as_str()).collect();
    for fence in fences.iter().rev() {
        if let Ok(v) = serde_json::from_str(fence) {
            return Some(v);
        }
    }
    let objects = top_level_objects(text);
    for object in objects.iter().rev() {
        if let Ok(v) = serde_json::from_str(object) {
            return Some(v);
        }
    }
    None
}

/// Balanced `{…}` spans that start outside any other object (`topLevelObjects`).
fn top_level_objects(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0u32;
    let mut start = None;
    let mut quote = false;
    let mut escape = false;
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if quote {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                quote = false;
            }
            continue;
        }
        if c == '"' {
            quote = true;
        } else if c == '{' {
            if depth == 0 {
                start = Some(i);
            }
            depth += 1;
        } else if c == '}' && depth > 0 {
            depth -= 1;
            if depth == 0
                && let Some(s) = start
            {
                out.push(chars[s..=i].iter().collect());
            }
        }
    }
    out
}

/// Short lines, like "`status`: must be one of pr_opened, needs_clarification, failed"
/// (`checkSchema`): only the keywords a `--json-schema` result schema uses.
pub(super) fn check_schema(schema: &Value, value: &Value, at: &str) -> Vec<String> {
    if !schema.is_object() {
        return Vec::new();
    }
    let where_at = if at.is_empty() { "result" } else { at };
    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array)
        && !enum_values.contains(value)
    {
        let names: Vec<String> = enum_values.iter().map(display_value).collect();
        return vec![format!("{where_at}: must be one of {}", names.join(", "))];
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let Some(obj) = value.as_object() else {
                return vec![format!("{where_at}: must be an object")];
            };
            let mut errors = Vec::new();
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for key in required.iter().filter_map(Value::as_str) {
                    if !obj.contains_key(key) {
                        let key_at = if at.is_empty() {
                            key.to_string()
                        } else {
                            format!("{at}.{key}")
                        };
                        errors.push(format!("{key_at}: is required"));
                    }
                }
            }
            let no_props = json!({});
            let props = schema.get("properties").unwrap_or(&no_props);
            for (key, child) in obj {
                let child_at = if at.is_empty() {
                    key.clone()
                } else {
                    format!("{at}.{key}")
                };
                if let Some(child_schema) = props.get(key) {
                    errors.extend(check_schema(child_schema, child, &child_at));
                } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                    errors.push(format!("{child_at}: is not allowed"));
                }
            }
            errors
        }
        Some("array") => {
            let Some(items) = value.as_array() else {
                return vec![format!("{where_at}: must be an array")];
            };
            match schema.get("items") {
                Some(item_schema) => items
                    .iter()
                    .enumerate()
                    .flat_map(|(i, item)| check_schema(item_schema, item, &format!("{where_at}[{i}]")))
                    .collect(),
                None => Vec::new(),
            }
        }
        Some("string") if !value.is_string() => vec![format!("{where_at}: must be a string")],
        Some("number") if !value.is_number() => vec![format!("{where_at}: must be a number")],
        Some("boolean") if !value.is_boolean() => vec![format!("{where_at}: must be a boolean")],
        _ => Vec::new(),
    }
}

fn display_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `s`, cut to at most `n` chars, with a trailing `…` when it was longer (`clip` in
/// `shared/text.ts`). Duplicated from `summarize.rs`, which stays Claude-only.
fn clip(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    let mut out: String = chars[..n.saturating_sub(1)].iter().collect();
    out.push('…');
    out
}

/// The last (up to) `max_chars` characters of `s`, on a char boundary (`tail` in
/// `shared/text.ts`).
pub(super) fn tail(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        chars[chars.len() - max_chars..].iter().collect()
    }
}

/// The tool name plus its first matching argument, for a transcript drawer (`toolText`).
fn tool_text(ev: &Value) -> String {
    let no_call = json!({});
    let call = ev.get("tool_call").unwrap_or(&no_call);
    let key = call
        .as_object()
        .and_then(|obj| obj.keys().find(|k| k.ends_with("ToolCall")));
    let no_args = json!({});
    let args = key
        .and_then(|k| call.get(k))
        .and_then(|v| v.get("args"))
        .or_else(|| ev.get("args"))
        .unwrap_or(&no_args);
    let name = match key {
        Some(k) => TOOLS
            .iter()
            .find(|(t, _)| t == k)
            .map(|(_, n)| n.to_string())
            .unwrap_or_else(|| k.trim_end_matches("ToolCall").to_string()),
        None => ev.get("name").and_then(Value::as_str).unwrap_or("tool").to_string(),
    };
    let arg = ["command", "path", "file_path", "pattern", "glob_pattern"]
        .iter()
        .find_map(|k| args.get(k).and_then(Value::as_str))
        .unwrap_or("");
    if arg.is_empty() {
        name
    } else {
        let collapsed = arg.split_whitespace().collect::<Vec<_>>().join(" ");
        format!("{name}: {}", clip(&collapsed, 160))
    }
}

/// Why a completed Shell tool call was refused, or `None` when it ran (`shellDenial`). An empty
/// engine reason becomes a concrete allowlist hint.
fn shell_denial(ev: &Value) -> Option<String> {
    let no_call = json!({});
    let call = ev.get("tool_call").unwrap_or(&no_call);
    let shell = call.get("shellToolCall").or_else(|| call.get("ShellToolCall"))?;
    let no_result = json!({});
    let result = shell.get("result").unwrap_or(&no_result);
    let reason = result
        .get("reason")
        .and_then(Value::as_str)
        .or_else(|| result.get("message").and_then(Value::as_str))
        .or_else(|| result.get("error").and_then(Value::as_str))
        .or_else(|| ev.get("reason").and_then(Value::as_str))
        .unwrap_or("")
        .trim();
    let bool_field = |key: &str| result.get(key).and_then(Value::as_bool) == Some(true);
    let str_field_is = |key: &str, want: &str| result.get(key).and_then(Value::as_str) == Some(want);
    let missing_or_null = |key: &str| result.get(key).is_none_or(Value::is_null);
    let denied = bool_field("rejected")
        || bool_field("denied")
        || str_field_is("permission", "deny")
        || str_field_is("permission", "denied")
        || str_field_is("status", "rejected")
        || str_field_is("status", "denied")
        || ev.get("subtype").and_then(Value::as_str) == Some("error")
        || (result.get("reason").is_some()
            && missing_or_null("exitCode")
            && missing_or_null("stdout")
            && missing_or_null("output"));
    if !denied {
        return None;
    }
    Some(if reason.is_empty() {
        EMPTY_DENIAL.to_string()
    } else {
        reason.to_string()
    })
}

/// An `assistant` event's own text blocks, trimmed and joined — shared by `summarize_cursor`'s
/// transcript line below and [`super::reader::CursorReader`]'s own fallback when a `result`
/// event carries no text of its own.
pub(super) fn assistant_text(event: &Value) -> String {
    event
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|c| c.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// `summarizeCursor(ev, at)`: an `assistant` event's text, a started/refused `tool_call`, and a
/// `result` event become the same `SessionEvent` shape Claude's reader produces.
pub(super) fn summarize_cursor(event: &Value, at: i64) -> Vec<SessionEvent> {
    match event.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            let text = assistant_text(event);
            if text.is_empty() {
                Vec::new()
            } else {
                vec![SessionEvent {
                    at,
                    kind: EventKind::Text,
                    text: clip(&text, 800),
                }]
            }
        }
        Some("tool_call") if event.get("subtype").and_then(Value::as_str) == Some("started") => {
            vec![SessionEvent {
                at,
                kind: EventKind::Tool,
                text: tool_text(event),
            }]
        }
        Some("tool_call")
            if matches!(
                event.get("subtype").and_then(Value::as_str),
                Some("completed") | Some("error")
            ) =>
        {
            shell_denial(event)
                .map(|why| {
                    vec![SessionEvent {
                        at,
                        kind: EventKind::Tool,
                        text: format!("Bash refused: {}", clip(&why, 200)),
                    }]
                })
                .unwrap_or_default()
        }
        Some("result") => {
            let done = event.get("subtype").and_then(Value::as_str) == Some("success")
                && event.get("is_error").and_then(Value::as_bool) != Some(true);
            let text = if done {
                "done".to_string()
            } else {
                event.get("subtype").and_then(Value::as_str).unwrap_or("").to_string()
            };
            vec![SessionEvent {
                at,
                kind: EventKind::Result,
                text: format!("Finished: {text}"),
            }]
        }
        _ => Vec::new(),
    }
}

pub(super) static LIMIT_TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)usage limit|rate limit").unwrap());

/// Whether a result event's own words say the usage limit was hit (`limitText`).
pub(super) fn limit_text(ev: &Value) -> bool {
    let result = ev.get("result").and_then(Value::as_str).unwrap_or("");
    let error = ev.get("error").and_then(Value::as_str).unwrap_or("");
    LIMIT_TEXT.is_match(&format!("{result} {error}"))
}

/// A result event's `usage` as the session's tokens, or `None` when it carries no counts
/// (`tokensOf`). Cache counts are left out — they're not part of what the session was billed for.
pub(super) fn tokens_of(usage: Option<&Value>) -> Option<Tokens> {
    let n = |key: &str| -> u64 {
        usage
            .and_then(|u| u.get(key))
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v > 0.0)
            .map(|v| v as u64)
            .unwrap_or(0)
    };
    let input = n("inputTokens");
    let output = n("outputTokens");
    if input > 0 || output > 0 {
        Some(Tokens { input, output })
    } else {
        None
    }
}

pub(super) fn result_or(ev: &Value, default: &str) -> String {
    ev.get("result")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(default)
        .to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // extract_json

    #[test]
    fn extract_json_takes_the_last_fenced_block_then_the_last_top_level_object_then_none() {
        assert_eq!(
            extract_json("notes\n```json\n{\"ok\":false}\n```\nthen\n```json\n{\"ok\":true}\n```\nthanks"),
            Some(json!({"ok": true}))
        );
        assert_eq!(
            extract_json("no fence, just {\"ok\":true} at the end"),
            Some(json!({"ok": true}))
        );
        assert_eq!(extract_json("nothing here"), None);
    }

    // check_schema

    #[test]
    fn check_schema_names_an_enum_miss_and_a_required_miss() {
        let schema = json!({
            "type": "object",
            "properties": {"status": {"enum": ["pr_opened", "failed"]}},
            "required": ["status"],
        });
        assert_eq!(
            check_schema(&schema, &json!({"status": "pr_opened"}), ""),
            Vec::<String>::new()
        );
        assert_eq!(
            check_schema(&schema, &json!({"status": "nope"}), ""),
            vec!["status: must be one of pr_opened, failed".to_string()]
        );
        assert_eq!(
            check_schema(&schema, &json!({}), ""),
            vec!["status: is required".to_string()]
        );
    }

    #[test]
    fn check_schema_names_a_type_mismatch_and_disallows_extras_when_asked() {
        let schema = json!({
            "type": "object",
            "properties": {"ok": {"type": "boolean"}},
            "required": ["ok"],
            "additionalProperties": false,
        });
        let errors = check_schema(&schema, &json!({"ok": "yes"}), "");
        assert!(errors[0].contains("ok: must be a boolean"), "{errors:?}");
        let errors = check_schema(&schema, &json!({"ok": true, "extra": 1}), "");
        assert!(errors.contains(&"extra: is not allowed".to_string()));
    }

    // shell_denial / tool_text

    #[test]
    fn shell_denial_is_none_when_it_ran_and_reports_the_reason_or_the_empty_hint() {
        assert_eq!(
            shell_denial(
                &json!({"type": "tool_call", "subtype": "completed", "tool_call": {"shellToolCall": {"result": {}}}})
            ),
            None
        );
        let denied = shell_denial(&json!({
            "type": "tool_call", "subtype": "completed",
            "tool_call": {"shellToolCall": {"result": {"rejected": true, "reason": ""}}},
        }))
        .unwrap();
        assert!(denied.contains("not on the allowlist"), "{denied}");
        assert_eq!(
            shell_denial(&json!({
                "type": "tool_call", "subtype": "completed",
                "tool_call": {"shellToolCall": {"result": {"rejected": true, "reason": "Shell(rtk) denied"}}},
            })),
            Some("Shell(rtk) denied".to_string())
        );
    }

    #[test]
    fn tool_text_names_the_known_tools_by_their_claude_names() {
        let ev = json!({"tool_call": {"shellToolCall": {"args": {"command": "git status"}}}});
        assert_eq!(tool_text(&ev), "Bash: git status");
    }

    // summarize_cursor

    #[test]
    fn summarize_cursor_reports_assistant_text_a_started_tool_a_refusal_and_the_result() {
        assert_eq!(
            summarize_cursor(&json!({"type": "system", "subtype": "init", "session_id": "chat-1"}), 1),
            Vec::new()
        );
        let started = summarize_cursor(
            &json!({"type": "tool_call", "subtype": "started", "tool_call": {"shellToolCall": {"args": {"command": "git status"}}}}),
            1,
        );
        assert_eq!(started[0].text, "Bash: git status");

        assert_eq!(
            summarize_cursor(
                &json!({"type": "tool_call", "subtype": "completed", "tool_call": {"shellToolCall": {"result": {}}}}),
                1
            ),
            Vec::new()
        );
        let refused = summarize_cursor(
            &json!({"type": "tool_call", "subtype": "completed", "tool_call": {"shellToolCall": {"result": {"reason": ""}}}}),
            1,
        );
        assert!(
            refused[0].text.starts_with("Bash refused: not on the allowlist"),
            "{:?}",
            refused[0]
        );

        assert_eq!(
            summarize_cursor(&json!({"type": "result", "subtype": "success"}), 9)[0].text,
            "Finished: done"
        );
    }
}
