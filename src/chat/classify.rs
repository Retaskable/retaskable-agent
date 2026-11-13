//! The Chat Agent's classifier (chat-classifier.md v0.2, `classify.ts` `routeAsk`): one structured Haiku call that sorts
//! a message into `bug`, `idea`, `question`, `code` or `other`, then where it goes. It never guesses: a call that cannot
//! run is [`Route::Unavailable`] with the reason, and the caller says it and files and answers nothing.
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::claude::Runner;
use crate::engines::{CouldNotStart, EngineId, SessionSpec, ephemeral_readonly_spec};
use crate::prompt::render_prompt;

pub const TEMPLATE: &str = include_str!("../../prompts/chat/classify.md");
/// A bug, idea or code question at or above this is acted on; anything else is answered as a question.
pub const CONFIDENCE: f64 = 0.8;
pub const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bug,
    Idea,
    Question,
    Code,
    Other,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "bug" => Kind::Bug,
            "idea" => Kind::Idea,
            "question" => Kind::Question,
            "code" => Kind::Code,
            "other" => Kind::Other,
            _ => return None,
        })
    }
}

/// Why the classifier could not run (`ClassifyUnavailable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    SignedOut,
    OverBudget,
    Timeout,
    Unreadable,
    NoClaude,
}

/// The sentence the chat shows when nothing was filed or answered (`unavailableText`). `host` is this computer's name.
pub fn unavailable_text(reason: Reason, host: &str) -> String {
    match reason {
        Reason::SignedOut => format!(
            "Chat Agent signed out. Nothing was filed or answered: sign in to Claude on {host}, then send it again."
        ),
        Reason::OverBudget => {
            "Chat Agent over budget. Nothing was filed or answered: try again when the shared allowance resets."
                .to_string()
        }
        Reason::NoClaude => {
            format!("There is no Claude on {host}, so the Chat Agent can't sort this. Nothing was filed or answered.")
        }
        Reason::Timeout => "The Chat Agent took too long. Nothing was filed or answered: send it again.".to_string(),
        Reason::Unreadable => {
            "The Chat Agent couldn't read that. Nothing was filed or answered: send it again.".to_string()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filing {
    Bug,
    Idea,
}

/// Where a message goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// File it on the board as the asker's own bug or idea, with the model's title when it gave one.
    File {
        kind: Filing,
        title: Option<String>,
    },
    /// A question about the code: a Coding Agent answers it.
    Code,
    /// Answer it from the board.
    Question,
    Unavailable(Reason),
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": { "enum": ["bug", "idea", "question", "code", "other"] },
            "confidence": { "type": "number" },
            "title": { "type": "string" }
        },
        "required": ["kind", "confidence"]
    })
}

/// Haiku, one turn, in an empty scratch directory, with the runner's tool list empty.
fn session_spec(text: &str, cwd: &Path) -> anyhow::Result<SessionSpec> {
    let prompt = render_prompt(TEMPLATE, &[("message", text.to_string())])?;
    let mut spec = ephemeral_readonly_spec(
        Some(EngineId::Claude),
        "haiku".to_string(),
        prompt,
        schema(),
        cwd.to_path_buf(),
        0.0,
        None,
        None,
        false,
        false,
    );
    spec.max_turns = Some(1);
    Ok(spec)
}

/// Why a session that ran did not give a result (`classifyWhy`'s session half).
pub fn failure_reason(r: &crate::engines::SessionResult) -> Reason {
    if r.limit_hit {
        Reason::OverBudget
    } else if r.signed_out {
        Reason::SignedOut
    } else {
        Reason::Unreadable
    }
}

/// `{kind, confidence, title?}` checked: a result that does not fit is unreadable, never `other`.
fn checked(v: &Value) -> Option<(Kind, f64, Option<String>)> {
    let kind = Kind::parse(v.get("kind")?.as_str()?)?;
    let confidence = v.get("confidence")?.as_f64().filter(|c| c.is_finite())?;
    let title = v
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    Some((kind, confidence, title))
}

