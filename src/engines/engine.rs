//! The engine-neutral core of a session: what it's asked to do (`SessionSpec`), what it reports
//! back (`SessionResult`), the `Engine` interface each AI tool implements behind
//! `runner::run_session`, and `describe_failure` — following `agent/src/engines/engine.ts` in
//! Retaskable.com (the ground truth this is porting) and docs/specs/engines/02-the-engine-layer.md.
use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde_json::Value;

/// `EngineId` in `shared/types.ts`: which AI tool a session runs on. Missing on a `SessionSpec`
/// means `Claude`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineId {
    Claude,
    Cursor,
}

impl std::fmt::Display for EngineId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EngineId::Claude => "claude",
            EngineId::Cursor => "cursor",
        })
    }
}

/// `engineOf`'s id lookup, for a value read back from disk rather than a live `SessionSpec`:
/// missing or unrecognized both mean Claude Code (`engines[id ?? "claude"] ?? claude`).
pub fn engine_id_from_str(id: Option<&str>) -> EngineId {
    match id {
        Some("cursor") => EngineId::Cursor,
        _ => EngineId::Claude,
    }
}

/// What a session is asked to do, following `engine.ts`'s `SessionSpec`.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// Missing means `EngineId::Claude` (`engineOf`, `runner.rs`).
    pub engine: Option<EngineId>,
    /// "opus" | "sonnet" | "haiku", or "local" (a local server has no sign-in or usage limit, and
    /// its tokens cost nothing). Pointing a session's environment at a local model's base URL is
    /// still a later slice.
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

/// One session's reader: turns an engine's raw output lines into a `SessionResult`, following
/// `engine.ts`'s `reader(spec, id)` member — one instance per session.
pub trait EngineReader: Send {
    /// Feeds one line of the engine's output. Returns `Some(result)` once the session is done;
    /// every other line returns `None`, having updated tracked state as a side effect.
    fn line(&mut self, line: &str, known_session_id: &str) -> Option<SessionResult>;
    /// The process exited with no result reported at all (a crash, or killed before it could
    /// finish): report that as a failure, with `known_session_id` as the result's id.
    fn no_result(&self, exit_code: Option<i32>, stderr: &str, known_session_id: &str) -> SessionResult;
}

/// One line of a transcript, boiled down for a UI drawer to show (`shared/types.ts`'s
/// `SessionEvent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Tool,
    Text,
    Result,
}

/// `shared/types.ts`'s `SessionEvent`: one readable line from a transcript, at the time it
/// happened.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEvent {
    pub at: i64,
    pub kind: EventKind,
    pub text: String,
}

/// What differs per AI tool a session can run on, following `engine.ts`'s `Engine` interface.
/// Everything else — spawning the process, teeing its lines to the run's `.jsonl`, the watchdog,
/// killing — stays in `runner.rs`, the same whichever engine this is.
pub trait Engine: Send + Sync {
    fn id(&self) -> EngineId;
    /// Finds this engine's binary on the computer (`resolveClaude()`/`resolveCursor()`), given the
    /// environment and `PATH` to search, so the search stays pure and testable.
    fn binary(&self, env: &dyn Fn(&str) -> Option<String>, windows: bool, path_dirs: &[PathBuf]) -> Result<PathBuf>;
    /// The invocation's argument list for `spec` (the prompt itself goes in on stdin, so it's
    /// never here — docs/specs/agent/04-architecture.md, clear of Windows's command-line length
    /// limit).
    fn args(&self, spec: &SessionSpec) -> Vec<String>;
    /// Environment variables layered on top of the agent's own for this session.
    fn env(&self, spec: &SessionSpec) -> Vec<(String, String)>;
    /// The prompt as sent to this engine (as is for Claude Code; Cursor appends its own result
    /// instructions, section 5).
    fn prompt(&self, spec: &SessionSpec) -> String;
    /// A fresh reader for one session.
    fn reader(&self, spec: &SessionSpec) -> Box<dyn EngineReader>;
    /// Turns one stored transcript line back into events, for reading a run back
    /// (`summarize(ev, at)` in `engine.ts`; `readTranscripts`).
    fn summarize(&self, event: &Value, at: i64) -> Vec<SessionEvent>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_out_is_only_a_failed_claude_result_never_cursor() {
        assert!(is_signed_out(false, Some("Failed to authenticate"), Some("claude")));
        assert!(!is_signed_out(false, Some("Failed to authenticate"), Some("cursor")));
        assert!(!is_signed_out(true, Some("Failed to authenticate"), None));
        assert!(signed_out_text(Some(
            "Implementer ended with error: Failed to authenticate: OAuth session expired"
        )));
    }

    #[test]
    fn describe_failure_names_signed_out_limit_budget_and_ordinary_errors() {
        let signed = SessionResult {
            ok: false,
            subtype: "error".to_string(),
            structured: None,
            error: Some("Not logged in".to_string()),
            cost_usd: 0.0,
            session_id: "s".to_string(),
            limit_hit: false,
            signed_out: true,
            context_tokens: None,
            usage: None,
        };
        assert_eq!(
            describe_failure(&signed, 5.0),
            "found Claude signed out on this computer (Not logged in)"
        );

        let limit = SessionResult {
            signed_out: false,
            limit_hit: true,
            error: Some("rate limited".to_string()),
            ..signed.clone()
        };
        assert_eq!(describe_failure(&limit, 5.0), "hit the usage limit");

        let budget = SessionResult {
            subtype: "error_max_budget_usd".to_string(),
            cost_usd: 4.2,
            limit_hit: false,
            signed_out: false,
            error: None,
            ..signed.clone()
        };
        assert_eq!(describe_failure(&budget, 5.0), "hit its $5 budget (spent $4.20)");

        let ordinary = SessionResult {
            subtype: "error_during_execution".to_string(),
            error: Some("tool crashed".to_string()),
            limit_hit: false,
            signed_out: false,
            ..signed
        };
        assert_eq!(
            describe_failure(&ordinary, 5.0),
            "ended with error_during_execution: tool crashed"
        );
    }

    #[test]
    fn engine_id_displays_its_lowercase_name() {
        assert_eq!(EngineId::Claude.to_string(), "claude");
        assert_eq!(EngineId::Cursor.to_string(), "cursor");
    }
}
