//! A Coding Agent's side of a code question (docs/specs/chat-classifier.md, `codeAnswer` in `assistant.ts`): an `ask`
//! intent row with `args.codeQuestion === true` and a string `args.question`, posted by the asker's Chat Agent. It is
//! answered from the checkout with no action carried out, and the row finishes with `{ answer }` cut to
//! [`MAX_ANSWER`] characters, or failed with a plain reason.
//!
//! This agent has no intent puller yet, so the puller's wiring is not here: [`question_of`] picks the question out of
//! a claimed row's kind and args, and [`answer_from`] turns the finished assistant session into the row's result.
use serde_json::{Value, json};

use super::model::parse_assistant_result;
use crate::engines::{SessionResult, describe_failure};
use crate::text::clip;

/// The most characters of an answer a row's result carries.
pub const MAX_ANSWER: usize = 3500;
/// The most characters of a failure's reason.
pub const MAX_REASON: usize = 280;

/// The question of a claimed intent row that is a code question; `None` for any other row, which the puller runs as a command.
pub fn question_of<'a>(kind: &str, args: &'a Value) -> Option<&'a str> {
    if kind != "ask" || args.get("codeQuestion") != Some(&Value::Bool(true)) {
        return None;
    }
    args.get("question")?.as_str()
}

/// What the claim answers when the board holds a code question back (a red main, queued bugs): `{ won: false, blocked: true }`.
/// The puller waits and asks again; it is no error and not a lost race.
pub fn is_blocked(claim: &Value) -> bool {
    claim.get("won") == Some(&Value::Bool(false)) && claim.get("blocked") == Some(&Value::Bool(true))
}

/// The row's result from the finished assistant session. Any actions the session asked for are ignored. A session that
/// failed or gave nothing usable is `Err` with a plain reason, for the row to finish failed with.
pub fn answer_from(r: &SessionResult, budget_usd: f64) -> Result<Value, String> {
    if !r.ok {
        return Err(clip(&describe_failure(r, budget_usd), MAX_REASON));
    }
    match r.structured.as_ref().and_then(parse_assistant_result) {
        Some(a) if !a.answer.trim().is_empty() => Ok(json!({ "answer": clip(&a.answer, MAX_ANSWER) })),
        _ => Err("Couldn't answer that.".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(v: Value) -> SessionResult {
        SessionResult {
            ok: true,
            subtype: "success".into(),
            structured: Some(v),
            ..Default::default()
        }
    }

    #[test]
    fn only_an_ask_row_marked_as_a_code_question_with_a_string_question_is_one() {
        let q = json!({ "question": "where is X?", "codeQuestion": true });
        assert_eq!(question_of("ask", &q), Some("where is X?"));
        assert_eq!(question_of("play", &q), None);
        assert_eq!(question_of("ask", &json!({ "question": "q" })), None);
        assert_eq!(
            question_of("ask", &json!({ "question": "q", "codeQuestion": "true" })),
            None
        );
        assert_eq!(
            question_of("ask", &json!({ "question": 5, "codeQuestion": true })),
            None
        );
        assert_eq!(question_of("ask", &json!({ "codeQuestion": true })), None);
    }

    #[test]
    fn a_blocked_claim_is_told_apart_from_a_lost_race() {
        assert!(is_blocked(&json!({ "won": false, "blocked": true })));
        assert!(!is_blocked(&json!({ "won": false })));
        assert!(!is_blocked(&json!({ "won": true })));
        assert!(!is_blocked(&json!({ "won": false, "blocked": false })));
    }

    #[test]
    fn the_answer_is_cut_and_the_actions_are_ignored() {
        let long = "x".repeat(MAX_ANSWER + 100);
        let out = answer_from(&done(json!({ "answer": long, "actions": [{ "type": "pause" }] })), 1.0).unwrap();
        let text = out["answer"].as_str().unwrap();
        assert!(text.chars().count() <= MAX_ANSWER);
        assert_eq!(out.as_object().unwrap().len(), 1);
        let short = answer_from(&done(json!({ "answer": "In src/a.rs.", "actions": [] })), 1.0).unwrap();
        assert_eq!(short, json!({ "answer": "In src/a.rs." }));
    }

    #[test]
    fn a_failed_or_empty_session_is_a_plain_reason() {
        let failed = SessionResult {
            ok: false,
            subtype: "error_max_budget_usd".into(),
            cost_usd: Some(1.2),
            ..Default::default()
        };
        let why = answer_from(&failed, 1.0).unwrap_err();
        assert!(why.contains("budget") && why.chars().count() <= MAX_REASON, "{why}");
        assert!(answer_from(&done(json!({ "nope": 1 })), 1.0).is_err());
        assert!(answer_from(&done(json!({ "answer": "  ", "actions": [] })), 1.0).is_err());
    }
}
