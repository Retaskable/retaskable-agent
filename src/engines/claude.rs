//! The Claude Code engine: finding its binary, the `claude -p` invocation for a session, and
//! reading its `stream-json` output back into a `SessionResult`. Following docs/specs/agent/
//! 06.2-implementer.md, docs/specs/agent/04-architecture.md, docs/specs/agent/07-orchestrator-loop.md
//! section 7.1, docs/specs/claude-signed-out.md section 3.1, docs/specs/engines/02-the-engine-layer.md,
//! and `agent/src/engines/claude.ts` / `engine.ts` / `noDeploy.ts` in Retaskable.com (the ground
//! truth this is porting). Milestone 2 of docs/specs/agent/11-build-milestones.md: spawn `claude -p`
//! with `--output-format stream-json`, parse the events, capture the final structured result,
//! resume a session that already has history, read the 5-hour usage limit out of the stream, and
//! classify a signed-out Claude (`signed_out` / `describe_failure`).
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use super::engine::{
    Engine, EngineId, EngineReader, SessionResult, SessionSpec, SessionStart, is_signed_out, signed_out_text,
};

/// `noDeploy.ts`: the fake Cloudflare token every session runs with, so a deploy command fails
/// instead of actually shipping anything.
const NO_DEPLOY_TOKEN: &str = "retaskable-sessions-never-deploy";

/// `noDeploy.ts` `NO_DEPLOY_TOOLS`: Bash patterns blocked on every session, belt-and-braces
/// alongside the fake token above.
const NO_DEPLOY_TOOLS: [&str; 8] = [
    "Bash(*wrangler deploy*)",
    "Bash(*wrangler versions deploy*)",
    "Bash(*wrangler publish*)",
    "Bash(*wrangler secret put*)",
    "Bash(*run deploy*)",
    "Bash(*deploy:production*)",
    "Bash(*deploy:staging*)",
    "Bash(*git push*production*)",
];

/// `claude.ts` `COMPACT_AT`: how many tokens of context Claude Code is told to auto-compact at.
const COMPACT_AT: &str = "200000";

/// `resolveClaude()`: finds the real `claude` binary to run, following docs/specs/agent/
/// 04-architecture.md's "Finding Claude" — on Windows the `claude` on PATH is an npm shim batch
/// file, so this looks behind it for the `claude.exe` it wraps, since spawning a `.cmd` directly
/// from a piped child process is unreliable. `RETASKABLE_CLAUDE` always wins when set.
pub fn resolve_claude(env: impl Fn(&str) -> Option<String>, windows: bool, path_dirs: &[PathBuf]) -> Result<PathBuf> {
    if let Some(over) = env("RETASKABLE_CLAUDE") {
        return Ok(PathBuf::from(over));
    }
    if !windows {
        return Ok(PathBuf::from("claude"));
    }
    for dir in path_dirs {
        let exe = dir.join("claude.exe");
        if exe.is_file() {
            return Ok(exe);
        }
        let shim = dir.join("claude.cmd");
        if shim.is_file() {
            let behind = dir
                .join("node_modules")
                .join("@anthropic-ai")
                .join("claude-code")
                .join("bin")
                .join("claude.exe");
            if behind.is_file() {
                return Ok(behind);
            }
        }
    }
    anyhow::bail!("claude.exe wasn't found on PATH behind the npm shim. Set RETASKABLE_CLAUDE to its full path.")
}

