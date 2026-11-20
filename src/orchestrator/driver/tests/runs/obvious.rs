//! A run that answered its own obvious questions (one-tap-or-none.md): once the store has taken the `questions` end, the loop
//! answers the piece with `piece_answer`, as a person's **Go with the recommended** does. The answers never go with the end.
use super::*;

fn finish_asking(starter: &FakeRuns, token: &str, auto: Option<Value>) {
    let mut result =
        json!({ "what": "Which name?", "questions": [{ "question": "Which name?", "options": ["foo", "bar"] }] });
    if let Some(auto) = auto {
        result["autoAnswer"] = auto;
    }
    let started = starter.started.lock().unwrap();
    let request = started.iter().find(|r| r.token == token).unwrap();
    request.ends.send(RunEnd {
        token: token.into(),
        outcome: RunOutcome::Questions,
        result,
    });
}

fn auto() -> Value {
    json!({ "piece": "one-tap-or-none#5", "answers": "foo", "went": ["Claude went with foo: the spec names it"] })
}

async fn ran_with(auto: Option<Value>, refused: Option<&'static str>, answer_refused: bool) -> Shared_ {
    let (base, script) = store().await;
    let mut rig = rig(1, &[&base]);
    let starter = FakeRuns::new(&[RunKind::Build]);
    script.lock().unwrap().takes = VecDeque::from([run("a", "build")]);
    script.lock().unwrap().done_refused = refused;
    script.lock().unwrap().answer_refused = answer_refused;
    let t0 = Instant::now();
    pass(&mut rig, &starter, t0).await;
    finish_asking(&starter, "a", auto);
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    script
}

async fn ran(auto: Option<Value>, refused: Option<&'static str>) -> Shared_ {
    ran_with(auto, refused, false).await
}

#[tokio::test]
async fn the_taken_end_is_followed_by_piece_answer_with_the_first_options() {
    let script = ran(Some(auto()), None).await;
    let s = script.lock().unwrap();
    let ops = s.ops();
    let done = ops.iter().position(|o| o == "run_done").expect("the end was sent");
    let answer = ops
        .iter()
        .position(|o| o == "piece_answer")
        .expect("the piece was answered");
    assert!(done < answer, "{ops:?}");
    assert_eq!(
        s.args("piece_answer")[0],
        json!({ "piece": "one-tap-or-none#5", "answers": "foo" })
    );
    assert!(
        s.args("run_done")[0]["result"].get("autoAnswer").is_none(),
        "the answers stay off the end"
    );
    assert_eq!(s.args("run_done")[0]["outcome"], "questions");
    let noted = ops
        .iter()
        .position(|o| o == "piece_note")
        .expect("what was chosen is noted");
    assert!(answer < noted, "{ops:?}");
    assert_eq!(
        s.args("piece_note")[0],
        json!({ "piece": "one-tap-or-none#5", "note": "Claude went with foo: the spec names it" })
    );
}

#[tokio::test]
async fn a_questions_end_with_no_answers_is_left_for_a_person() {
    let script = ran(None, None).await;
    assert!(!script.lock().unwrap().ops().iter().any(|o| o == "piece_answer"));
}

#[tokio::test]
async fn nothing_is_noted_when_the_piece_could_not_be_answered() {
    let script = ran_with(Some(auto()), None, true).await;
    let ops = script.lock().unwrap().ops();
    assert!(
        ops.iter().any(|o| o == "piece_answer") && !ops.iter().any(|o| o == "piece_note"),
        "{ops:?}"
    );
}

#[tokio::test]
async fn an_end_the_store_refused_answers_nothing() {
    let script = ran(Some(auto()), Some("stale_run")).await;
    assert!(!script.lock().unwrap().ops().iter().any(|o| o == "piece_answer"));
}
