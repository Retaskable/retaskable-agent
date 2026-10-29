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
    Engine, EngineId, EngineReader, LocalTarget, SessionEvent, SessionResult, SessionSpec, SessionStart, is_signed_out,
    signed_out_text,
};
use super::text::tail;

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
    let dirs = crate::env_path::path_dirs(&env);
    resolve_claude(env, cfg!(windows), &dirs)
}

/// `sandboxSupported()`: Claude Code's own sandbox (04-architecture.md section 4.2) is built for
/// macOS and Linux only; on native Windows a session runs unsandboxed, as before.
pub fn sandbox_supported(windows: bool) -> bool {
    !windows
}

/// `cfg.sandbox == Auto && sandboxSupported()`: whether a session for this project should run
/// sandboxed, combining the project's own setting with whether this computer can run one at all.
pub fn sandbox_enabled(mode: crate::config::SandboxMode, windows: bool) -> bool {
    mode == crate::config::SandboxMode::Auto && sandbox_supported(windows)
}

/// `spec.model == "local"` with `spec.local` actually naming a server: the one check the args,
/// the environment and the reader all share, following `claude.ts`, so they can't disagree about
/// which session is local (07-orchestrator-loop.md section 7.2).
fn local_target(spec: &SessionSpec) -> Option<&LocalTarget> {
    (spec.model == "local").then_some(spec.local.as_ref()).flatten()
}

/// `buildArgs(spec)`: the full `claude -p` argument list for one session. The prompt itself isn't
/// here — docs/specs/agent/04-architecture.md says it goes in on stdin, to stay clear of Windows's
/// command-line length limit.
fn build_args(spec: &SessionSpec) -> Vec<String> {
    let local = local_target(spec);
    // section 7.2: a local session still starts with `--model sonnet`; its environment remaps that.
    let model = if local.is_some() {
        "sonnet".to_string()
    } else {
        spec.model.clone()
    };
    let mut args = vec![
        "-p".to_string(),
        "--model".to_string(),
        model,
        "--permission-mode".to_string(),
        "auto".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--json-schema".to_string(),
        spec.schema.to_string(),
    ];
    // section 7.2: "no dollar caps" — a local session's tokens are free, so the budget cap never
    // applies to it.
    if spec.budget_usd > 0.0 && local.is_none() {
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
    if spec.sandbox {
        args.push("--settings".to_string());
        args.push(serde_json::json!({"sandbox": sandbox_settings(cfg!(target_os = "macos"))}).to_string());
    }
    args
}

/// The sandbox a session runs in (`--settings`'s `sandbox` key), following `sandboxSettings()`.
/// On macOS it may also reach the system's certificate checker (trustd) and nothing more, so
/// `gh`, a Go program, can check GitHub's certificate and open the pull request; without it, it
/// fails with `x509: OSStatus -26276`.
fn sandbox_settings(macos: bool) -> Value {
    let mut settings = serde_json::json!({"enabled": true, "autoAllowBashIfSandboxed": true});
    if macos {
        settings["network"] = serde_json::json!({"allowMachLookup": ["com.apple.trustd.agent"]});
    }
    settings
}

/// `sessionEnv(spec)`: environment variables layered on top of the agent's own for one session —
/// no MCP servers from claude.ai and `noDeploy.ts`'s fake deploy token for every session, plus
/// either a cloud session's auto-compact window and sonnet/haiku subagents, or (section 7.2) a
/// local one's own model env vars in their place.
fn session_env(spec: &SessionSpec) -> Vec<(String, String)> {
    let mut env = vec![
        ("ENABLE_CLAUDEAI_MCP_SERVERS".to_string(), "false".to_string()),
        ("CLOUDFLARE_API_TOKEN".to_string(), NO_DEPLOY_TOKEN.to_string()),
        ("CF_API_TOKEN".to_string(), NO_DEPLOY_TOKEN.to_string()),
    ];
    match local_target(spec) {
        Some(local) => {
            env.push(("ANTHROPIC_BASE_URL".to_string(), local.base_url.clone()));
            env.push(("ANTHROPIC_AUTH_TOKEN".to_string(), "local".to_string()));
            env.push(("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(), "1".to_string()));
            for var in [
                "ANTHROPIC_DEFAULT_OPUS_MODEL",
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                "ANTHROPIC_SMALL_FAST_MODEL",
                "CLAUDE_CODE_SUBAGENT_MODEL",
            ] {
                env.push((var.to_string(), local.model.clone()));
            }
        }
        None => {
            let subagent_model = if spec.model == "haiku" { "haiku" } else { "sonnet" };
            env.push(("CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(), COMPACT_AT.to_string()));
            env.push(("CLAUDE_CODE_SUBAGENT_MODEL".to_string(), subagent_model.to_string()));
        }
    }
    env
}

/// Reads a session's `stream-json` events one line at a time, following `claude.ts`'s `reader()`.
/// Tracks the running session's own context size as it goes (a subagent's turns, which carry a
/// `parent_tool_use_id`, don't count), and turns the final `result` event into a `SessionResult`.
#[derive(Default)]
pub struct SessionReader {
    /// `local_target(spec).is_some()`: a local server signs nothing in and has no Anthropic usage
    /// limit.
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
        let cost_usd = Some(if self.local {
            0.0
        } else {
            event.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0)
        });

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
            tokens: None,
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
            cost_usd: Some(0.0),
            session_id: known_session_id.to_string(),
            limit_hit,
            signed_out,
            context_tokens: self.context_tokens,
            usage: self.usage.clone(),
            tokens: None,
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
        session_env(spec)
    }

    fn prompt(&self, spec: &SessionSpec) -> String {
        spec.prompt.clone()
    }

    fn reader(&self, spec: &SessionSpec) -> Box<dyn EngineReader> {
        Box::new(SessionReader::new(local_target(spec).is_some()))
    }

    fn summarize(&self, event: &Value, at: i64) -> Vec<SessionEvent> {
        super::summarize::summarize(event, at)
    }
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;