/// `resolve_claude`, reading PATH itself the way the real agent does.
pub fn resolve_claude_on_path(env: impl Fn(&str) -> Option<String>) -> Result<PathBuf> {
    let path = env("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    resolve_claude(env, cfg!(windows), &dirs)
}

/// `buildArgs(spec)`: the full `claude -p` argument list for one session. The prompt itself isn't
/// here — docs/specs/agent/04-architecture.md says it goes in on stdin, to stay clear of Windows's
/// command-line length limit.
fn build_args(spec: &SessionSpec) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        "--model".to_string(),
        spec.model.clone(),
        "--permission-mode".to_string(),
        "auto".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--json-schema".to_string(),
        spec.schema.to_string(),
    ];
    if spec.budget_usd > 0.0 {
        args.push("--max-budget-usd".to_string());
        args.push(spec.budget_usd.to_string());
    }
    if let Some(max_turns) = spec.max_turns {
        args.push("--max-turns".to_string());
        args.push(max_turns.to_string());
    }
    args.push("--strict-mcp-config".to_string());
    let mcp_config = spec.cwd.join(".mcp.json");
    if mcp_config.is_file() {
        args.push("--mcp-config".to_string());
        args.push(mcp_config.to_string_lossy().into_owned());
    }
    args.push("--setting-sources".to_string());
    args.push("project".to_string());
    match &spec.start {
        SessionStart::Ephemeral => args.push("--no-session-persistence".to_string()),
        SessionStart::Fresh(id) => {
            args.push("--session-id".to_string());
            args.push(id.clone());
        }
        SessionStart::Resume(id) => {
            args.push("--resume".to_string());
            args.push(id.clone());
        }
    }
    args.push("--disallowedTools".to_string());
    for tool in NO_DEPLOY_TOOLS {
        args.push(tool.to_string());
    }
    args
}

