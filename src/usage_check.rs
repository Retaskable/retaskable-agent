//! The usage recheck (docs/specs/small-agent-jobs.md, `usageCheck.ts`): while Claude's limit holds the computer,
//! every [`GAP`] one tiny Haiku question is read only for its rate-limit event, so a hold that no longer applies ends
//! on the next tick. It never fails: no reading means the old one stands.
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::env::Env;

/// How long after one recheck the next may start.
pub const GAP: Duration = Duration::from_secs(10 * 60);
/// A recheck that hasn't finished by then is no reading.
const TIMEOUT: Duration = Duration::from_secs(90);

/// What the question's stream said.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    /// The latest `rate_limit_info` it carried.
    Event(Value),
    /// It answered with no event: an account with room.
    Answered,
    /// A refusal with no event, or a run that didn't finish: the old reading stands.
    Unread,
}

impl Reading {}

/// Reads the `stream-json` lines of the question.
pub fn read(stdout: &str) -> Reading {
    let (mut info, mut result) = (None, None);
    for event in stdout.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        match event.get("type").and_then(Value::as_str) {
            Some("rate_limit_event") => {
                if let Some(i) = event.get("rate_limit_info").filter(|v| !v.is_null()) {
                    info = Some(i.clone());
                }
            }
            Some("result") => result = Some(event),
            _ => {}
        }
    }
    match (info, result) {
        (Some(info), _) => Reading::Event(info),
        (None, Some(r))
            if r.get("is_error").and_then(Value::as_bool) != Some(true)
                && r.get("api_error_status").is_none_or(Value::is_null) =>
        {
            Reading::Answered
        }
        _ => Reading::Unread,
    }
}

/// The arguments of the question: Haiku, one turn, no session kept.
fn args() -> Vec<String> {
    [
        "-p",
        "Reply with ok.",
        "--model",
        "haiku",
        "--max-turns",
        "1",
        "--output-format",
        "stream-json",
        "--verbose",
        "--no-session-persistence",
        "--permission-mode",
        "auto",
        "--strict-mcp-config",
        "--setting-sources",
        "project",
    ]
    .map(String::from)
    .to_vec()
}

async fn ask(claude: PathBuf) -> Reading {
    let mut cmd = Command::new(claude);
    // Like a Haiku session, run away from any project: no user connectors, and the temp folder has no settings.
    cmd.args(args())
        .current_dir(std::env::temp_dir())
        .env("ENABLE_CLAUDEAI_MCP_SERVERS", "false")
        .env("CLAUDE_CODE_SUBAGENT_MODEL", "haiku")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let Ok(child) = cmd.spawn() else {
        return Reading::Unread;
    };
    match tokio::time::timeout(TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(out)) => read(&String::from_utf8_lossy(&out.stdout)),
        _ => Reading::Unread,
    }
}

/// The recheck's pace: at most one question in flight, one every [`GAP`], the first on the first tick that wants it.
#[derive(Default)]
pub struct UsageCheck {
    last: Option<Instant>,
    inflight: Option<JoinHandle<Reading>>,
}

impl UsageCheck {
    /// Whether a question may start at `now`.
    pub fn due(&self, now: Instant) -> bool {
        self.inflight.is_none() && self.last.is_none_or(|t| now.duration_since(t) >= GAP)
    }

    /// Starts a question, in its own task so the tick isn't held for it.
    pub fn start(&mut self, env: &Env, now: Instant) {
        self.last = Some(now);
        match crate::engines::resolve::resolve_claude_on_path(env) {
            Ok(claude) => self.inflight = Some(tokio::spawn(ask(claude))),
            Err(e) => tracing::warn!("the usage recheck can't find claude: {e:#}"),
        }
    }

    /// The finished question's reading, once it has finished.
    pub async fn take(&mut self) -> Option<Reading> {
        if !self.inflight.as_ref()?.is_finished() {
            return None;
        }
        Some(self.inflight.take()?.await.unwrap_or(Reading::Unread))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_stream_with_an_event_reads_the_latest() {
        let out = format!(
            "{}\n{}\n{}\n",
            json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}}),
            json!({"type": "rate_limit_event", "rate_limit_info": {"status": "rejected", "resetsAt": 5}}),
            json!({"type": "result", "is_error": true}),
        );
        assert_eq!(read(&out), Reading::Event(json!({"status": "rejected", "resetsAt": 5})));
    }

    #[test]
    fn an_answer_with_no_event_is_an_account_with_room() {
        let out = format!("garbage\n{}\n", json!({"type": "result", "is_error": false}));
        assert_eq!(read(&out), Reading::Answered);
    }

    #[test]
    fn a_refusal_or_no_result_is_no_reading() {
        assert_eq!(
            read(&json!({"type": "result", "is_error": true}).to_string()),
            Reading::Unread
        );
        assert_eq!(read(""), Reading::Unread);
        let api = json!({"type": "result", "is_error": false, "api_error_status": 429});
        assert_eq!(read(&api.to_string()), Reading::Unread);
    }

    #[test]
    fn a_question_is_asked_every_ten_minutes_from_the_first_tick() {
        let mut c = UsageCheck::default();
        let t0 = Instant::now();
        assert!(c.due(t0));
        c.last = Some(t0);
        assert!(!c.due(t0 + Duration::from_secs(599)));
        assert!(c.due(t0 + GAP));
    }
}
