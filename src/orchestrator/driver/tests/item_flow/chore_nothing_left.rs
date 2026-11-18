//! A chore whose session found nothing to do (`driver/nothing_left.rs`; `nothingLeft` in the TS agent's `steps.ts`): it is
//! not a stop that needs a person, so it is never parked and reported `needs_human` (the store may turn a stopped chore
//! into a needs-you item); it is closed and its task ends ok.
use super::super::*;
use super::chores::{live, sent};

#[tokio::test]
async fn a_chore_with_nothing_to_do_ends_its_task_ok_and_is_never_reported_stopped() {
    let (mut rig, fake, asked, _plan) = live("chore:tidy").await;
    fake.script
        .lock()
        .unwrap()
        .push(SessionResult::NothingLeft("Nothing to do. Clippy is clean.".into()));
    for _ in 0..4 {
        rig.machine.pass().await;
        settle().await;
    }
    assert_eq!(sent(&asked, "task_end"), [json!({ "id": "task-c", "ok": true })]);
    for health in sent(&asked, "health") {
        assert!(
            health["running"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r.get("state").is_none()),
            "a stop was reported: {health}"
        );
    }
    assert!(sent(&asked, "chore_nofix").is_empty());
    let lp = rig.lp(0);
    assert!(lp.runs.is_empty() && lp.chores.is_empty() && lp.records.is_empty());
    assert_eq!(rig.machine.pool.lane_of("p0", "task-c"), None, "its lane is free");
}
