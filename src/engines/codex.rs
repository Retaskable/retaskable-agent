//! The Codex engine: `codex exec --json` with a prompt on stdin, behind the `Engine` interface the
//! runner calls through. Following docs/specs/engines/14-the-codex-engine.md and
//! `agent/src/engines/codex.ts` in Retaskable.com (the ground truth this is porting). The result
//! is the last fenced json block of the final agent message, checked against the same schema as
//! any session.
//!
//! Codex picks its own thread id (`thread.started`), so the runner's transcript id stays the
//! session id and the thread is remembered here, to resume. A follow-up across a restart starts
//! fresh, as pi's does.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use anyhow::Result;
use regex::Regex;
use serde_json::Value;

use super::engine::{
    Engine, EngineId, EngineReader, EventKind, SessionEvent, SessionResult, SessionSpec, SessionStart, Tokens,
};
use super::json_result::{check_schema, extract_json};
use crate::text::{clip, tail};

/// Codex's own thread id for each of our session ids, learnt from `thread.started`, so a resume can
/// find it.
static THREADS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn remember_thread(session_id: &str, thread: &str) {
    if let Ok(mut threads) = THREADS.lock() {
        threads.insert(session_id.to_string(), thread.to_string());
    }
}

fn thread_of(session_id: &str) -> Option<String> {
    THREADS.lock().ok()?.get(session_id).cloned()
}

/// `/usage limit|rate limit|\b429\b/i`: Codex's words for a limit, from a failed turn or a crash's stderr.
static LIMIT_TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)usage limit|rate limit|\b429\b").unwrap());

/// `resolveCodex()`: `RETASKABLE_CODEX` always wins; otherwise the first `codex[.exe]` on `PATH`,
/// falling back to the bare name so a missing Codex is reported by the spawn itself.
pub fn resolve_codex(env: impl Fn(&str) -> Option<String>, windows: bool, path_dirs: &[PathBuf]) -> PathBuf {
    if let Some(over) = env("RETASKABLE_CODEX") {
        return PathBuf::from(over);
    }
    let name = if windows { "codex.exe" } else { "codex" };
    for dir in path_dirs {
        let exe = dir.join(name);
        if exe.is_file() {
            return exe;
        }
    }
    PathBuf::from(name)
}

/// `resolve_codex`, reading `PATH` itself the way the real agent does.
pub fn resolve_codex_on_path(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    let dirs = crate::env_path::path_dirs(&env);
    resolve_codex(&env, cfg!(windows), &dirs)
}

/// The repo's git folder (the main checkout's, for a worktree): a writing session commits, and it's
/// outside the lane (`gitDir`).
fn git_dir(cwd: &Path) -> Option<String> {
    let out = crate::git_bin::command(cwd)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!dir.is_empty()).then_some(dir)
}

/// `codexArgs(spec, git)`. Never the person's `~/.codex/config.toml` (their plugins, MCP servers
/// and model), like Claude's `--setting-sources project`. A read-only session runs in Codex's
/// read-only sandbox; a writing one in workspace-write, with the network (installs, tests) and the
/// git folder (commits). Never `--dangerously-bypass-approvals-and-sandbox`. `thread` is the
/// Codex thread to resume, if any.
fn codex_args(
    spec: &SessionSpec,
    windows: bool,
    git: &dyn Fn(&Path) -> Option<String>,
    thread: Option<&str>,
) -> Vec<String> {
    let mode = if spec.readonly { "read-only" } else { "workspace-write" };
    let mut args: Vec<String> = if thread.is_some() {
        // `exec resume` has no --sandbox flag, so the mode goes in as config there.
        ["exec", "resume", "--json", "-c"].map(String::from).to_vec()
    } else {
        ["exec", "--json", "--sandbox", mode].map(String::from).to_vec()
    };
    if thread.is_some() {
        args.push(format!("sandbox_mode=\"{mode}\""));
    }
    args.push("--ignore-user-config".to_string());
    if windows {
        args.extend(windows_sandbox(&spec.cwd));
    }
    if !spec.readonly {
        args.push("-c".to_string());
        args.push("sandbox_workspace_write.network_access=true".to_string());
        if let Some(dir) = git(&spec.cwd) {
            args.push("--add-dir".to_string());
            args.push(dir);
        }
    }
    if let Some(thread) = thread {
        args.push(thread.to_string());
    }
    args.push("-".to_string());
    args
}

