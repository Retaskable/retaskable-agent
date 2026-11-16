//! A restart in the middle of a store outage keeps a finished run's end (the owner's rule, 2026-11-16: "it should be
//! able to finish what it's doing but it can't pick up the next thing till the store is back."): the end is in the
//! state file before it is sent, is sent again at once on the next start and before `runs_drop` (which would end the
//! run as `interrupted`), and goes when the store answers.
use super::super::restart::restarted;
use super::*;

fn on_disk(rig: &Rig) -> Vec<(String, Value)> {
    let owed = Store::load(&rig._dir.path().join("0")).unwrap().run_ends_owed();
    owed.into_iter().map(|e| (e.token, e.outcome)).collect()
}

fn ops(script: &Shared_, names: &[&str]) -> Vec<String> {
    let all = script.lock().unwrap().ops();
    all.into_iter().filter(|o| names.contains(&o.as_str())).collect()
}

/// A computer with its store's address, which a restart goes back to.
async fn start() -> (Rig, Shared_, String) {
    let (base, script) = store().await;
    (rig(1, &[&base]), script, base)
}

/// A run "a" that finished while the store was down, the computer then stopped: its end on disk, nothing taken.
async fn finished_in_an_outage(starter: &FakeRuns, t0: Instant) -> (Rig, Shared_, String) {
    let (mut rig, script, base) = start().await;
    script.lock().unwrap().takes = VecDeque::from([run("a", "build"), run("b", "build")]);
    pass(&mut rig, starter, t0).await;
    script.lock().unwrap().down = true;
    starter.finish("a", RunOutcome::PrOpened);
    pass(&mut rig, starter, t0 + Duration::from_secs(1)).await;
    assert_eq!(on_disk(&rig), [("a".to_string(), json!({ "outcome": "pr_opened" }))]);
    (rig, script, base)
}

#[tokio::test]
async fn a_restart_in_an_outage_sends_the_owed_end_once_the_store_is_back_and_only_then_drops_and_asks() {
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    let (rig, script, base) = finished_in_an_outage(&starter, t0).await;
    let mut rig = restarted(rig, &base);
    let after = FakeRuns::new(&[RunKind::Build]);

    // Still out: the end is tried at once, and nothing is dropped or taken.
    let before = script.lock().unwrap().log.len();
    pass(&mut rig, &after, t0 + Duration::from_secs(2)).await;
    let tried: Vec<String> = script.lock().unwrap().ops()[before..].to_vec();
    assert_eq!(tried, ["run_done"], "tried at once, and no runs_drop or run_take");
    assert_eq!(on_disk(&rig).len(), 1, "still owed");

    // Back: taken once, and runs_drop and the asks come after it.
    script.lock().unwrap().down = false;
    pass(&mut rig, &after, t0 + Duration::from_secs(70)).await;
    let (done, seq) = {
        let s = script.lock().unwrap();
        (s.args("run_done"), s.ops()[before + 1..].to_vec())
    };
    assert_eq!(
        done.len(),
        3,
        "once before the restart, once after it, and the one that was taken"
    );
    assert_eq!(
        (done[2]["token"].as_str(), done[2]["outcome"].as_str()),
        (Some("a"), Some("pr_opened"))
    );
    assert_eq!(
        seq[..3],
        ["run_done", "runs_drop", "run_take"],
        "the result before the drop"
    );
    assert!(on_disk(&rig).is_empty(), "off the disk once the store took it");
    pass(&mut rig, &after, t0 + Duration::from_secs(140)).await;
    assert_eq!(ops(&script, &["run_done"]).len(), 3, "taken once");
}

#[tokio::test]
async fn an_owed_end_the_store_says_moved_on_is_dropped_and_not_sent_again() {
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    let (rig, script, base) = finished_in_an_outage(&starter, t0).await;
    let mut rig = restarted(rig, &base);
    let after = FakeRuns::new(&[RunKind::Build]);
    {
        let mut s = script.lock().unwrap();
        s.down = false;
        s.done_refused = Some("stale_run");
    }
    pass(&mut rig, &after, t0 + Duration::from_secs(2)).await;
    assert!(on_disk(&rig).is_empty());
    pass(&mut rig, &after, t0 + Duration::from_secs(200)).await;
    assert_eq!(
        script.lock().unwrap().args("run_done").len(),
        2,
        "one try after the restart, no more"
    );
}

#[tokio::test]
async fn a_store_that_answers_with_a_refusal_has_been_reached_so_runs_drop_goes_and_the_end_stays_owed() {
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    let (rig, script, base) = finished_in_an_outage(&starter, t0).await;
    let mut rig = restarted(rig, &base);
    let after = FakeRuns::new(&[RunKind::Build]);
    {
        let mut s = script.lock().unwrap();
        s.down = false;
        s.done_refused = Some("bad_request");
    }
    pass(&mut rig, &after, t0 + Duration::from_secs(2)).await;
    assert_eq!(
        ops(&script, &["runs_drop"]).len(),
        2,
        "it was reached, so the start goes on"
    );
    assert_eq!(
        on_disk(&rig).len(),
        1,
        "and the refused end is kept for the usual retry"
    );
}

#[tokio::test]
async fn an_end_kept_more_than_a_day_is_given_up_on_at_the_start() {
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    let (mut rig, script, base) = finished_in_an_outage(&starter, t0).await;
    let store = &mut rig.lp(0).store;
    let mut owed = store.run_ends_owed();
    owed[0].since -= crate::runs::OWED_KEPT_SECS + 1;
    store.set_run_ends_owed(&owed);
    store.save().unwrap();
    let mut rig = restarted(rig, &base);
    script.lock().unwrap().down = false;
    let before = script.lock().unwrap().args("run_done").len();
    pass(&mut rig, &FakeRuns::new(&[RunKind::Build]), t0 + Duration::from_secs(2)).await;
    assert_eq!(script.lock().unwrap().args("run_done").len(), before, "not sent");
    assert!(on_disk(&rig).is_empty());
}

#[tokio::test]
async fn a_merge_turns_owed_result_is_sent_as_merge_result_after_a_restart() {
    let (mut rig, script, base) = start().await;
    let starter = FakeRuns::new(&[RunKind::Merge]);
    let t0 = Instant::now();
    script.lock().unwrap().merges = VecDeque::from([turn("m1")]);
    pass(&mut rig, &starter, t0).await;
    script.lock().unwrap().down = true;
    starter.finish("m1", RunOutcome::Merged);
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    assert_eq!(on_disk(&rig).len(), 1);
    let mut rig = restarted(rig, &base);
    script.lock().unwrap().down = false;
    let before = script.lock().unwrap().log.len();
    pass(&mut rig, &FakeRuns::new(&[RunKind::Merge]), t0 + Duration::from_secs(2)).await;
    let s = script.lock().unwrap();
    let seq: Vec<String> = s.ops()[before..].to_vec();
    assert_eq!(seq[..2], ["merge_result", "runs_drop"]);
    assert_eq!(s.args("merge_result").last().unwrap()["token"], "m1");
    drop(s);
    assert!(on_disk(&rig).is_empty());
}
