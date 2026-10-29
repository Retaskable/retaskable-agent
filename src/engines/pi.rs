//! The pi engine: `pi --mode json --no-session` with a prompt on stdin, behind the `Engine`
//! interface the runner calls through. Following docs/specs/engines/13-the-pi-engine.md and
//! `agent/src/engines/pi.ts` in Retaskable.com (the ground truth this is porting). Pi can't be
//! pointed at a chosen session id (`--session <id>` only resumes one that already exists), so it
//! always runs with `--no-session`: the runner's own transcript file is the session's identity,
//! and a follow-up starts a fresh pi session rather than resuming one.
use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde_json::{Value, json};

use super::engine::{Engine, EngineId, EngineReader, EventKind, SessionEvent, SessionResult, SessionSpec};
use super::json_result::{check_schema, extract_json};
use crate::text::{clip, collapse_and_clip, tail};

/// Pi's own words for a usage or rate limit, from a crash's stderr (`/usage limit|rate limit/i`
/// in `pi.ts`'s `noResult`).
static LIMIT_TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)usage limit|rate limit").unwrap());

/// `resolvePi()`: `RETASKABLE_PI` always wins; otherwise the first `pi[.exe]` on `PATH`. Never
/// errors (unlike `resolve_claude`): a missing pi isn't fatal, so this falls back to the bare name
/// and lets the spawn itself report "not installed".
pub fn resolve_pi(env: impl Fn(&str) -> Option<String>, windows: bool, path_dirs: &[PathBuf]) -> PathBuf {
    if let Some(over) = env("RETASKABLE_PI") {
        return PathBuf::from(over);
    }
    let name = if windows { "pi.exe" } else { "pi" };
    for dir in path_dirs {
        let exe = dir.join(name);
        if exe.is_file() {
            return exe;
        }
    }
    PathBuf::from(name)
}

/// `resolve_pi`, reading `PATH` itself the way the real agent does.
pub fn resolve_pi_on_path(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    let dirs = crate::env_path::path_dirs(&env);
    resolve_pi(&env, cfg!(windows), &dirs)
}

/// `piArgs(spec)`: json mode, no persistent session, the chosen model, and the read-only tool set
/// for a role that only reads. Never touches `spec.start` — pi has no resume to point at.
fn pi_args(spec: &SessionSpec) -> Vec<String> {
    let mut args = vec![
        "--mode".to_string(),
        "json".to_string(),
        "--no-session".to_string(),
        "--model".to_string(),
        spec.model.clone(),
    ];
    if spec.readonly {
        args.push("--tools".to_string());
        args.push("read,grep,find,ls".to_string());
    }
    args
}

/// `piPrompt(spec)`: the same result instructions as Cursor's own (section 5) — pi has no
/// `--json-schema` either, so the schema is spelled out and the result must be the final
/// message's own fenced block.
fn pi_prompt(spec: &SessionSpec) -> String {
    format!(
        "{}\n\nWhen you've finished, end your final message with exactly one fenced ```json block holding your result, \
         matching this schema. Nothing after it.\n\n{}",
        spec.prompt, spec.schema,
    )
}

/// The assistant message's text: pi sends it as a string or as an array of content blocks
/// (`piText`).
fn pi_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The tool's first matching argument alone, or the tool's own name when it has none
/// (`piSummarize`'s tool branch — unlike Cursor's `toolText`, pi's transcript line carries no
/// "name: " prefix).
fn pi_tool_text(ev: &Value) -> String {
    let no_args = json!({});
    let args = ev.get("args").unwrap_or(&no_args);
    let arg = ["command", "path", "pattern"]
        .iter()
        .find_map(|k| args.get(k).and_then(Value::as_str))
        .unwrap_or("");
    if arg.is_empty() {
        ev.get("toolName").and_then(Value::as_str).unwrap_or("tool").to_string()
    } else {
        collapse_and_clip(arg, 160)
    }
}

