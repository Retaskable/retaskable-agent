//! The chat's classifier (docs/specs/questions-and-triage.md, `classify.ts`): one structured call on Haiku that sorts a
//! message into `bug`, `idea`, `question` or `other` with a confidence. Never an error: anything that goes wrong is
//! [`Classified::Unavailable`], and the caller carries on as it did without a classifier.
use std::future::Future;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

use crate::engines::{SessionResult, SessionSpec, ephemeral_readonly_spec};
use crate::prompt::render_prompt;

pub const TEMPLATE: &str = include_str!("../../prompts/classify.md");
/// A bug or idea at or above this counts as confident.
pub const CONFIDENCE: f64 = 0.8;
pub const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bug,
    Idea,
    Question,
    Other,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "bug" => Kind::Bug,
            "idea" => Kind::Idea,
            "question" => Kind::Question,
            "other" => Kind::Other,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sorted {
    pub kind: Kind,
    pub confidence: f64,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Classified {
    Sorted(Sorted),
    /// No classifier this time: signed out, no Claude here, the day's budget spent, a failure or the timeout.
    Unavailable,
}

impl Classified {
    /// True only for `kind` (a bug or an idea) at or above [`CONFIDENCE`].
    pub fn is_confident(&self, kind: Kind) -> bool {
        matches!(self, Classified::Sorted(s) if matches!(kind, Kind::Bug | Kind::Idea) && s.kind == kind && s.confidence >= CONFIDENCE)
    }
}

/// What a call is allowed to do (`ClassifyOptions`).
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Claude Code is among the engines found here: only it has Haiku.
    pub has_claude: bool,
    pub claude_signed_out: bool,
    /// `budget.askUsd` (0 is no cap) and what has been spent today.
    pub ask_usd: f64,
    pub spent_today_usd: f64,
    pub timeout: Duration,
}

impl Options {
    pub fn new(has_claude: bool, claude_signed_out: bool, ask_usd: f64, spent_today_usd: f64) -> Self {
        Options {
            has_claude,
            claude_signed_out,
            ask_usd,
            spent_today_usd,
            timeout: TIMEOUT,
        }
    }
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": { "enum": ["bug", "idea", "question", "other"] },
            "confidence": { "type": "number" },
            "title": { "type": "string" }
        },
        "required": ["kind", "confidence"]
    })
}

/// A result that doesn't fit is `other` at 0, as `checked` reads it.
fn checked(v: &Value) -> Sorted {
    let other = Sorted {
        kind: Kind::Other,
        confidence: 0.0,
        title: None,
    };
    let Some(kind) = v.get("kind").and_then(Value::as_str).and_then(Kind::parse) else {
        return other;
    };
    let Some(confidence) = v.get("confidence").and_then(Value::as_f64).filter(|c| c.is_finite()) else {
        return other;
    };
    let title = v.get("title").and_then(Value::as_str).filter(|t| !t.is_empty());
    Sorted {
        kind,
        confidence,
        title: title.map(str::to_string),
    }
}

/// The one session: Haiku on Claude Code, one turn, no repository (`cwd` is a scratch directory), and only what is left
/// of the day's ask budget.
pub fn session_spec(text: &str, o: &Options, cwd: &Path) -> Result<SessionSpec> {
    let prompt = render_prompt(TEMPLATE, &[("message", text.to_string())])?;
    let budget = if o.ask_usd > 0.0 {
        (o.ask_usd - o.spent_today_usd).max(0.0001)
    } else {
        0.0
    };
    let mut spec = ephemeral_readonly_spec(
        Some(crate::engines::EngineId::Claude),
        "haiku".to_string(),
        prompt,
        schema(),
        cwd.to_path_buf(),
        budget,
        None,
        None,
        false,
        false,
    );
    spec.max_turns = Some(1);
    Ok(spec)
}

/// Sorts `text`, running the session through `run` (the caller's [`crate::engines::run_session`]).
pub async fn classify<F, Fut>(text: &str, o: &Options, cwd: &Path, run: F) -> Classified
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    if !o.has_claude || o.claude_signed_out || (o.ask_usd > 0.0 && o.spent_today_usd >= o.ask_usd) {
        return Classified::Unavailable;
    }
    let Ok(spec) = session_spec(text, o, cwd) else {
        return Classified::Unavailable;
    };
    match tokio::time::timeout(o.timeout, run(spec)).await {
        Ok(Ok(r)) if r.ok => Classified::Sorted(r.structured.as_ref().map_or_else(|| checked(&Value::Null), checked)),
        _ => Classified::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::engine::test_result;

    fn opts() -> Options {
        Options::new(true, false, 0.0, 0.0)
    }

    async fn run(v: Value, o: &Options) -> Classified {
        classify("it crashes", o, Path::new("."), |_| async move {
            Ok(test_result(true, "success", Some(v), None))
        })
        .await
    }

    #[tokio::test]
    async fn a_bug_at_point_eight_is_confident_and_below_is_not() {
        let r = run(json!({ "kind": "bug", "confidence": 0.8, "title": "Crash" }), &opts()).await;
        assert!(r.is_confident(Kind::Bug) && !r.is_confident(Kind::Idea));
        let r = run(json!({ "kind": "bug", "confidence": 0.79 }), &opts()).await;
        assert!(!r.is_confident(Kind::Bug));
    }

    #[tokio::test]
    async fn a_question_is_never_confident_and_a_misfit_is_other() {
        let r = run(json!({ "kind": "question", "confidence": 1.0 }), &opts()).await;
        assert!(!r.is_confident(Kind::Question));
        let r = run(json!({ "kind": "poem", "confidence": 1.0 }), &opts()).await;
        let Classified::Sorted(s) = r else { panic!() };
        assert_eq!((s.kind, s.confidence), (Kind::Other, 0.0));
    }

    #[tokio::test]
    async fn signed_out_no_claude_or_a_spent_budget_never_call() {
        for o in [
            Options::new(true, true, 0.0, 0.0),
            Options::new(false, false, 0.0, 0.0),
            Options::new(true, false, 1.0, 1.0),
        ] {
            let r = classify("x", &o, Path::new("."), |_| async { panic!("called") }).await;
            assert_eq!(r, Classified::Unavailable);
        }
    }

    #[tokio::test]
    async fn a_failure_an_error_and_a_timeout_are_unavailable() {
        let failed = classify("x", &opts(), Path::new("."), |_| async {
            Ok(test_result(false, "error", None, Some("x")))
        })
        .await;
        assert_eq!(failed, Classified::Unavailable);
        let err = classify("x", &opts(), Path::new("."), |_| async { anyhow::bail!("no") }).await;
        assert_eq!(err, Classified::Unavailable);
        let slow = Options {
            timeout: Duration::from_millis(10),
            ..opts()
        };
        let late = classify("x", &slow, Path::new("."), |_| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(test_result(true, "success", None, None))
        })
        .await;
        assert_eq!(late, Classified::Unavailable);
    }

    #[test]
    fn the_session_is_one_haiku_turn_with_what_is_left_of_the_budget() {
        let o = Options::new(true, false, 2.0, 0.5);
        let s = session_spec("hi", &o, Path::new(".")).unwrap();
        assert_eq!((s.model.as_str(), s.max_turns, s.budget_usd), ("haiku", Some(1), 1.5));
        assert!(s.prompt.contains("hi"));
    }
}
