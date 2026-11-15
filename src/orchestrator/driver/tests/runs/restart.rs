//! A restart's `merge_open`, the `claim` event's `op`, a result kept for a store that can't be reached, and a red
//! main's fix-main chore going through the queue (docs/specs/merge-queue-from-store.md sections 3.5 to 3.8).
use super::*;

fn claims(rig: &mut Rig) -> Vec<Value> {
    (rig.lp(0).events().recent().iter())
        .map(Entry::to_value)
        .filter(|e| e["kind"] == "claim")
        .collect()
}

#[tokio::test]
async fn a_restart_asks_merge_open_before_any_ask_and_resumes_the_held_turn() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build, RunKind::Merge]);
    script.lock().unwrap().open = turn("m9")["turn"].clone();
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build")]);
    pass(&mut rig, &starter, Instant::now()).await;
    let s = script.lock().unwrap();
    assert_eq!(s.ops()[..2], ["runs_drop", "merge_open"]);
    assert!(!s.args("run_take").is_empty());
    assert!(s.args("merge_next").is_empty(), "the held turn is not asked for again");
    assert_eq!(starter.started(), vec!["m9", "b1"]);
    let context = starter.started.lock().unwrap()[0].context.clone();
    assert!(
        context.locks.held("m9"),
        "the turn is the run's, beaten with merge_beat"
    );
}

#[tokio::test]
async fn with_no_held_turn_the_computer_goes_back_to_merge_next_and_asks_merge_open_once() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Merge]);
    let t0 = Instant::now();
    script.lock().unwrap().merges = VecDeque::from([turn("m1")]);
    pass(&mut rig, &starter, t0).await;
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    let s = script.lock().unwrap();
    assert_eq!(s.ops()[..3], ["runs_drop", "merge_open", "merge_next"]);
    assert_eq!(s.args("merge_open").len(), 1);
    assert_eq!(starter.started(), vec!["m1"]);
}

#[tokio::test]
async fn a_store_that_cant_say_what_it_holds_gets_no_ask_and_no_run() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build, RunKind::Merge]);
    let t0 = Instant::now();
    {
        let mut s = script.lock().unwrap();
        s.open_fails = true;
        s.merges = VecDeque::from([turn("m1")]);
    }
    pass(&mut rig, &starter, t0).await;
    assert_eq!(script.lock().unwrap().ops(), vec!["runs_drop", "merge_open"]);
    assert!(starter.started().is_empty());
    script.lock().unwrap().open_fails = false;
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    assert_eq!(starter.started(), vec!["m1"]);
}

#[tokio::test]
async fn a_held_turn_with_no_piece_is_given_back_released() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Merge]);
    script.lock().unwrap().open = json!({ "token": "t7", "entry": "e" });
    pass(&mut rig, &starter, Instant::now()).await;
    assert_eq!(
        script.lock().unwrap().args("merge_result"),
        vec![json!({ "token": "t7", "outcome": "released" })]
    );
    assert!(starter.started().is_empty());
}

#[tokio::test]
async fn the_claim_event_names_its_op_for_run_take_and_merge_next() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build, RunKind::Merge]);
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build")]);
    script.lock().unwrap().merges = VecDeque::from([json!({ "waiting": { "place": 2, "ahead": 1 } })]);
    pass(&mut rig, &starter, Instant::now()).await;
    let ops: Vec<_> = claims(&mut rig).iter().map(|c| c["op"].clone()).collect();
    assert!(
        ops.contains(&json!("merge_next")) && ops.contains(&json!("run_take")),
        "{ops:?}"
    );
    let take = claims(&mut rig).into_iter().find(|c| c["op"] == "run_take").unwrap();
    assert_eq!(take["answer"], json!("b1"));
}

#[tokio::test]
async fn a_result_the_store_couldnt_take_is_kept_and_sent_when_it_is_back() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Merge]);
    let t0 = Instant::now();
    script.lock().unwrap().merges = VecDeque::from([turn("m1")]);
    pass(&mut rig, &starter, t0).await;
    script.lock().unwrap().result_fails = true;
    starter.finish("m1", RunOutcome::Merged);
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    assert_eq!(script.lock().unwrap().args("merge_result").len(), 1);
    script.lock().unwrap().result_fails = false;
    pass(&mut rig, &starter, t0 + Duration::from_secs(120)).await;
    let s = script.lock().unwrap();
    let sent = s.args("merge_result");
    assert_eq!(sent.len(), 2, "sent again once the store answers");
    assert_eq!(sent[1]["outcome"], json!("merged"));
    assert!(s.args("run_done").is_empty());
}

#[tokio::test]
async fn a_red_mains_fix_main_chore_merges_through_the_queue_like_any_change() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Merge]);
    let t0 = Instant::now();
    let mut t = turn("m1");
    t["turn"]["piece"]["chore"] = json!({ "kind": "fix_main", "report": "build is red" });
    script.lock().unwrap().merges = VecDeque::from([t]);
    pass(&mut rig, &starter, t0).await;
    assert_eq!(starter.started(), vec!["m1"], "its merge takes merge_next");
    starter.finish("m1", RunOutcome::Merged);
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    let s = script.lock().unwrap();
    assert!(
        s.args("lock_take")
            .iter()
            .all(|l| !l["name"].as_str().unwrap_or("").starts_with("merge-"))
    );
    assert_eq!(s.args("merge_result")[0]["outcome"], json!("merged"));
}