/// `piSummarize(ev, at)`: an assistant `message_end`'s text, a tool call's start/end, a retry, and
/// the run's own end, for reading a stored transcript back.
fn pi_summarize(event: &Value, at: i64) -> Vec<SessionEvent> {
    let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
    if event_type == "message_end"
        && event.get("message").and_then(|m| m.get("role")).and_then(Value::as_str) == Some("assistant")
    {
        let text = event
            .get("message")
            .and_then(|m| m.get("content"))
            .map(pi_text)
            .unwrap_or_default();
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![SessionEvent {
                at,
                kind: EventKind::Text,
                text: clip(&text, 800),
            }]
        };
    }
    if event_type == "tool_execution_start" || event_type == "tool_execution_end" {
        return vec![SessionEvent {
            at,
            kind: EventKind::Tool,
            text: pi_tool_text(event),
        }];
    }
    if event_type == "auto_retry_start" {
        return vec![SessionEvent {
            at,
            kind: EventKind::Result,
            text: "Retrying".to_string(),
        }];
    }
    if event_type == "agent_end" {
        let text = if event.get("willRetry").and_then(Value::as_bool) == Some(true) {
            "Retrying"
        } else {
            "Finished"
        };
        return vec![SessionEvent {
            at,
            kind: EventKind::Result,
            text: text.to_string(),
        }];
    }
    Vec::new()
}

/// The per-session `EngineReader` for pi: `agent_settled` is pi's own end-of-run marker (it has no
/// `result` event), so the last assistant message's own text is read for the structured result
/// once that arrives.
pub struct PiReader {
    schema: Value,
    last_text: String,
    last_error: String,
}

impl PiReader {
    pub fn new(schema: Value) -> Self {
        Self {
            schema,
            last_text: String::new(),
            last_error: String::new(),
        }
    }
}

impl EngineReader for PiReader {
    fn line(&mut self, line: &str, known_session_id: &str) -> Option<SessionResult> {
        let event: Value = serde_json::from_str(line).ok()?;
        let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
        if event_type == "message_end"
            && event.get("message").and_then(|m| m.get("role")).and_then(Value::as_str) == Some("assistant")
        {
            let message = event.get("message");
            let text = message.and_then(|m| m.get("content")).map(pi_text).unwrap_or_default();
            if !text.trim().is_empty() {
                self.last_text = text;
            }
            // A failed model call ends as an assistant message with stopReason "error" and no content.
            self.last_error = if message.and_then(|m| m.get("stopReason")).and_then(Value::as_str) == Some("error") {
                message
                    .and_then(|m| m.get("errorMessage"))
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_string()
            } else {
                String::new()
            };
        }
        // `agent_settled` is pi's own end-of-run marker (it has no `result` event). The last
        // assistant message carries the final text; use it even if it was an error response.
        if event_type != "agent_settled" {
            return None;
        }

        let structured = extract_json(&self.last_text);
        let errors = match &structured {
            Some(v) => check_schema(&self.schema, v, ""),
            None => Vec::new(),
        };
        let ok = structured.is_some() && errors.is_empty();
        let subtype = if errors.is_empty() && structured.is_some() {
            "ok".to_string()
        } else {
            "invalid_result".to_string()
        };
        let error = if !errors.is_empty() {
            Some(errors.join("; "))
        } else if structured.is_none() {
            Some(if self.last_error.is_empty() {
                "No structured output returned".to_string()
            } else {
                format!("pi: {}", self.last_error)
            })
        } else {
            None
        };

        Some(SessionResult {
            ok,
            subtype,
            structured: if errors.is_empty() { structured } else { None },
            error,
            cost_usd: None,
            session_id: known_session_id.to_string(),
            limit_hit: false,
            signed_out: false,
            context_tokens: None,
            usage: None,
            tokens: None,
        })
    }