/// `sessionEnv(model)`: environment variables layered on top of the agent's own for one session —
/// no MCP servers from claude.ai, a known auto-compact window, subagents on sonnet (haiku for a
/// haiku-tier session, to keep a cheap session cheap), and `noDeploy.ts`'s fake deploy token.
fn session_env(model: &str) -> Vec<(String, String)> {
    let subagent_model = if model == "haiku" { "haiku" } else { "sonnet" };
    vec![
        ("ENABLE_CLAUDEAI_MCP_SERVERS".to_string(), "false".to_string()),
        ("CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(), COMPACT_AT.to_string()),
        ("CLAUDE_CODE_SUBAGENT_MODEL".to_string(), subagent_model.to_string()),
        ("CLOUDFLARE_API_TOKEN".to_string(), NO_DEPLOY_TOKEN.to_string()),
        ("CF_API_TOKEN".to_string(), NO_DEPLOY_TOKEN.to_string()),
    ]
}

/// Reads a session's `stream-json` events one line at a time, following `claude.ts`'s `reader()`.
/// Tracks the running session's own context size as it goes (a subagent's turns, which carry a
/// `parent_tool_use_id`, don't count), and turns the final `result` event into a `SessionResult`.
#[derive(Default)]
pub struct SessionReader {
    /// `spec.model === "local"`: a local server signs nothing in and has no Anthropic usage limit.
    local: bool,
    context_tokens: Option<u64>,
    /// The latest `rate_limit_info` seen so far (section 7.1: "the latest one is kept"); a
    /// session can report it more than once, one per turn.
    usage: Option<Value>,
}

impl SessionReader {
    pub fn new(local: bool) -> Self {
        Self {
            local,
            ..Self::default()
        }
    }

    fn usage_rejected(&self) -> bool {
        self.usage
            .as_ref()
            .and_then(|u| u.get("status"))
            .and_then(Value::as_str)
            == Some("rejected")
    }

    fn result_of(&self, event: &Value, known_session_id: &str) -> SessionResult {
        let is_error = event.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        let raw_subtype = event
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let structured = event.get("structured_output").filter(|v| !v.is_null()).cloned();
        let session_id = event
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or(known_session_id)
            .to_string();
        // Claude Code prices local tokens at Anthropic's rates; they cost nothing.
        let cost_usd = if self.local {
            0.0
        } else {
            event.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0)
        };

        // `claude.ts`: ok needs subtype "success", and an is_error+success pair becomes subtype "error".
        let ok = raw_subtype == "success" && !is_error && structured.is_some();
        let subtype = if is_error && raw_subtype == "success" {
            "error".to_string()
        } else {
            raw_subtype.clone()
        };
        // Error text uses the event's own subtype (`ev.result || ev.subtype`), not the rewritten
        // "error" label above — so is_error+success with no result string falls back to "success".
        let error = if is_error {
            event
                .get("errors")
                .and_then(Value::as_array)
                .filter(|errors| !errors.is_empty())
                .map(|errors| errors.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; "))
                .or_else(|| {
                    event
                        .get("result")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                })
                .or(Some(raw_subtype))
        } else if structured.is_none() {
            Some("No structured output returned".to_string())
        } else {
            None
        };

        // A local server has no usage limit; a 429 from it is not a reason to wait for a reset.
        let api_429 = event.get("api_error_status").and_then(Value::as_u64) == Some(429);
        let limit_hit = !self.local && (api_429 || (self.usage_rejected() && is_error));
        // Its sign-in ran out (claude-signed-out.md section 3.1). A local server signs nothing in.
        let signed_out = !self.local && !limit_hit && is_signed_out(ok, error.as_deref(), None);

        SessionResult {
            ok,
            subtype,
            structured,
            error,
            cost_usd,
            session_id,
            limit_hit,
            signed_out,
            context_tokens: self.context_tokens,
            usage: self.usage.clone(),
        }
    }
}

impl EngineReader for SessionReader {
    /// Feeds one line of `stream-json` output. Returns `Some(result)` once a `result` event is
    /// seen (the session is done); every other event returns `None`, having updated tracked state
    /// as a side effect.
    fn line(&mut self, line: &str, known_session_id: &str) -> Option<SessionResult> {
        let event: Value = serde_json::from_str(line).ok()?;
        match event.get("type").and_then(Value::as_str)? {
            // Real stream-json main-thread turns carry `"parent_tool_use_id": null`; JS treats
            // that as falsy (`!ev.parent_tool_use_id`), so null and missing both count.
            "assistant" if event.get("parent_tool_use_id").is_none_or(Value::is_null) => {
                if let Some(c) = context_of(&event) {
                    self.context_tokens = Some(c);
                }
                None
            }
            // section 7.1: every session's stream starts with one of these, and a later turn can
            // send another; the latest replaces whatever came before.
            "rate_limit_event" => {
                if let Some(info) = event.get("rate_limit_info").filter(|v| !v.is_null()).cloned() {
                    self.usage = Some(info);
                }
                None
            }
            "result" => Some(self.result_of(&event, known_session_id)),
            _ => None,
        }
    }

    /// The process exited with no `result` event at all (a crash, or killed before it could
    /// finish): report that as a failure naming the exit code, with the end of stderr attached.
    fn no_result(&self, exit_code: Option<i32>, stderr: &str, known_session_id: &str) -> SessionResult {
        let code = exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let error = format!(
            "claude exited with code {code} and no result. {}",
            tail(stderr.trim(), 1500)
        );
        let limit_hit = !self.local && self.usage_rejected();
        let signed_out = !self.local && !limit_hit && signed_out_text(Some(&error));
        SessionResult {
            ok: false,
            subtype: "exited".to_string(),
            structured: None,
            error: Some(error),
            cost_usd: 0.0,
            session_id: known_session_id.to_string(),
            limit_hit,
            signed_out,
            context_tokens: self.context_tokens,
            usage: self.usage.clone(),
        }
    }
}

/// `contextOf(event)`: a turn's own context size — the tokens it was given to read, not the
/// tokens it wrote — from an `assistant` event's usage block. Returns `None` unless
/// `input_tokens` is a number, matching `claude.ts` so a turn with no usage keeps the previous
/// context size.
fn context_of(event: &Value) -> Option<u64> {
    let usage = event.get("message")?.get("usage")?;
    let input = usage.get("input_tokens")?.as_u64()?;
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
    Some(input + field("cache_read_input_tokens") + field("cache_creation_input_tokens"))
}

/// The last (up to) `max_chars` characters of `s`, on a char boundary.
fn tail(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        chars[chars.len() - max_chars..].iter().collect()
    }
}

