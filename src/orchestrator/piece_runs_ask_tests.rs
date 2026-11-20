//! A run's questions (one-tap-or-none.md, piece 5): without options they go back to the session once and are then asked with
//! "Follow the brief"; all obvious, the end carries the answers for the loop to send as `piece_answer`; a mixed set, a repeat, the
//! tiers, a chore and a playtest fix are asked.
use super::*;
use crate::orchestrator::piece_ask::AUTO_ANSWER;

fn named(extra: Value) -> Value {
    let mut p = json!({ "id": "one-tap-or-none-piece-5", "n": 5, "tier": "routine", "checking": "auto", "title": "Ask well", "what": "Ask it.", "doneWhen": "Asked." });
    p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    p
}

fn qs(list: &str) -> String {
    answer("needs_clarification", &format!(r#","questions":{list}"#))
}

async fn run_with(r: &Rig, piece: Value) -> RunEnd {
    let (req, mut s) = request(r, RunKind::Build, piece);
    r.starter.start(req).unwrap();
    end_of(&mut s.ends).await
}

const NO_OPTIONS: &str = r#"[{"question":"Claude's answer was already sent. What should Change it do?","options":[]}]"#;
const OBVIOUS: &str = r#"[{"question":"Which name?","options":["foo","bar"],"obvious":true,"why":"the spec names it"},{"question":"Keep the old way?","options":["Yes","No"],"obvious":true}]"#;

#[tokio::test]
async fn a_question_without_options_goes_back_once_and_is_then_asked_with_follow_the_brief() {
    let r = rig(&["true", "true", "true"], &qs(NO_OPTIONS));
    let end = run_with(&r, named(json!({}))).await;
    assert_eq!(end.outcome, RunOutcome::Questions);
    let back = std::fs::read_to_string(r.root.join("prompt2.txt")).unwrap();
    assert!(back.contains(crate::obvious::SEND_BACK), "{back}");
    assert!(!r.root.join("prompt3.txt").exists(), "sent back once, not twice");
    assert_eq!(
        end.result["questions"][0]["options"],
        json!([{ "label": "Follow the brief" }])
    );
    assert!(end.result.get(AUTO_ANSWER).is_none());
}

#[tokio::test]
async fn a_session_that_gives_options_when_sent_back_is_asked_with_them() {
    let r = rig(&["true", r#"cp "$R/fixed.jsonl" "$R/out.jsonl""#], &qs(NO_OPTIONS));
    std::fs::write(
        r.root.join("fixed.jsonl"),
        qs(r#"[{"question":"What should Change it do?","options":["Reopen the questions","Leave it"]}]"#),
    )
    .unwrap();
    let end = run_with(&r, named(json!({}))).await;
    assert_eq!(end.outcome, RunOutcome::Questions);
    assert_eq!(
        end.result["questions"][0]["options"],
        json!([{ "label": "Reopen the questions" }, { "label": "Leave it" }])
    );
}

#[tokio::test]
async fn questions_with_options_are_not_sent_back() {
    let r = rig(&["true", "true"], &qs(r#"[{"question":"Which?","options":["a","b"]}]"#));
    let end = run_with(&r, named(json!({}))).await;
    assert_eq!(end.outcome, RunOutcome::Questions);
    assert!(!r.root.join("prompt2.txt").exists());
    assert!(end.result.get(AUTO_ANSWER).is_none(), "not obvious: a person is asked");
}

#[tokio::test]
async fn an_all_obvious_set_carries_the_first_options_as_the_answers_for_the_piece() {
    let r = rig(&["true"], &qs(OBVIOUS));
    let end = run_with(&r, named(json!({}))).await;
    assert_eq!(end.outcome, RunOutcome::Questions);
    let auto = &end.result[AUTO_ANSWER];
    assert_eq!(auto["piece"], "one-tap-or-none#5");
    assert_eq!(auto["answers"], "foo\nYes");
    assert_eq!(auto["went"][0], "Claude went with foo: the spec names it");
    assert_eq!(auto["went"][1], "Claude went with Yes");
}

#[tokio::test]
async fn a_mixed_set_is_asked() {
    let mixed = r#"[{"question":"Which name?","options":["foo","bar"],"obvious":true},{"question":"Which colour?","options":["red","blue"]}]"#;
    let end = run_with(&rig(&["true"], &qs(mixed)), named(json!({}))).await;
    assert!(end.result.get(AUTO_ANSWER).is_none());
}

#[tokio::test]
async fn a_repeat_is_asked() {
    let next =
        json!({ "next": { "kind": "build", "what": "Questions: Which name?\nKeep the old way?\nAnswers: foo\nYes" } });
    let end = run_with(&rig(&["true"], &qs(OBVIOUS)), named(next)).await;
    assert!(end.result.get(AUTO_ANSWER).is_none());
}

#[tokio::test]
async fn the_tier_and_chore_exceptions_hold() {
    for extra in [
        json!({ "tier": "needsyou" }),
        json!({ "tier": "tryfirst" }),
        json!({ "checking": "manual" }),
        json!({ "held": "touches sign-in" }),
        json!({ "chore": { "kind": "playtest_fix", "title": "t" } }),
    ] {
        let end = run_with(&rig(&["true"], &qs(OBVIOUS)), named(extra.clone())).await;
        assert!(end.result.get(AUTO_ANSWER).is_none(), "{extra}");
    }
}