    /// The process exited with no `agent_settled` marker at all: report that as a failure, naming
    /// the exit code, with the end of stderr attached (`noResult`).
    fn no_result(&self, exit_code: Option<i32>, stderr: &str, known_session_id: &str) -> SessionResult {
        let code = exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let error = format!(
            "pi exited with code {code} and no result. {}",
            tail(stderr.trim(), 1500)
        );
        SessionResult {
            ok: false,
            subtype: "exited".to_string(),
            structured: None,
            error: Some(error),
            cost_usd: None,
            session_id: known_session_id.to_string(),
            limit_hit: LIMIT_TEXT.is_match(stderr),
            signed_out: false,
            context_tokens: None,
            usage: None,
            tokens: None,
        }
    }
}

/// The pi engine: `resolvePi`, `piArgs`, `piPrompt` and the `agent_settled` reader above, behind
/// the `Engine` interface the runner calls through. Pi has no isolate config to prepare, so
/// `prepare` stays the trait's default no-op.
pub struct PiEngine;

impl Engine for PiEngine {
    fn id(&self) -> EngineId {
        EngineId::Pi
    }

    fn binary(&self, env: &dyn Fn(&str) -> Option<String>, windows: bool, path_dirs: &[PathBuf]) -> Result<PathBuf> {
        Ok(resolve_pi(env, windows, path_dirs))
    }

    fn args(&self, spec: &SessionSpec) -> Vec<String> {
        pi_args(spec)
    }

    fn env(&self, _spec: &SessionSpec) -> Vec<(String, String)> {
        Vec::new()
    }

    fn prompt(&self, spec: &SessionSpec) -> String {
        pi_prompt(spec)
    }

    fn reader(&self, spec: &SessionSpec) -> Box<dyn EngineReader> {
        Box::new(PiReader::new(spec.schema.clone()))
    }

    fn summarize(&self, event: &Value, at: i64) -> Vec<SessionEvent> {
        pi_summarize(event, at)
    }
}