/// The Claude Code engine: `resolveClaude`, `buildArgs`, `sessionEnv` and the `stream-json` reader
/// above, behind the `Engine` interface the runner calls through.
pub struct ClaudeEngine;

impl Engine for ClaudeEngine {
    fn id(&self) -> EngineId {
        EngineId::Claude
    }

    fn binary(&self, env: &dyn Fn(&str) -> Option<String>, windows: bool, path_dirs: &[PathBuf]) -> Result<PathBuf> {
        resolve_claude(env, windows, path_dirs).context("resolving the claude binary")
    }

    fn args(&self, spec: &SessionSpec) -> Vec<String> {
        build_args(spec)
    }

    fn env(&self, spec: &SessionSpec) -> Vec<(String, String)> {
        session_env(&spec.model)
    }

    fn prompt(&self, spec: &SessionSpec) -> String {
        spec.prompt.clone()
    }

    fn reader(&self, spec: &SessionSpec) -> Box<dyn EngineReader> {
        Box::new(SessionReader::new(spec.model == "local"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;

    fn base_spec(cwd: &Path) -> SessionSpec {
        SessionSpec {
            engine: None,
            model: "sonnet".to_string(),
            prompt: "do the thing".to_string(),
            schema: json!({"type": "object"}),
            cwd: cwd.to_path_buf(),
            max_turns: None,
            budget_usd: 0.0,
            start: SessionStart::Ephemeral,
        }
    }

    // resolve_claude / resolve_claude_on_path

    #[test]
    fn resolve_claude_prefers_the_env_override_on_any_platform() {
        let env = |k: &str| (k == "RETASKABLE_CLAUDE").then(|| "C:/custom/claude.exe".to_string());
        assert_eq!(
            resolve_claude(env, true, &[]).unwrap(),
            PathBuf::from("C:/custom/claude.exe")
        );
        assert_eq!(
            resolve_claude(env, false, &[]).unwrap(),
            PathBuf::from("C:/custom/claude.exe")
        );
    }

    #[test]
    fn resolve_claude_is_plain_claude_off_windows() {
        let no_env = |_: &str| None;
        assert_eq!(resolve_claude(no_env, false, &[]).unwrap(), PathBuf::from("claude"));
    }

    #[test]
    fn resolve_claude_finds_claude_exe_directly_on_the_path_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("claude.exe"), "").unwrap();
        let no_env = |_: &str| None;
        let got = resolve_claude(no_env, true, &[dir.path().to_path_buf()]).unwrap();
        assert_eq!(got, dir.path().join("claude.exe"));
    }

    #[test]
    fn resolve_claude_goes_behind_the_npm_shim_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("claude.cmd"), "").unwrap();
        let behind = dir
            .path()
            .join("node_modules")
            .join("@anthropic-ai")
            .join("claude-code")
            .join("bin");
        std::fs::create_dir_all(&behind).unwrap();
        std::fs::write(behind.join("claude.exe"), "").unwrap();
        let no_env = |_: &str| None;
        let got = resolve_claude(no_env, true, &[dir.path().to_path_buf()]).unwrap();
        assert_eq!(got, behind.join("claude.exe"));
    }

    #[test]
    fn resolve_claude_fails_naming_the_fix_when_nothing_matches_on_windows() {
        let no_env = |_: &str| None;
        let err = resolve_claude(no_env, true, &[]).unwrap_err();
        assert!(err.to_string().contains("RETASKABLE_CLAUDE"));
    }

    #[test]
    fn resolve_claude_on_path_reads_dirs_from_the_path_env_on_windows() {
        if !cfg!(windows) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("claude.exe"), "").unwrap();
        let path_value = dir.path().to_string_lossy().into_owned();
        let env = move |k: &str| (k == "PATH").then(|| path_value.clone());
        assert_eq!(resolve_claude_on_path(env).unwrap(), dir.path().join("claude.exe"));
    }

    // build_args

    #[test]
    fn build_args_has_the_documented_flags() {
        let dir = tempfile::tempdir().unwrap();
        let args = build_args(&base_spec(dir.path()));
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "--model");
        assert_eq!(args[2], "sonnet");
        for flag in [
            "--permission-mode",
            "--output-format",
            "--verbose",
            "--strict-mcp-config",
            "--setting-sources",
            "--no-session-persistence",
            "--disallowedTools",
        ] {
            assert!(args.contains(&flag.to_string()), "missing {flag} in {args:?}");
        }
        for absent in ["--max-budget-usd", "--max-turns", "--mcp-config", "--session-id"] {
            assert!(!args.contains(&absent.to_string()), "unexpected {absent} in {args:?}");
        }
    }

    #[test]
    fn build_args_never_omits_the_no_deploy_tools() {
        let dir = tempfile::tempdir().unwrap();
        let args = build_args(&base_spec(dir.path()));
        let idx = args.iter().position(|a| a == "--disallowedTools").unwrap();
        for tool in NO_DEPLOY_TOOLS {
            assert!(args[idx..].contains(&tool.to_string()));
        }
    }

    #[test]
    fn build_args_adds_the_budget_cap_only_when_positive() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = base_spec(dir.path());
        spec.budget_usd = 5.0;
        let args = build_args(&spec);
        let idx = args.iter().position(|a| a == "--max-budget-usd").unwrap();
        assert_eq!(args[idx + 1], "5");
    }

    #[test]
    fn build_args_adds_max_turns_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = base_spec(dir.path());
        spec.max_turns = Some(300);
        let args = build_args(&spec);
        let idx = args.iter().position(|a| a == "--max-turns").unwrap();
        assert_eq!(args[idx + 1], "300");
    }

    #[test]
    fn build_args_points_at_the_projects_mcp_config_only_when_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let spec = base_spec(dir.path());
        assert!(!build_args(&spec).contains(&"--mcp-config".to_string()));
        std::fs::write(dir.path().join(".mcp.json"), "{}").unwrap();
        let args = build_args(&spec);
        let idx = args.iter().position(|a| a == "--mcp-config").unwrap();
        assert_eq!(args[idx + 1], dir.path().join(".mcp.json").to_string_lossy());
    }

    #[test]
    fn build_args_starts_a_named_session_instead_of_no_persistence_when_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = base_spec(dir.path());
        spec.start = SessionStart::Fresh("abc-123".to_string());
        let args = build_args(&spec);
        assert!(!args.contains(&"--no-session-persistence".to_string()));
        assert!(!args.contains(&"--resume".to_string()));
        let idx = args.iter().position(|a| a == "--session-id").unwrap();
        assert_eq!(args[idx + 1], "abc-123");
    }

    #[test]
    fn build_args_resumes_with_the_sessions_id_instead_of_no_persistence_or_a_fresh_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = base_spec(dir.path());
        spec.start = SessionStart::Resume("abc-123".to_string());
        let args = build_args(&spec);
        assert!(!args.contains(&"--no-session-persistence".to_string()));
        assert!(!args.contains(&"--session-id".to_string()));
        let idx = args.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(args[idx + 1], "abc-123");
    }

    #[test]
    fn build_args_keeps_every_other_flag_the_same_on_a_resume() {
        let dir = tempfile::tempdir().unwrap();
        let mut fresh = base_spec(dir.path());
        fresh.start = SessionStart::Fresh("abc-123".to_string());
        let mut resumed = base_spec(dir.path());
        resumed.start = SessionStart::Resume("abc-123".to_string());
        let without_start = |args: Vec<String>| -> Vec<String> {
            let idx = args
                .iter()
                .position(|a| ["--session-id", "--resume", "--no-session-persistence"].contains(&a.as_str()))
                .unwrap();
            let skip = if args[idx] == "--no-session-persistence" { 1 } else { 2 };
            [&args[..idx], &args[idx + skip..]].concat()
        };
        assert_eq!(without_start(build_args(&fresh)), without_start(build_args(&resumed)));
    }

    // session_env

    #[test]
    fn session_env_runs_subagents_on_sonnet_by_default_and_haiku_for_a_haiku_session() {
        assert!(session_env("sonnet").contains(&("CLAUDE_CODE_SUBAGENT_MODEL".to_string(), "sonnet".to_string())));
        assert!(session_env("opus").contains(&("CLAUDE_CODE_SUBAGENT_MODEL".to_string(), "sonnet".to_string())));
        assert!(session_env("haiku").contains(&("CLAUDE_CODE_SUBAGENT_MODEL".to_string(), "haiku".to_string())));
    }

    #[test]
    fn session_env_disables_claude_ai_connectors_and_sets_the_compact_window() {
        let env = session_env("sonnet");
        assert!(env.contains(&("ENABLE_CLAUDEAI_MCP_SERVERS".to_string(), "false".to_string())));
        assert!(env.contains(&("CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(), "200000".to_string())));
    }

    #[test]
    fn session_env_overwrites_the_cloudflare_token_so_nothing_can_deploy() {
        let env = session_env("sonnet");
        assert!(
            env.iter()
                .any(|(k, v)| k == "CLOUDFLARE_API_TOKEN" && v == NO_DEPLOY_TOKEN)
        );
        assert!(env.iter().any(|(k, v)| k == "CF_API_TOKEN" && v == NO_DEPLOY_TOKEN));
    }

    // SessionReader::line / result_of

    #[test]
    fn reader_returns_the_structured_result_from_a_successful_run() {
        let mut r = SessionReader::new(false);
        let line = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"pr_opened"},"total_cost_usd":2.5,"session_id":"abc"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(result.ok);
        assert_eq!(result.subtype, "success");
        assert_eq!(result.structured, Some(json!({"status": "pr_opened"})));
        assert_eq!(result.cost_usd, 2.5);
        assert_eq!(result.session_id, "abc");
        assert_eq!(result.error, None);
    }

    #[test]
    fn reader_falls_back_to_the_known_session_id_when_the_event_has_none() {
        let mut r = SessionReader::new(false);
        let line = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{}}"#;
        let result = r.line(line, "known-id").unwrap();
        assert_eq!(result.session_id, "known-id");
    }

    #[test]
    fn reader_reports_missing_structured_output_as_an_error() {
        let mut r = SessionReader::new(false);
        let line =
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":null,"session_id":"abc"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(!result.ok);
        assert_eq!(result.error, Some("No structured output returned".to_string()));
    }

    #[test]
    fn reader_joins_the_errors_array_when_the_run_failed() {
        let mut r = SessionReader::new(false);
        let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["No conversation found with session ID: x"],"session_id":"abc"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(!result.ok);
        assert_eq!(result.subtype, "error_during_execution");
        assert_eq!(
            result.error,
            Some("No conversation found with session ID: x".to_string())
        );
    }

    #[test]
    fn reader_falls_back_to_the_result_string_when_there_is_no_errors_array() {
        let mut r = SessionReader::new(false);
        let line = r#"{"type":"result","subtype":"success","is_error":true,"result":"boom","session_id":"abc"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(!result.ok);
        assert_eq!(result.subtype, "error");
        assert_eq!(result.error, Some("boom".to_string()));

        // No result string either: error text keeps the event's own subtype, while the field
        // itself is rewritten to "error".
        let line = r#"{"type":"result","subtype":"success","is_error":true,"session_id":"abc"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(!result.ok);
        assert_eq!(result.subtype, "error");
        assert_eq!(result.error, Some("success".to_string()));
    }

    #[test]
    fn reader_falls_back_to_the_subtype_when_a_failed_run_has_no_errors_or_result_string() {
        let mut r = SessionReader::new(false);
        let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"abc"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(!result.ok);
        assert_eq!(result.subtype, "error_during_execution");
        assert_eq!(result.error, Some("error_during_execution".to_string()));
    }

    #[test]
    fn reader_requires_subtype_success_for_ok_even_with_structured_output() {
        let mut r = SessionReader::new(false);
        let line = r#"{"type":"result","subtype":"error_max_turns","is_error":false,"structured_output":{"status":"pr_opened"},"session_id":"abc"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(!result.ok);
        assert_eq!(result.subtype, "error_max_turns");
        assert_eq!(result.structured, Some(json!({"status": "pr_opened"})));
        assert_eq!(result.error, None);
    }

    #[test]
    fn reader_ignores_non_result_events() {
        let mut r = SessionReader::new(false);
        assert!(r.line(r#"{"type":"system","subtype":"init"}"#, "known").is_none());
    }

    #[test]
    fn reader_tracks_context_tokens_from_the_sessions_own_turns_only() {
        let mut r = SessionReader::new(false);
        r.line(
            r#"{"type":"assistant","parent_tool_use_id":"tool-1","message":{"usage":{"input_tokens":999999}}}"#,
            "known",
        );
        let before_any_turn = r
            .line(
                r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#,
                "known",
            )
            .unwrap();
        assert_eq!(before_any_turn.context_tokens, None);

        // Real stream-json main-thread turns carry null, not a missing field.
        r.line(
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"usage":{"input_tokens":100,"cache_read_input_tokens":20,"cache_creation_input_tokens":5}}}"#,
            "known",
        );
        let after_a_turn = r
            .line(
                r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#,
                "known",
            )
            .unwrap();
        assert_eq!(after_a_turn.context_tokens, Some(125));

        // A later turn with no usage (or usage without input_tokens) keeps the previous value.
        r.line(
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{}}"#,
            "known",
        );
        r.line(
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"usage":{"cache_read_input_tokens":50}}}"#,
            "known",
        );
        let still_previous = r
            .line(
                r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#,
                "known",
            )
            .unwrap();
        assert_eq!(still_previous.context_tokens, Some(125));
    }

    #[test]
    fn reader_keeps_the_latest_rate_limit_info_as_usage() {
        let mut r = SessionReader::new(false);
        r.line(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1000,"rateLimitType":"five_hour"}}"#,
            "known",
        );
        r.line(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":2000,"rateLimitType":"five_hour"}}"#,
            "known",
        );
        let result = r
            .line(
                r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#,
                "known",
            )
            .unwrap();
        assert_eq!(
            result.usage,
            Some(json!({"status": "allowed_warning", "resetsAt": 2000, "rateLimitType": "five_hour"}))
        );
    }

    #[test]
    fn reader_reports_no_usage_when_the_stream_never_sent_a_rate_limit_event() {
        let mut r = SessionReader::new(false);
        let result = r
            .line(
                r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#,
                "known",
            )
            .unwrap();
        assert_eq!(result.usage, None);
    }

    #[test]
    fn reader_ignores_a_rate_limit_event_with_no_info() {
        let mut r = SessionReader::new(false);
        assert!(
            r.line(r#"{"type":"rate_limit_event","rate_limit_info":null}"#, "known")
                .is_none()
        );
        let result = r
            .line(
                r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#,
                "known",
            )
            .unwrap();
        assert_eq!(result.usage, None);
    }

    #[test]
    fn no_result_still_carries_the_usage_seen_before_the_crash() {
        let mut r = SessionReader::new(false);
        r.line(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#,
            "known",
        );
        let result = r.no_result(Some(1), "", "known");
        assert_eq!(result.usage, Some(json!({"status": "rejected"})));
    }

    // SessionReader::no_result / tail

    #[test]
    fn no_result_names_the_exit_code_and_tails_the_trimmed_stderr() {
        let r = SessionReader::new(false);
        let long_stderr = format!("  {}  ", "x".repeat(2000));
        let result = r.no_result(Some(1), &long_stderr, "known");
        assert!(!result.ok);
        assert_eq!(result.subtype, "exited");
        assert_eq!(result.session_id, "known");
        let err = result.error.unwrap();
        assert!(err.starts_with("claude exited with code 1 and no result."));
        // Prefix + space + 1500 of the trimmed body; no leading/trailing spaces from stderr.
        assert!(err.ends_with(&"x".repeat(1500)));
        assert!(!err.contains("  x"));
        assert!(err.len() < long_stderr.len());
    }

    #[test]
    fn tail_keeps_only_the_last_n_characters() {
        assert_eq!(tail("hello world", 5), "world");
        assert_eq!(tail("hi", 10), "hi");
    }

    // signed_out / describe_failure (claude-signed-out.md section 3.1)

    fn error_result_line(extra: &str) -> String {
        format!(r#"{{"type":"result","subtype":"success","is_error":true,{extra},"session_id":"s1"}}"#)
    }

    #[test]
    fn reader_marks_a_run_out_sign_in_as_signed_out() {
        let mut r = SessionReader::new(false);
        let result = r
            .line(
                &error_result_line(
                    r#""result":"Failed to authenticate: OAuth session expired and could not be refreshed""#,
                ),
                "s1",
            )
            .unwrap();
        assert!(result.signed_out);
        assert!(!result.limit_hit);

        let mut r = SessionReader::new(false);
        let result = r
            .line(
                r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["Not logged in · Please run /login"],"session_id":"s1"}"#,
                "s1",
            )
            .unwrap();
        assert!(result.signed_out);

        let mut r = SessionReader::new(false);
        let result = r
            .line(
                &error_result_line(
                    r#""result":"API Error: 401 {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\"}}""#,
                ),
                "s1",
            )
            .unwrap();
        assert!(result.signed_out);

        let result = SessionReader::new(false).no_result(Some(1), "Invalid API key · Please run /login", "s1");
        assert!(result.signed_out, "from stderr");
    }

    #[test]
    fn reader_does_not_mark_a_limit_budget_ordinary_error_or_local_as_signed_out() {
        let mut r = SessionReader::new(false);
        let result = r
            .line(
                r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":429,"result":"limit","session_id":"s1"}"#,
                "s1",
            )
            .unwrap();
        assert!(result.limit_hit);
        assert!(!result.signed_out);

        let mut r = SessionReader::new(false);
        let result = r
            .line(
                r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"session_id":"s1"}"#,
                "s1",
            )
            .unwrap();
        assert!(!result.signed_out);

        let mut r = SessionReader::new(false);
        let result = r
            .line(
                r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["Tool failed"],"session_id":"s1"}"#,
                "s1",
            )
            .unwrap();
        assert!(!result.signed_out);

        let result = SessionReader::new(true).no_result(Some(1), "Not logged in", "s1");
        assert!(!result.signed_out, "a local server signs nothing in");
        assert!(!result.limit_hit);
    }

    #[test]
    fn reader_marks_limit_hit_from_a_rejected_rate_limit_on_error() {
        let mut r = SessionReader::new(false);
        r.line(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#,
            "s1",
        );
        let result = r
            .line(
                r#"{"type":"result","subtype":"success","is_error":true,"result":"rate limited","session_id":"s1"}"#,
                "s1",
            )
            .unwrap();
        assert!(result.limit_hit);
        assert!(!result.signed_out);

        let mut r = SessionReader::new(false);
        r.line(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#,
            "s1",
        );
        let exited = r.no_result(Some(1), "", "s1");
        assert!(exited.limit_hit);
        assert!(!exited.signed_out);
    }

    #[test]
    fn claude_engine_dispatches_args_env_prompt_and_reader_through_the_trait() {
        let dir = tempfile::tempdir().unwrap();
        let engine = ClaudeEngine;
        let spec = base_spec(dir.path());
        assert_eq!(engine.id(), EngineId::Claude);
        assert_eq!(engine.args(&spec), build_args(&spec));
        assert_eq!(engine.env(&spec), session_env(&spec.model));
        assert_eq!(engine.prompt(&spec), spec.prompt);
        let mut reader = engine.reader(&spec);
        let line =
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#;
        assert!(reader.line(line, "known").unwrap().ok);
    }
}
