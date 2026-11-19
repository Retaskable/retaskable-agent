//! The run loop and the store's `wait` (pools-on-server.md section 14.4).
use super::*;

#[tokio::test]
async fn a_computer_that_takes_tasks_takes_no_piece_until_the_store_says_nothing_holds_it() {
    use crate::orchestrator::driver::pool::Wait;
    let (mut rig, script) = setup(1).await;
    rig.machine.projects[0].takes_tasks = true;
    let starter = FakeRuns::new(&[RunKind::Watch]);
    // No `health` answer yet: what the store holds is unknown, so nothing is asked (fail closed).
    pass(&mut rig, &starter, Instant::now()).await;
    assert!(script.lock().unwrap().args("run_take").is_empty());
    // The store's `wait` is set: still nothing.
    rig.machine.projects[0].pool.wait = Wait::Held("paused".into());
    pass(&mut rig, &starter, Instant::now()).await;
    assert!(script.lock().unwrap().args("run_take").is_empty());
    // A store that can't be read again: back to unknown.
    rig.machine.projects[0].pool.wait = Wait::Unknown;
    pass(&mut rig, &starter, Instant::now()).await;
    assert!(script.lock().unwrap().args("run_take").is_empty());
    // Clear: asked.
    rig.machine.projects[0].pool.wait = Wait::Clear;
    pass(&mut rig, &starter, Instant::now()).await;
    assert!(!script.lock().unwrap().args("run_take").is_empty());
}

#[tokio::test]
async fn a_push_while_a_watch_runs_beats_at_once_but_not_more_than_every_few_seconds() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Watch]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("w", "watch")]);
    pass(&mut rig, &starter, t0).await;
    // The first beat is due a minute after the first pass; the store's `stop` for a watch it settled itself comes then.
    script.lock().unwrap().beat = json!({ "w": "ok" });
    pass(&mut rig, &starter, t0 + Duration::from_secs(60)).await;
    let beats = || script.lock().unwrap().args("run_beat").len();
    assert_eq!(beats(), 1);
    // A passed `ci_finish` makes the store push `work_available` and end the watch: the beat that says so goes now.
    pass(&mut rig, &starter, t0 + Duration::from_secs(70)).await;
    assert_eq!(beats(), 1, "no push, no early beat");
    rig.wake();
    script.lock().unwrap().beat = json!({ "w": "stop" });
    pass(&mut rig, &starter, t0 + Duration::from_secs(71)).await;
    assert_eq!(beats(), 2, "the push beat at once");
    let done = script.lock().unwrap().args("run_done");
    assert_eq!(done[0]["outcome"], "interrupted", "the store's stop ended the watch");
}

#[tokio::test]
async fn pushes_close_together_and_pushes_with_no_watch_do_not_beat_early() {
    let (mut rig, script) = setup(1).await;
    let t0 = Instant::now();
    let beats = || script.lock().unwrap().args("run_beat").len();
    // A build is live, not a watch: a push beats nothing early.
    let starter = FakeRuns::new(&[RunKind::Build, RunKind::Watch]);
    script.lock().unwrap().takes = VecDeque::from([run("b", "build")]);
    pass(&mut rig, &starter, t0).await;
    pass(&mut rig, &starter, t0 + Duration::from_secs(60)).await;
    assert_eq!(beats(), 1);
    rig.wake();
    pass(&mut rig, &starter, t0 + Duration::from_secs(70)).await;
    assert_eq!(beats(), 1, "a build is not waiting on CI");
    // With a watch live too, a push beats at once; the next, a second later, waits for the gap.
    script.lock().unwrap().takes = VecDeque::from([run("w", "watch")]);
    rig.wake();
    pass(&mut rig, &starter, t0 + Duration::from_secs(71)).await;
    rig.wake();
    pass(&mut rig, &starter, t0 + Duration::from_secs(72)).await;
    assert_eq!(beats(), 2, "the watch's push beat");
    let before = beats();
    rig.wake();
    pass(&mut rig, &starter, t0 + Duration::from_secs(73)).await;
    assert_eq!(beats(), before, "within the gap of the last beat");
}
