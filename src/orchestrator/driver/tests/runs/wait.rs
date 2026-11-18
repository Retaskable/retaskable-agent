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
