use super::*;
use crate::implementer::Question;
use anyhow::anyhow;

fn answer(status: &str) -> ImplementerResult {
    serde_json::from_value(json!({ "status": status, "summary": "why" })).unwrap()
}

fn pushed() -> Pushed {
    Pushed {
        branch: "b".into(),
        change: Published {
            url: "u".into(),
            number: 7,
            created: true,
        },
    }
}

fn q(text: &str) -> Question {
    Question {
        header: "H".into(),
        question: text.into(),
        options: vec!["a".into(), "b".into()],
    }
}

fn decided(text: &str) -> Decision {
    Decision {
        question: text.into(),
        answer: "yes".into(),
        at: "t".into(),
    }
}

fn end(r: ImplementerResult, p: Option<&Pushed>, d: &[Decision]) -> (RunOutcome, Value) {
    end_from_answer(Ok(r), p, d)
}

#[test]
fn pr_opened_carries_branch_and_change() {
    let (o, v) = end(answer("pr_opened"), Some(&pushed()), &[]);
    assert_eq!(o, RunOutcome::PrOpened);
    assert_eq!(v, json!({ "branch": "b", "change": { "number": 7, "url": "u" } }));
}

#[test]
fn pr_opened_without_a_push_fails() {
    assert!(matches!(end(answer("pr_opened"), None, &[]).0, RunOutcome::Failed(_)));
}

#[test]
fn questions_without_a_push() {
    let mut r = answer("needs_clarification");
    r.questions = vec![q("Which?")];
    let (o, v) = end(r, None, &[]);
    assert_eq!(o, RunOutcome::Questions);
    assert_eq!(
        v,
        json!({ "questions": [{ "header": "H", "question": "Which?", "options": ["a", "b"] }] })
    );
}

#[test]
fn questions_after_a_pushed_slice_carry_the_change() {
    let mut r = answer("needs_clarification");
    r.questions = vec![Question {
        header: String::new(),
        question: "Which?".into(),
        options: vec![],
    }];
    let (o, v) = end(r, Some(&pushed()), &[]);
    assert_eq!(o, RunOutcome::Questions);
    assert_eq!(
        v,
        json!({ "branch": "b", "change": { "number": 7, "url": "u" }, "questions": [{ "question": "Which?", "options": [] }] })
    );
}

#[test]
fn an_already_answered_question_fails() {
    let mut r = answer("needs_clarification");
    r.questions = vec![q("  which?  ")];
    let (o, v) = end(r, None, &[decided("Which?")]);
    assert!(matches!(o, RunOutcome::Failed(w) if w.contains("already answered") && w.contains("Which?")));
    assert_eq!(v, json!({}));
}

#[test]
fn person_steps_with_and_without_steps() {
    let mut r = answer("person_steps");
    r.person_steps = vec!["Add the secret.".into()];
    assert_eq!(
        end(r, None, &[]),
        (RunOutcome::PersonSteps, json!({ "steps": ["Add the secret."] }))
    );
    assert_eq!(
        end(answer("person_steps"), None, &[]).0,
        RunOutcome::Failed("Implementer gave up: why".into())
    );
}

#[test]
fn nothing_left_failed_waits_on_and_unknown() {
    assert_eq!(
        end(answer("nothing_left"), None, &[]),
        (RunOutcome::NothingLeft, json!({}))
    );
    assert_eq!(
        end(answer("failed"), None, &[]),
        (RunOutcome::Failed("why".into()), json!({}))
    );
    let mut r = answer("waits_on");
    r.waits_on = vec!["a".into(), "b".into()];
    assert_eq!(
        end(r, None, &[]).0,
        RunOutcome::Failed("Needs a, b built first: why".into())
    );
    assert_eq!(
        end(answer("waits_on"), None, &[]).0,
        RunOutcome::Failed("Needs another item built first: why".into())
    );
    assert_eq!(
        end(answer("zzz"), None, &[]).0,
        RunOutcome::Failed("The session answered with zzz: why".into())
    );
}

#[test]
fn session_errors() {
    let e = |err: Error| end_from_answer(Err(err), None, &[]);
    assert_eq!(
        e(Interrupted("cut".into()).into()),
        (RunOutcome::Interrupted, json!({}))
    );
    assert_eq!(
        e(SilenceKilled("quiet".into()).into()).0,
        RunOutcome::Failed("went quiet and was killed: quiet".into())
    );
    assert_eq!(
        e(MaxTurns("turns".into()).into()).0,
        RunOutcome::Failed("ran out of turns: turns".into())
    );
    assert_eq!(
        e(anyhow!("boom")).0,
        RunOutcome::Failed("The session ended without answering: boom".into())
    );
}

#[test]
fn pushes_for_pr_opened_and_for_questions_with_commits() {
    assert!(needs_push("pr_opened", 0));
    assert!(needs_push("needs_clarification", 2));
    assert!(!needs_push("needs_clarification", 0));
    assert!(!needs_push("person_steps", 3));
    assert!(!needs_push("failed", 3));
}
