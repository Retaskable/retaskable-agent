//! Running a Claude Code session and reading back what it did, following
//! docs/specs/agent/06.2-implementer.md, docs/specs/agent/04-architecture.md,
//! docs/specs/agent/07-orchestrator-loop.md section 7.1, docs/specs/claude-signed-out.md section 3.1,
//! and `agent/src/engines/claude.ts` / `engine.ts` / `noDeploy.ts` in Retaskable.com (the ground truth
//! this is porting). Milestone 2 of docs/specs/agent/11-build-milestones.md: spawn `claude -p` with
//! `--output-format stream-json`, parse the events, capture the final structured result, resume a
//! session that already has history, read the 5-hour usage limit out of the stream, and classify a
//! signed-out Claude (`signed_out` / `describe_failure`). Nothing calls `run_session` yet, or saves
//! its usage reading to `state.json` (`store::Store::set_usage`), since wiring it into a real
//! orchestrator loop is a later milestone — and the computer-wide hold for a sign-out (claude-signed-out
//! sections 3.2–3.4) waits on that pool and orchestrator.
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

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

/// What a session is asked to do, following `engine.ts`'s `SessionSpec`.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// "opus" | "sonnet" | "haiku". The "local" tier (sessionEnv pointing a local model's base
    /// URL) is a later slice.
    pub model: String,
    pub prompt: String,
    pub schema: Value,
    pub cwd: PathBuf,
    pub max_turns: Option<u32>,
    /// 0 (or anything <= 0) means no cap, matching `claude.ts`'s `budget > 0` check.
    pub budget_usd: f64,
    pub start: SessionStart,
}

/// How this session begins, following docs/specs/agent/06.2-implementer.md: a fresh invocation
/// names its own id up front (`--session-id <uuid>`) so a later follow-up can resume it
/// (`claude -p "<follow-up>" --resume <sessionId> ...same flags...`); one that will never be
/// resumed (e.g. a scout) skips persistence entirely.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionStart {
    /// `--no-session-persistence`: no transcript is kept, so there's nothing to resume later.
    Ephemeral,
    /// `--session-id <id>`: a brand new session, started under a known id.
    Fresh(String),
    /// `--resume <id>`: a follow-up that continues a session that already has history.
    Resume(String),
}

/// Claude Code's words when its sign-in on the computer ran out or was never made
/// (claude-signed-out.md section 3.1 / `engine.ts` `SIGNED_OUT`).
static SIGNED_OUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)failed to authenticate|oauth (session|token).*(expired|revoked)|not logged in|please run /login|invalid api key|authentication_error",
    )
    .expect("signed-out pattern")
});

/// Whether `text` (a session's error, or an item's needs-help reason quoting one) says Claude Code
/// is signed out (`signedOutText`).
pub fn signed_out_text(text: Option<&str>) -> bool {
    SIGNED_OUT.is_match(text.unwrap_or(""))
}

/// A Claude Code result that failed only because Claude's sign-in ran out: the computer's problem,
/// not the item's. Plain code on the error's words, never a model's call. A Cursor result never is
/// (`signedOut` in `engine.ts`). `engine` missing means Claude Code.
pub fn is_signed_out(ok: bool, error: Option<&str>, engine: Option<&str>) -> bool {
    engine.unwrap_or("claude") == "claude" && !ok && signed_out_text(error)
}

/// What a session did, following `engine.ts`'s `SessionResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionResult {
    pub ok: bool,
    /// The `result` event's `subtype` (e.g. "success", "error_during_execution"), or "exited"
    /// when the process ended with no `result` event at all.
    pub subtype: String,
    pub structured: Option<Value>,
    pub error: Option<String>,
    pub cost_usd: f64,
    pub session_id: String,
    /// The session ended because the usage limit was hit (`limitHit` in `claude.ts`).
    pub limit_hit: bool,
    /// The session ended because Claude Code's sign-in on this computer ran out (claude-signed-out.md).
    pub signed_out: bool,
    /// The last turn's own context size (prompt + cache), not counting subagents' turns.
    pub context_tokens: Option<u64>,
    /// The latest `rate_limit_info` the stream reported (07-orchestrator-loop.md section 7.1),
    /// kept whole and ready to save as `state.usage`. `None` when the stream never sent one.
    pub usage: Option<Value>,
}