/// Sorts a message that has neither pictures nor a reply. A message with either is a question: the classifier sees
/// only text, and a reply continues an answer the asker already has.
pub async fn route_ask<R: Runner>(runner: &R, text: &str, plain: bool, cwd: &Path, timeout: Duration) -> Route {
    if !plain {
        return Route::Question;
    }
    if !runner.available() {
        return Route::Unavailable(Reason::NoClaude);
    }
    let Ok(spec) = session_spec(text, cwd) else {
        return Route::Unavailable(Reason::Unreadable);
    };
    let result = match tokio::time::timeout(timeout, runner.run(spec)).await {
        Err(_) => return Route::Unavailable(Reason::Timeout),
        Ok(Err(e)) if e.downcast_ref::<CouldNotStart>().is_some() => return Route::Unavailable(Reason::NoClaude),
        Ok(Err(_)) => return Route::Unavailable(Reason::Unreadable),
        Ok(Ok(r)) => r,
    };
    if !result.ok {
        return Route::Unavailable(failure_reason(&result));
    }
    let Some((kind, confidence, title)) = result.structured.as_ref().and_then(checked) else {
        return Route::Unavailable(Reason::Unreadable);
    };
    if confidence < CONFIDENCE {
        return Route::Question;
    }
    match kind {
        Kind::Bug => Route::File {
            kind: Filing::Bug,
            title,
        },
        Kind::Idea => Route::File {
            kind: Filing::Idea,
            title,
        },
        Kind::Code => Route::Code,
        Kind::Question | Kind::Other => Route::Question,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::claude::fake::Fake;
    use crate::engines::SessionResult;
    use crate::engines::engine::test_result;

    async fn route(v: Value) -> Route {
        let fake = Fake::new(vec![Ok(test_result(true, "success", Some(v), None))]);
        route_ask(&fake, "it crashes", true, Path::new("."), TIMEOUT).await
    }

    #[tokio::test]
    async fn a_confident_bug_or_idea_is_filed_with_its_title() {
        let r = route(json!({ "kind": "bug", "confidence": 0.8, "title": " Crash " })).await;
        assert_eq!(
            r,
            Route::File {
                kind: Filing::Bug,
                title: Some("Crash".into())
            }
        );
        let r = route(json!({ "kind": "idea", "confidence": 0.95 })).await;
        assert_eq!(
            r,
            Route::File {
                kind: Filing::Idea,
                title: None
            }
        );
    }

    #[tokio::test]
    async fn code_is_a_code_question_only_when_confident() {
        assert_eq!(route(json!({ "kind": "code", "confidence": 0.9 })).await, Route::Code);
        assert_eq!(
            route(json!({ "kind": "code", "confidence": 0.79 })).await,
            Route::Question
        );
        assert_eq!(
            route(json!({ "kind": "bug", "confidence": 0.79 })).await,
            Route::Question
        );
        assert_eq!(
            route(json!({ "kind": "other", "confidence": 1.0 })).await,
            Route::Question
        );
    }

    #[tokio::test]
    async fn a_misfit_result_is_unreadable_not_other() {
        for v in [
            json!({ "kind": "poem", "confidence": 1.0 }),
            json!({ "kind": "bug" }),
            json!("bug"),
        ] {
            assert_eq!(route(v).await, Route::Unavailable(Reason::Unreadable));
        }
    }

    #[tokio::test]
    async fn pictures_or_a_reply_skip_the_classifier() {
        let fake = Fake::new(vec![]);
        let r = route_ask(&fake, "see this", false, Path::new("."), TIMEOUT).await;
        assert_eq!(r, Route::Question);
        assert!(fake.specs.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn no_claude_never_runs_a_session() {
        let mut fake = Fake::new(vec![]);
        fake.available = false;
        let r = route_ask(&fake, "x", true, Path::new("."), TIMEOUT).await;
        assert_eq!(r, Route::Unavailable(Reason::NoClaude));
        assert!(fake.specs.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failed_session_says_why() {
        let cases = [
            (
                SessionResult {
                    limit_hit: true,
                    ..test_result(false, "error", None, Some("x"))
                },
                Reason::OverBudget,
            ),
            (
                SessionResult {
                    signed_out: true,
                    ..test_result(false, "error", None, Some("x"))
                },
                Reason::SignedOut,
            ),
            (test_result(false, "error", None, Some("boom")), Reason::Unreadable),
        ];
        for (result, reason) in cases {
            let fake = Fake::new(vec![Ok(result)]);
            let r = route_ask(&fake, "x", true, Path::new("."), TIMEOUT).await;
            assert_eq!(r, Route::Unavailable(reason));
        }
    }

    #[tokio::test]
    async fn a_session_that_could_not_start_is_no_claude_and_another_error_is_unreadable() {
        let fake = Fake::new(vec![Err(anyhow::Error::new(CouldNotStart("gone".into())))]);
        let r = route_ask(&fake, "x", true, Path::new("."), TIMEOUT).await;
        assert_eq!(r, Route::Unavailable(Reason::NoClaude));
        let fake = Fake::new(vec![Err(anyhow::anyhow!("pipe"))]);
        let r = route_ask(&fake, "x", true, Path::new("."), TIMEOUT).await;
        assert_eq!(r, Route::Unavailable(Reason::Unreadable));
    }

    struct Hangs;
    impl Runner for Hangs {
        fn available(&self) -> bool {
            true
        }
        async fn run(&self, _: SessionSpec) -> anyhow::Result<SessionResult> {
            std::future::pending().await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_past_the_timeout_is_a_timeout() {
        let r = route_ask(&Hangs, "x", true, Path::new("."), TIMEOUT).await;
        assert_eq!(r, Route::Unavailable(Reason::Timeout));
    }

    #[tokio::test]
    async fn the_session_is_haiku_one_turn_and_read_only() {
        let fake = Fake::new(vec![Ok(test_result(
            true,
            "success",
            Some(json!({ "kind": "idea", "confidence": 1 })),
            None,
        ))]);
        route_ask(&fake, "make it blue", true, Path::new("."), TIMEOUT).await;
        let specs = fake.specs.lock().unwrap();
        let s = &specs[0];
        assert_eq!(
            (s.model.as_str(), s.max_turns, s.readonly, s.shell),
            ("haiku", Some(1), true, false)
        );
        assert!(s.prompt.contains("make it blue"));
    }

    #[test]
    fn every_reason_names_what_was_not_done() {
        for r in [
            Reason::SignedOut,
            Reason::OverBudget,
            Reason::NoClaude,
            Reason::Timeout,
            Reason::Unreadable,
        ] {
            assert!(unavailable_text(r, "box").contains("Nothing was filed or answered"));
        }
        assert!(unavailable_text(Reason::SignedOut, "box").starts_with("Chat Agent signed out."));
        assert!(unavailable_text(Reason::OverBudget, "box").starts_with("Chat Agent over budget."));
    }
}