#[cfg(test)]
fn base_spec(cwd: &std::path::Path) -> SessionSpec {
    super::engine::test_spec(cwd, "plan the board")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::engine::SessionStart;

    // resolve_pi / resolve_pi_on_path

    #[test]
    fn resolve_pi_prefers_the_env_override_on_any_platform() {
        let env = |k: &str| (k == "RETASKABLE_PI").then(|| "C:/custom/pi.exe".to_string());
        assert_eq!(resolve_pi(env, true, &[]), PathBuf::from("C:/custom/pi.exe"));
        assert_eq!(resolve_pi(env, false, &[]), PathBuf::from("C:/custom/pi.exe"));
    }

    #[test]
    fn resolve_pi_finds_it_on_the_path_or_falls_back_to_the_bare_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pi"), "").unwrap();
        let no_env = |_: &str| None;
        assert_eq!(
            resolve_pi(no_env, false, &[dir.path().to_path_buf()]),
            dir.path().join("pi")
        );
        assert_eq!(resolve_pi(no_env, false, &[]), PathBuf::from("pi"));
        assert_eq!(resolve_pi(no_env, true, &[]), PathBuf::from("pi.exe"));
    }

    #[test]
    fn resolve_pi_on_path_reads_dirs_from_the_path_env() {
        let dir = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "pi.exe" } else { "pi" };
        std::fs::write(dir.path().join(name), "").unwrap();
        let path_value = dir.path().to_string_lossy().into_owned();
        let env = move |k: &str| (k == "PATH").then(|| path_value.clone());
        assert_eq!(resolve_pi_on_path(env), dir.path().join(name));
    }

    // pi_args / pi_prompt

    #[test]
    fn pi_args_never_touches_start_and_adds_read_only_tools_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = base_spec(dir.path());
        spec.start = SessionStart::Resume("ignored".to_string());
        spec.readonly = true;
        let args = pi_args(&spec);
        assert_eq!(
            args,
            vec![
                "--mode",
                "json",
                "--no-session",
                "--model",
                "sonnet",
                "--tools",
                "read,grep,find,ls"
            ]
        );

        let mut writing = base_spec(dir.path());
        writing.readonly = false;
        assert!(!pi_args(&writing).contains(&"--tools".to_string()));
    }

    #[test]
    fn pi_prompt_ends_with_the_schema_json() {
        let dir = tempfile::tempdir().unwrap();
        let spec = base_spec(dir.path());
        let text = pi_prompt(&spec);
        assert!(text.contains("```json block"));
        assert!(text.ends_with(&spec.schema.to_string()));
    }

    // pi_text / pi_tool_text / pi_summarize

    #[test]
    fn pi_text_reads_a_plain_string_or_the_text_blocks_of_an_array() {
        assert_eq!(pi_text(&json!("hello")), "hello");
        assert_eq!(
            pi_text(
                &json!([{"type": "text", "text": " hi "}, {"type": "tool_use"}, {"type": "text", "text": "there"}])
            ),
            "hi\nthere"
        );
        assert_eq!(pi_text(&json!(null)), "");
    }

    #[test]
    fn pi_summarize_reports_assistant_text_a_tool_call_a_retry_and_the_run_end() {
        assert_eq!(
            pi_summarize(
                &json!({"type": "message_end", "message": {"role": "assistant", "content": "hi"}}),
                1
            )[0]
            .text,
            "hi"
        );
        assert_eq!(
            pi_summarize(
                &json!({"type": "message_end", "message": {"role": "user", "content": "hi"}}),
                1
            ),
            Vec::new()
        );
        assert_eq!(
            pi_summarize(
                &json!({"type": "tool_execution_start", "toolName": "Read", "args": {"path": "src/mod.rs"}}),
                1
            )[0]
            .text,
            "src/mod.rs"
        );
        assert_eq!(
            pi_summarize(&json!({"type": "tool_execution_start", "toolName": "Bash"}), 1)[0].text,
            "Bash"
        );
        assert_eq!(
            pi_summarize(&json!({"type": "auto_retry_start"}), 1)[0].text,
            "Retrying"
        );
        assert_eq!(
            pi_summarize(&json!({"type": "agent_end", "willRetry": true}), 1)[0].text,
            "Retrying"
        );
        assert_eq!(pi_summarize(&json!({"type": "agent_end"}), 1)[0].text, "Finished");
    }

    // PiReader::line / agent_settled

    #[test]
    fn reader_returns_the_structured_result_once_agent_settled_arrives() {
        let mut r =
            PiReader::new(json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]}));
        assert!(
            r.line(
                r#"{"type":"message_end","message":{"role":"assistant","content":"```json\n{\"ok\":true}\n```"}}"#,
                "known"
            )
            .is_none()
        );
        let result = r.line(r#"{"type":"agent_settled"}"#, "known").unwrap();
        assert!(result.ok);
        assert_eq!(result.subtype, "ok");
        assert_eq!(result.structured, Some(json!({"ok": true})));
        assert_eq!(result.session_id, "known");
        assert_eq!(result.error, None);
    }

    #[test]
    fn reader_names_a_schema_miss_as_invalid_result() {
        let mut r =
            PiReader::new(json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]}));
        r.line(
            r#"{"type":"message_end","message":{"role":"assistant","content":"```json\n{\"ok\":\"yes\"}\n```"}}"#,
            "known",
        );
        let result = r.line(r#"{"type":"agent_settled"}"#, "known").unwrap();
        assert!(!result.ok);
        assert_eq!(result.subtype, "invalid_result");
        assert_eq!(result.structured, None);
        assert!(result.error.unwrap().contains("ok: must be a boolean"));
    }

    #[test]
    fn reader_reports_no_structured_output_when_the_final_message_has_none() {
        let mut r = PiReader::new(json!({"type": "object"}));
        r.line(
            r#"{"type":"message_end","message":{"role":"assistant","content":"I forgot"}}"#,
            "known",
        );
        let result = r.line(r#"{"type":"agent_settled"}"#, "known").unwrap();
        assert!(!result.ok);
        assert_eq!(result.error, Some("No structured output returned".to_string()));
    }

    #[test]
    fn reader_reports_a_failed_model_call_as_a_pi_error() {
        let mut r = PiReader::new(json!({"type": "object"}));
        r.line(
            r#"{"type":"message_end","message":{"role":"assistant","stopReason":"error","errorMessage":"rate limited upstream"}}"#,
            "known",
        );
        let result = r.line(r#"{"type":"agent_settled"}"#, "known").unwrap();
        assert_eq!(result.error, Some("pi: rate limited upstream".to_string()));
        assert!(
            !result.limit_hit,
            "agent_settled never flags the usage limit, only a crash's stderr does"
        );
    }

    #[test]
    fn reader_keeps_the_last_non_empty_text_across_several_assistant_messages() {
        let mut r = PiReader::new(json!({"type": "object"}));
        r.line(
            r#"{"type":"message_end","message":{"role":"assistant","content":"```json\n{}\n```"}}"#,
            "known",
        );
        r.line(
            r#"{"type":"message_end","message":{"role":"assistant","content":""}}"#,
            "known",
        );
        let result = r.line(r#"{"type":"agent_settled"}"#, "known").unwrap();
        assert!(result.ok, "an empty follow-up message doesn't erase the earlier result");
    }

    #[test]
    fn reader_ignores_lines_until_agent_settled() {
        let mut r = PiReader::new(json!({"type": "object"}));
        assert!(
            r.line(r#"{"type":"tool_execution_start","toolName":"Read"}"#, "known")
                .is_none()
        );
        assert!(r.line(r#"{"type":"agent_end","willRetry":true}"#, "known").is_none());
    }

    // PiReader::no_result

    #[test]
    fn no_result_names_the_exit_code_tails_stderr_and_flags_a_limit_from_its_text() {
        let r = PiReader::new(json!({"type": "object"}));
        let long_stderr = format!("  {}  ", "x".repeat(2000));
        let result = r.no_result(Some(1), &long_stderr, "known");
        assert!(!result.ok);
        assert_eq!(result.subtype, "exited");
        assert_eq!(result.session_id, "known");
        let err = result.error.unwrap();
        assert!(err.starts_with("pi exited with code 1 and no result."));
        assert!(err.ends_with(&"x".repeat(1500)));
        assert!(!result.limit_hit);

        let limited = r.no_result(Some(1), "rate limit exceeded", "known");
        assert!(limited.limit_hit);
    }

    #[test]
    fn no_result_names_an_unknown_exit_code() {
        let r = PiReader::new(json!({"type": "object"}));
        let result = r.no_result(None, "", "known");
        assert!(
            result
                .error
                .unwrap()
                .starts_with("pi exited with code unknown and no result.")
        );
    }

    // PiEngine dispatch

    #[test]
    fn engine_dispatches_id_args_prompt_and_reader_through_the_trait() {
        let dir = tempfile::tempdir().unwrap();
        let engine = PiEngine;
        let spec = base_spec(dir.path());
        assert_eq!(engine.id(), EngineId::Pi);
        assert_eq!(engine.args(&spec), pi_args(&spec));
        assert_eq!(engine.env(&spec), Vec::<(String, String)>::new());
        assert_eq!(engine.prompt(&spec), pi_prompt(&spec));
        assert!(engine.prepare(&spec).is_ok());
        let mut reader = engine.reader(&spec);
        reader.line(
            r#"{"type":"message_end","message":{"role":"assistant","content":"```json\n{}\n```"}}"#,
            "known",
        );
        assert!(reader.line(r#"{"type":"agent_settled"}"#, "known").unwrap().ok);
    }
}
