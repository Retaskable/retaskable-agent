//! A store outage in `runs` mode (the owner's rule, 2026-11-16: "it should be able to finish what it's doing but it
//! can't pick up the next thing till the store is back."): a run in hand is finished and its `run_done` kept and sent
//! when the store is back; nothing new is taken or dropped meanwhile; only the store's own word that the work moved on
//! (`stop`, `stale_run`) ends or drops anything.
use super::*;

fn count(script: &Shared_, op: &str) -> usize {
    script.lock().unwrap().args(op).len()
}

#[tokio::test]
async fn a_run_finished_in_an_outage_keeps_its_end_and_nothing_new_is_taken_until_the_store_is_back() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("a", "build"), run("b", "build")]);
    pass(&mut rig, &starter, t0).await;
    assert_eq!(starter.started(), ["a"]);
    let drops = count(&script, "runs_drop");

    // The store goes away: the beat can't be answered, and that cuts nothing.
    script.lock().unwrap().down = true;
    pass(&mut rig, &starter, t0 + Duration::from_secs(60)).await;
    assert!(count(&script, "run_beat") >= 1, "the beat was tried");
    assert_eq!(count(&script, "run_done"), 0, "a beat that failed doesn't end the run");
    assert_eq!(rig.machine.pool.held_lanes().count(), 1, "its lane is still its own");

    // It finishes meanwhile: the end can't be sent, and stays owed.
    starter.finish("a", RunOutcome::PrOpened);
    pass(&mut rig, &starter, t0 + Duration::from_secs(61)).await;
    assert_eq!(count(&script, "run_done"), 1, "tried at once");
    pass(&mut rig, &starter, t0 + Duration::from_secs(125)).await;
    assert_eq!(count(&script, "run_done"), 2, "and again a minute later");
    assert_eq!(starter.started(), ["a"], "nothing new is taken while the store is out");
    assert_eq!(
        script.lock().unwrap().takes.len(),
        1,
        "the next run was never handed out"
    );
    assert_eq!(count(&script, "runs_drop"), drops, "nothing is let go either");

    // The store is back: the owed end is taken once, then the next run may start.
    script.lock().unwrap().down = false;
    pass(&mut rig, &starter, t0 + Duration::from_secs(190)).await;
    let done = script.lock().unwrap().args("run_done");
    assert_eq!(done.len(), 3);
    assert_eq!(
        (done[2]["token"].as_str(), done[2]["outcome"].as_str()),
        (Some("a"), Some("pr_opened"))
    );
    pass(&mut rig, &starter, t0 + Duration::from_secs(260)).await;
    assert_eq!(count(&script, "run_done"), 3, "sent no more once taken");
    assert_eq!(starter.started(), ["a", "b"], "the next thing is picked up after");
    assert_eq!(count(&script, "runs_drop"), drops);
}

#[tokio::test]
async fn a_stop_from_the_store_is_the_only_beat_that_cuts_a_run() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("a", "build")]);
    pass(&mut rig, &starter, t0).await;
    // The store restores an untouched lapsed run and says `ok`: the run goes on.
    script.lock().unwrap().beat = json!({ "runs": { "a": "ok" } });
    pass(&mut rig, &starter, t0 + Duration::from_secs(60)).await;
    assert_eq!(count(&script, "run_done"), 0);
    // The piece was taken: `stop`, in the store's own wrapping.
    script.lock().unwrap().beat = json!({ "runs": { "a": "stop" } });
    pass(&mut rig, &starter, t0 + Duration::from_secs(120)).await;
    let done = script.lock().unwrap().args("run_done");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0]["outcome"], "interrupted");
}