/// "hit its $5 budget", "found Claude signed out on this computer (…)", "ended with …"
/// (`describeFailure` in `engine.ts`).
pub fn describe_failure(r: &SessionResult, budget_usd: f64) -> String {
    if r.subtype == "error_max_budget_usd" {
        let spent = format!("${:.2}", r.cost_usd);
        return format!("hit its ${budget_usd} budget (spent {spent})");
    }
    if r.limit_hit {
        return "hit the usage limit".to_string();
    }
    if r.signed_out {
        return format!(
            "found Claude signed out on this computer ({})",
            r.error.as_deref().unwrap_or("")
        );
    }
    match &r.error {
        Some(err) if !err.is_empty() && err != &r.subtype => {
            format!("ended with {}: {err}", r.subtype)
        }
        _ => format!("ended with {}", r.subtype),
    }
}

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
struct SessionReader {
    /// `spec.model === "local"`: a local server signs nothing in and has no Anthropic usage limit.
    local: bool,
    context_tokens: Option<u64>,
    /// The latest `rate_limit_info` seen so far (section 7.1: "the latest one is kept"); a
    /// session can report it more than once, one per turn.
    usage: Option<Value>,
}

impl SessionReader {
    fn new(local: bool) -> Self {
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
        let signed_out =
            !self.local && !limit_hit && is_signed_out(ok, error.as_deref(), None);

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

/// Spawns `bin` with `args`, feeds `prompt` in on stdin, and reads `stream-json` events from
/// stdout until the process exits — the engine-neutral half of running a session (`claude.ts`
/// supplies the claude-specific `args`/`envs`; this part doesn't know or care that it's Claude).
async fn spawn_and_read(
    bin: &Path,
    args: &[String],
    envs: &[(String, String)],
    cwd: &Path,
    prompt: &str,
    known_session_id: &str,
    local: bool,
) -> Result<SessionResult> {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {}", bin.display()))?;

    let mut stdin = child.stdin.take().context("session has no stdin")?;
    let prompt = prompt.to_string();
    let write_prompt = tokio::spawn(async move {
        let _ = stdin.write_all(prompt.as_bytes()).await;
    });

    let mut stderr = child.stderr.take().context("session has no stderr")?;
    let drain_stderr = tokio::spawn(async move {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf).await;
        buf
    });

    let stdout = child.stdout.take().context("session has no stdout")?;
    let mut lines = BufReader::new(stdout).lines();
    let mut reader = SessionReader::new(local);
    let mut result = None;
    while let Some(line) = lines.next_line().await.context("reading the session's output")? {
        if let Some(r) = reader.line(&line, known_session_id) {
            result = Some(r);
        }
    }

    let _ = write_prompt.await;
    let stderr_text = drain_stderr.await.unwrap_or_default();
    let status = child.wait().await.context("waiting for the session to exit")?;

    Ok(result.unwrap_or_else(|| reader.no_result(status.code(), &stderr_text, known_session_id)))
}

/// Runs one Claude Code session end to end: builds the `claude -p` invocation for `spec`, runs
/// it, and reads back its `SessionResult`. `known_session_id` is used when a `result` event
/// doesn't carry its own `session_id` (and as the id on a crash with no `result` event at all).
pub async fn run_session(bin: &Path, spec: &SessionSpec, known_session_id: &str) -> Result<SessionResult> {
    spawn_and_read(
        bin,
        &build_args(spec),
        &session_env(&spec.model),
        &spec.cwd,
        &spec.prompt,
        known_session_id,
        spec.model == "local",
    )
    .await
}

#[cfg(test)]
mod tests;