/// `windowsSandbox(cwd)`: without a Windows sandbox Codex refuses to run anything that writes. Its
/// commands run as a sandbox user, so git calls the lane's folder "dubious ownership" unless it's
/// a safe.directory, set through the sandbox's own environment.
fn windows_sandbox(cwd: &Path) -> Vec<String> {
    let abs = std::path::absolute(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let safe = Value::String(abs.to_string_lossy().replace('\\', "/")).to_string();
    vec![
        "-c".to_string(),
        "windows.sandbox=\"elevated\"".to_string(),
        "-c".to_string(),
        format!(
            "shell_environment_policy.set={{GIT_CONFIG_COUNT=\"1\",GIT_CONFIG_KEY_0=\"safe.directory\",GIT_CONFIG_VALUE_0={safe}}}"
        ),
    ]
}

/// `codexPrompt(spec)`: the same result instructions as the other schema-checked engines.
fn codex_prompt(spec: &SessionSpec) -> String {
    format!(
        "{}\n\nWhen you've finished, end your final message with exactly one fenced ```json block holding your result, \
         matching this schema. Nothing after it.\n\n{}",
        spec.prompt, spec.schema,
    )
}

fn event(at: i64, kind: EventKind, text: String) -> Vec<SessionEvent> {
    vec![SessionEvent { at, kind, text }]
}

fn text_of(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or("").to_string()
}

/// `codexSummarize(ev, at)`: messages, a command as it starts, an edit's paths, MCP calls, web
/// searches and the end of a turn.
fn codex_summarize(ev: &Value, at: i64) -> Vec<SessionEvent> {
    let ev_type = ev.get("type").and_then(Value::as_str).unwrap_or("");
    let item = ev.get("item");
    let item_type = item.and_then(|i| i.get("type")).and_then(Value::as_str).unwrap_or("");
    let field = |key: &str| item.and_then(|i| i.get(key));
    match (ev_type, item_type) {
        ("item.completed", "agent_message") => {
            let text = text_of(field("text")).trim().to_string();
            if text.is_empty() {
                Vec::new()
            } else {
                event(at, EventKind::Text, clip(&text, 800))
            }
        }
        ("item.started", "command_execution") => {
            let text = clip(
                &text_of(field("command"))
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                160,
            );
            if text.is_empty() {
                Vec::new()
            } else {
                event(at, EventKind::Tool, text)
            }
        }
        ("item.completed", "file_change") => {
            let paths: Vec<&str> = field("changes")
                .and_then(Value::as_array)
                .map(|changes| {
                    changes
                        .iter()
                        .filter_map(|c| c.get("path").and_then(Value::as_str))
                        .filter(|p| !p.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if paths.is_empty() {
                Vec::new()
            } else {
                event(at, EventKind::Tool, clip(&format!("Edited {}", paths.join(", ")), 160))
            }
        }
        ("item.completed", "mcp_tool_call") => {
            let server = field("server").and_then(Value::as_str).unwrap_or("mcp");
            let tool = field("tool").and_then(Value::as_str).unwrap_or("tool");
            event(at, EventKind::Tool, clip(&format!("{server}.{tool}"), 160))
        }
        ("item.completed", "web_search") => event(
            at,
            EventKind::Tool,
            clip(&format!("Search: {}", text_of(field("query"))), 160),
        ),
        ("turn.completed", _) => event(at, EventKind::Result, "Finished".to_string()),
        ("turn.failed", _) => {
            let message = ev
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("error");
            event(at, EventKind::Result, clip(&format!("Failed: {message}"), 160))
        }
        _ => Vec::new(),
    }
}

/// The per-session `EngineReader` for Codex: the last agent message is read for the structured
/// result at `turn.completed`; `turn.failed` ends it with Codex's own words.
pub struct CodexReader {
    schema: Value,
    last_text: String,
    last_error: String,
}

impl CodexReader {
    pub fn new(schema: Value) -> Self {
        Self {
            schema,
            last_text: String::new(),
            last_error: String::new(),
        }
    }

    fn failed(&self, known_session_id: &str, error: String) -> SessionResult {
        SessionResult {
            ok: false,
            subtype: "error".to_string(),
            structured: None,
            error: Some(format!("codex: {error}")),
            cost_usd: None,
            session_id: known_session_id.to_string(),
            limit_hit: LIMIT_TEXT.is_match(&error),
            signed_out: false,
            context_tokens: None,
            usage: None,
            tokens: None,
        }
    }

    fn completed(&self, ev: &Value, known_session_id: &str) -> SessionResult {
        let structured = extract_json(&self.last_text);
        let errors = structured
            .as_ref()
            .map(|v| check_schema(&self.schema, v, ""))
            .unwrap_or_default();
        let ok = structured.is_some() && errors.is_empty();
        let n = |key: &str| {
            ev.get("usage")
                .and_then(|u| u.get(key))
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v > 0.0)
                .map(|v| v as u64)
                .unwrap_or(0)
        };
        let tokens = ev.get("usage").filter(|u| !u.is_null()).map(|_| Tokens {
            input: n("input_tokens"),
            output: n("output_tokens"),
        });
        SessionResult {
            ok,
            subtype: if ok { "ok" } else { "invalid_result" }.to_string(),
            structured: if errors.is_empty() { structured.clone() } else { None },
            error: if !errors.is_empty() {
                Some(errors.join("; "))
            } else if structured.is_none() {
                Some("No structured output returned".to_string())
            } else {
                None
            },
            cost_usd: None,
            session_id: known_session_id.to_string(),
            limit_hit: false,
            signed_out: false,
            context_tokens: None,
            usage: None,
            tokens,
        }
    }
}

impl EngineReader for CodexReader {
    fn line(&mut self, line: &str, known_session_id: &str) -> Option<SessionResult> {
        let ev: Value = serde_json::from_str(line).ok()?;
        let ev_type = ev.get("type").and_then(Value::as_str).unwrap_or("");
        match ev_type {
            "thread.started" => {
                if let Some(thread) = ev.get("thread_id").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                    remember_thread(known_session_id, thread);
                }
                None
            }
            "item.completed" => {
                let item = ev.get("item");
                if item.and_then(|i| i.get("type")).and_then(Value::as_str) == Some("agent_message") {
                    let text = text_of(item.and_then(|i| i.get("text")));
                    if !text.trim().is_empty() {
                        self.last_text = text;
                    }
                }
                None
            }
            // A top-level `error` can be a reconnect notice; only `turn.failed` ends the turn.
            "error" => {
                self.last_error = ev.get("message").and_then(Value::as_str).unwrap_or("error").to_string();
                None
            }
            "turn.failed" => {
                let error = ev
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        if self.last_error.is_empty() {
                            "error".to_string()
                        } else {
                            self.last_error.clone()
                        }
                    });
                Some(self.failed(known_session_id, error))
            }
            "turn.completed" => Some(self.completed(&ev, known_session_id)),
            _ => None,
        }
    }

    /// The process exited with no `turn.completed` or `turn.failed`: report that as a failure,
    /// naming the exit code, with the end of stderr attached (`noResult`).
    fn no_result(&self, exit_code: Option<i32>, stderr: &str, known_session_id: &str) -> SessionResult {
        let code = exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        SessionResult {
            ok: false,
            subtype: "exited".to_string(),
            structured: None,
            error: Some(format!(
                "codex exited with code {code} and no result. {}",
                tail(stderr.trim(), 1500)
            )),
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

/// The Codex engine: `resolveCodex`, `codexArgs`, `codexPrompt` and the `turn.completed` reader
/// above. It reads no cost and has no budget cap (`can.cost: false`); its process can linger after
/// the result, so the runner ends it a moment after one (`endsOnResult`).
pub struct CodexEngine;

impl Engine for CodexEngine {
    fn id(&self) -> EngineId {
        EngineId::Codex
    }

    fn binary(&self, env: &dyn Fn(&str) -> Option<String>, windows: bool, path_dirs: &[PathBuf]) -> Result<PathBuf> {
        Ok(resolve_codex(env, windows, path_dirs))
    }

    fn args(&self, spec: &SessionSpec) -> Vec<String> {
        let thread = match &spec.start {
            SessionStart::Resume(id) => thread_of(id),
            _ => None,
        };
        codex_args(spec, cfg!(windows), &git_dir, thread.as_deref())
    }

    fn env(&self, _spec: &SessionSpec) -> Vec<(String, String)> {
        Vec::new()
    }

    fn prompt(&self, spec: &SessionSpec) -> String {
        codex_prompt(spec)
    }

    fn reader(&self, spec: &SessionSpec) -> Box<dyn EngineReader> {
        Box::new(CodexReader::new(spec.schema.clone()))
    }

    fn summarize(&self, event: &Value, at: i64) -> Vec<SessionEvent> {
        codex_summarize(event, at)
    }

    fn ends_on_result(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(readonly: bool) -> SessionSpec {
        let mut s = crate::engines::engine::test_spec(Path::new("."), "plan the board");
        s.readonly = readonly;
        s
    }

    fn no_git(_: &Path) -> Option<String> {
        None
    }

    fn schema() -> Value {
        json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]})
    }

    #[test]
    fn resolve_codex_prefers_the_env_then_the_path_then_the_bare_name() {
        let env = |k: &str| (k == "RETASKABLE_CODEX").then(|| "C:/custom/codex.exe".to_string());
        assert_eq!(resolve_codex(env, false, &[]), PathBuf::from("C:/custom/codex.exe"));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("codex"), "").unwrap();
        let none = |_: &str| None;
        assert_eq!(
            resolve_codex(none, false, &[dir.path().to_path_buf()]),
            dir.path().join("codex")
        );
        assert_eq!(resolve_codex(none, false, &[]), PathBuf::from("codex"));
        assert_eq!(resolve_codex(none, true, &[]), PathBuf::from("codex.exe"));
    }

    #[test]
    fn a_read_only_session_runs_in_codexs_read_only_sandbox_never_the_persons_config_no_model_flag() {
        let args = codex_args(&spec(true), false, &no_git, None);
        assert_eq!(args[..4], ["exec", "--json", "--sandbox", "read-only"]);
        assert!(args.contains(&"--ignore-user-config".to_string()));
        assert_eq!(args.last().unwrap(), "-", "the prompt is on stdin");
        assert!(!args.contains(&"-m".to_string()) && !args.contains(&"--model".to_string()));
        assert!(
            !args.iter().any(|a| a.contains("network_access")),
            "no network read-only"
        );
    }

    #[test]
    fn a_writing_session_gets_workspace_write_the_network_and_the_git_folder_never_bypassing_the_sandbox() {
        let git = |_: &Path| Some("/repo/.git".to_string());
        let args = codex_args(&spec(false), false, &git, None);
        assert_eq!(args[2..4], ["--sandbox", "workspace-write"]);
        assert!(args.contains(&"sandbox_workspace_write.network_access=true".to_string()));
        let at = args.iter().position(|a| a == "--add-dir").unwrap();
        assert_eq!(args[at + 1], "/repo/.git");
        assert!(
            !args
                .iter()
                .any(|a| a.contains("dangerously") || a.contains("danger-full-access"))
        );
    }

    #[test]
    fn a_resume_names_the_sandbox_as_config_and_the_thread_before_the_prompt_marker() {
        let args = codex_args(&spec(true), false, &no_git, Some("thread-1"));
        assert_eq!(
            args[..5],
            ["exec", "resume", "--json", "-c", "sandbox_mode=\"read-only\""]
        );
        assert_eq!(args[args.len() - 2..], ["thread-1", "-"]);
        assert!(!args.contains(&"--sandbox".to_string()), "exec resume has no --sandbox");
    }

    #[test]
    fn windows_adds_the_elevated_sandbox_and_the_lanes_folder_as_git_safe_directory() {
        let dir = std::env::temp_dir().join("lane 1");
        let mut s = spec(false);
        s.cwd = dir;
        let args = codex_args(&s, true, &no_git, None);
        let sandbox = args.iter().position(|a| a == "windows.sandbox=\"elevated\"").unwrap();
        let env = &args[sandbox + 2];
        assert!(env.starts_with(
            "shell_environment_policy.set={GIT_CONFIG_COUNT=\"1\",GIT_CONFIG_KEY_0=\"safe.directory\",GIT_CONFIG_VALUE_0=\""
        ));
        assert!(env.ends_with("lane 1\"}"));
        assert!(!env.contains('\\'), "forward slashes only");
        assert!(
            !codex_args(&s, false, &no_git, None)
                .iter()
                .any(|a| a.contains("windows.sandbox"))
        );
    }

    #[test]
    fn the_prompt_ends_with_the_schema_json() {
        let s = spec(true);
        let text = codex_prompt(&s);
        assert!(text.starts_with("plan the board"));
        assert!(text.contains("```json block"));
        assert!(text.ends_with(&s.schema.to_string()));
    }

    #[test]
    fn turn_completed_settles_the_last_agent_messages_json_block_with_its_tokens() {
        let mut r = CodexReader::new(schema());
        assert!(
            r.line(r#"{"type":"thread.started","thread_id":"t-1"}"#, "s-1")
                .is_none()
        );
        r.line(
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"Looking"}}"#,
            "s-1",
        );
        r.line(
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"```json\n{\"ok\":true}\n```"}}"#,
            "s-1",
        );
        let out = r
            .line(
                r#"{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":5,"output_tokens":3}}"#,
                "s-1",
            )
            .unwrap();
        assert!(out.ok);
        assert_eq!(out.structured, Some(json!({"ok": true})));
        assert_eq!(
            out.session_id, "s-1",
            "the session id is the runner's, not Codex's thread"
        );
        assert_eq!(out.tokens, Some(Tokens { input: 10, output: 3 }));
        assert_eq!(out.cost_usd, None);
        assert_eq!(
            thread_of("s-1").as_deref(),
            Some("t-1"),
            "the thread is remembered to resume"
        );
    }

    #[test]
    fn a_final_message_without_a_valid_block_is_invalid_result() {
        let mut r = CodexReader::new(schema());
        r.line(
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"Done."}}"#,
            "s-2",
        );
        let out = r.line(r#"{"type":"turn.completed","usage":{}}"#, "s-2").unwrap();
        assert!(!out.ok);
        assert_eq!(out.subtype, "invalid_result");
        assert_eq!(out.error.as_deref(), Some("No structured output returned"));
        assert_eq!(out.tokens, Some(Tokens { input: 0, output: 0 }));

        let mut r = CodexReader::new(schema());
        r.line(
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"```json\n{\"ok\":\"yes\"}\n```"}}"#,
            "s-2",
        );
        let out = r.line(r#"{"type":"turn.completed"}"#, "s-2").unwrap();
        assert_eq!(out.subtype, "invalid_result");
        assert!(out.structured.is_none());
        assert!(out.error.unwrap().contains("ok: must be a boolean"));
        assert_eq!(out.tokens, None);
    }

    #[test]
    fn turn_failed_reports_codexs_words_and_a_usage_limit_is_a_limit_hit() {
        let mut r = CodexReader::new(schema());
        assert!(
            r.line(r#"{"type":"error","message":"Reconnecting... 1/5"}"#, "s-3")
                .is_none(),
            "a reconnect notice doesn't end the turn"
        );
        let out = r
            .line(
                r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit. Try again later."}}"#,
                "s-3",
            )
            .unwrap();
        assert!(!out.ok);
        assert!(out.limit_hit);
        assert!(out.error.unwrap().starts_with("codex: You've hit your usage limit"));

        // Without its own message, the last top-level error stands in.
        let out = r.line(r#"{"type":"turn.failed"}"#, "s-3").unwrap();
        assert_eq!(out.error.as_deref(), Some("codex: Reconnecting... 1/5"));
        assert!(!out.limit_hit);
    }

    #[test]
    fn a_process_that_exited_without_a_result_names_its_code_and_flags_a_limit_from_stderr() {
        let r = CodexReader::new(schema());
        let out = r.no_result(Some(1), "not logged in", "s-4");
        assert_eq!(out.subtype, "exited");
        assert!(
            out.error
                .unwrap()
                .starts_with("codex exited with code 1 and no result. not logged in")
        );
        assert!(!out.limit_hit);
        assert!(r.no_result(None, "HTTP 429 too many requests", "s-4").limit_hit);
    }

    #[test]
    fn summarize_shows_messages_commands_edits_and_the_end() {
        let s = |v: Value| codex_summarize(&v, 1);
        let text = |v: Value| s(v).into_iter().map(|e| (e.kind, e.text)).collect::<Vec<_>>();
        assert_eq!(
            text(json!({"type": "item.completed", "item": {"type": "agent_message", "text": "Hi"}})),
            [(EventKind::Text, "Hi".to_string())]
        );
        assert_eq!(
            text(json!({"type": "item.started", "item": {"type": "command_execution", "command": "npm  test"}})),
            [(EventKind::Tool, "npm test".to_string())]
        );
        assert_eq!(
            text(
                json!({"type": "item.completed", "item": {"type": "file_change", "changes": [{"path": "a.ts", "kind": "update"}]}})
            ),
            [(EventKind::Tool, "Edited a.ts".to_string())]
        );
        assert_eq!(
            text(
                json!({"type": "item.completed", "item": {"type": "mcp_tool_call", "server": "docs", "tool": "find"}})
            ),
            [(EventKind::Tool, "docs.find".to_string())]
        );
        assert_eq!(
            text(json!({"type": "item.completed", "item": {"type": "web_search", "query": "rust"}})),
            [(EventKind::Tool, "Search: rust".to_string())]
        );
        assert_eq!(
            text(json!({"type": "turn.completed"})),
            [(EventKind::Result, "Finished".to_string())]
        );
        assert_eq!(
            text(json!({"type": "turn.failed", "error": {"message": "boom"}})),
            [(EventKind::Result, "Failed: boom".to_string())]
        );
        assert!(s(json!({"type": "item.completed", "item": {"type": "reasoning", "text": "hmm"}})).is_empty());
        assert!(s(json!({"type": "item.completed", "item": {"type": "agent_message", "text": "  "}})).is_empty());
    }

    #[test]
    fn the_engine_dispatches_through_the_trait_and_ends_on_a_result() {
        let engine = CodexEngine;
        let s = spec(true);
        assert_eq!(engine.id(), EngineId::Codex);
        assert!(engine.ends_on_result());
        assert_eq!(engine.prompt(&s), codex_prompt(&s));
        assert!(engine.env(&s).is_empty());
        assert!(engine.prepare(&s).is_ok());
        assert_eq!(engine.args(&s)[..4], ["exec", "--json", "--sandbox", "read-only"]);

        // A Resume of a session whose thread is known resumes that thread; an unknown one starts fresh.
        remember_thread("known-session", "thread-9");
        let mut resume = spec(true);
        resume.start = SessionStart::Resume("known-session".to_string());
        assert_eq!(engine.args(&resume)[..2], ["exec", "resume"]);
        resume.start = SessionStart::Resume("unknown-session".to_string());
        assert_eq!(engine.args(&resume)[..2], ["exec", "--json"]);
    }
}
