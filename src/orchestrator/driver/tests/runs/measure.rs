//! The measurement events a run writes (agent-telemetry.md section 3.4): `run_start`, `run_end`, `needs_person`, the
//! chore's `chore_start` and `chore_end`, and the merge commit and way on `merged`.
use super::*;

fn events_of(rig: &mut Rig, kinds: &[&str]) -> Vec<Value> {
    (rig.lp(0).events().recent().iter())
        .map(Entry::to_value)
        .filter(|e| kinds.contains(&e["kind"].as_str().unwrap_or("")))
        .collect()
}

/// A run of `kind` on a piece (a chore of `chore` when given) that starts, then ends with `outcome` and `result`.
async fn run_ends(kind: &str, chore: Option<&str>, outcome: RunOutcome, result: Value) -> Vec<Value> {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build, RunKind::Merge]);
    let t0 = Instant::now();
    let mut taken = run("r1", kind);
    if let Some(chore) = chore {
        taken["piece"]["chore"] = json!({ "kind": chore });
    }
    if kind == "merge" {
        script.lock().unwrap().merges = VecDeque::from([merge_turn("r1", chore)]);
    } else {
        script.lock().unwrap().takes = VecDeque::from([taken]);
    }
    pass(&mut rig, &starter, t0).await;
    let request = starter
        .started
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.token == "r1")
        .map(|r| r.ends.clone());
    request.unwrap().send(RunEnd {
        token: "r1".into(),
        outcome,
        result,
    });
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    events_of(
        &mut rig,
        &[
            "run_start",
            "run_end",
            "needs_person",
            "chore_start",
            "chore_end",
            "merged",
        ],
    )
}

fn merge_turn(token: &str, chore: Option<&str>) -> Value {
    let mut r = turn(token);
    r["turn"]["piece"]["change"] = json!(42);
    if let Some(chore) = chore {
        r["turn"]["piece"]["chore"] = json!({ "kind": chore });
    }
    r
}

fn of<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events.iter().filter(|e| e["kind"] == kind).collect()
}

#[tokio::test]
async fn a_run_writes_its_start_and_its_end_with_kind_piece_outcome_and_duration() {
    let events = run_ends("build", None, RunOutcome::PrOpened, json!({})).await;
    let (start, end) = (&of(&events, "run_start")[0], &of(&events, "run_end")[0]);
    assert_eq!(
        (start["runKind"].as_str(), start["piece"].as_str()),
        (Some("build"), Some("piece-r1"))
    );
    assert_eq!(
        (end["runKind"].as_str(), end["piece"].as_str(), end["outcome"].as_str()),
        (Some("build"), Some("piece-r1"), Some("pr_opened"))
    );
    assert!(end["ms"].as_f64().is_some_and(|ms| ms >= 0.0));
    // The same short name in both, and not the token.
    assert_eq!(start["run"], end["run"]);
    assert!(!start["run"].as_str().unwrap().contains("r1"));
    assert!(of(&events, "needs_person").is_empty());
}

#[tokio::test]
async fn a_failed_run_says_why_and_asks_for_a_person_with_a_reason_code() {
    let why = RunOutcome::Failed("git push rejected: non-fast-forward".into());
    let events = run_ends("build", None, why, json!({})).await;
    let end = of(&events, "run_end")[0];
    assert_eq!(end["outcome"], "failed");
    assert_eq!(end["why"], "git push rejected: non-fast-forward");
    let needs = of(&events, "needs_person")[0];
    assert_eq!(
        (needs["reason"].as_str(), needs["item"].as_str(), needs["from"].as_str()),
        (Some("push_rejected"), Some("piece-r1"), Some("build"))
    );
    assert!(needs["text"].as_str().unwrap().contains("non-fast-forward"));
    assert_eq!(needs["run"], end["run"]);
}

#[tokio::test]
async fn a_run_that_ends_with_questions_asks_for_a_person_by_code() {
    let events = run_ends("build", None, RunOutcome::Questions, json!({})).await;
    assert_eq!(of(&events, "needs_person")[0]["reason"], "questions");
    let events = run_ends("build", None, RunOutcome::PersonSteps, json!({})).await;
    assert_eq!(of(&events, "needs_person")[0]["reason"], "person_steps");
}

#[tokio::test]
async fn a_chore_run_writes_chore_start_and_a_chore_end_with_the_outcome() {
    let events = run_ends("build", Some("tidy"), RunOutcome::NothingLeft, json!({})).await;
    let (start, end) = (of(&events, "chore_start")[0], of(&events, "chore_end")[0]);
    assert_eq!(
        (start["item"].as_str(), start["chore"].as_str(), start["mode"].as_str()),
        (Some("piece-r1"), Some("tidy"), Some("runs"))
    );
    assert_eq!(
        (end["outcome"].as_str(), end["chore"].as_str(), end["mode"].as_str()),
        (Some("nothing_left"), Some("tidy"), Some("runs"))
    );
    assert!(end["ms"].as_f64().is_some());
    // A build that opened its pull request has not ended the chore.
    let events = run_ends("build", Some("tidy"), RunOutcome::PrOpened, json!({})).await;
    assert_eq!(of(&events, "chore_start").len(), 1);
    assert!(of(&events, "chore_end").is_empty());
    let failed = run_ends("build", Some("tidy"), RunOutcome::Failed("x".into()), json!({})).await;
    assert_eq!(of(&failed, "chore_end")[0]["outcome"], "failed");
    let stopped = run_ends("build", Some("tidy"), RunOutcome::Questions, json!({})).await;
    assert_eq!(of(&stopped, "chore_end")[0]["outcome"], "stopped");
}

#[tokio::test]
async fn a_merge_run_ends_the_chore_merged_and_its_merged_event_has_the_commit_and_way() {
    let events = run_ends("merge", Some("tidy"), RunOutcome::Merged, json!({ "commit": "abc123" })).await;
    assert_eq!(of(&events, "chore_end")[0]["outcome"], "merged");
    let merged = of(&events, "merged")[0];
    assert_eq!(
        (merged["pr"].as_u64(), merged["commit"].as_str(), merged["how"].as_str()),
        (Some(42), Some("abc123"), Some("merge"))
    );
    assert_eq!(of(&events, "run_end")[0]["runKind"], "merge");
}
