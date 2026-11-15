//! A session that won't start, and an item handed back again and again (`driver/settle.rs`, section 7.3).
use super::*;

fn done(result: ImplementerOutcome) -> SessionResult {
    SessionResult::Implementer(result)
}

/// A rig with "a" claimed on lane 1, the first session ending as `first` (not yet read).
async fn start(first: SessionResult, before: u32) -> (Rig, Arc<Fake>, Asked) {
    let (base, asked) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    if before > 0 {
        rig.lp(0).own_failures.insert("a".into(), before);
    }
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(first);
    rig.machine.pass().await;
    (rig, fake, asked)
}

fn no_start() -> SessionResult {
    SessionResult::NotStarted("starting claude: busy".into())
}

/// Makes the pending retry of "a" due now and runs a pass, with the next session ending as `next`.
async fn retry_now(rig: &mut Rig, fake: &Fake, next: SessionResult) {
    fake.script.lock().unwrap().push(next);
    rig.lp(0).silenced.insert("a".into(), std::time::Instant::now());
    rig.machine.pass().await;
}

#[tokio::test]
async fn a_session_that_wont_start_keeps_lane_lease_and_run_and_does_not_pause_claiming() {
    let (mut rig, fake, asked) = start(no_start(), 0).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
    assert_eq!(rig.lp(0).store.leases()["a"], "lease-1");
    assert!(rig.machine.claim_paused_until.is_none());
    assert!(
        !ops(&asked)
            .iter()
            .any(|o| o == "release" || o == "park" || o == "report")
    );
    assert_eq!(rig.lp(0).store.own_failures()["a"], 1);
    assert!(
        rig.lp(0).store.runs()["a"].not_started,
        "saved, so a restart starts it fresh instead of resuming an id that never ran"
    );
    assert_eq!(fake.started.lock().unwrap().len(), 1, "not retried at once");
}

#[tokio::test]
async fn a_session_that_wont_start_is_started_fresh_again_after_the_delay() {
    let (mut rig, fake, _) = start(no_start(), 0).await;
    rig.machine.pass().await;
    retry_now(&mut rig, &fake, done(ImplementerOutcome::Finished)).await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 2);
    assert!(!started[1].resume && started[1].lane == 1);
    rig.machine.pass().await;
    assert!(rig.lp(0).store.own_failures().is_empty(), "an answer clears the count");
}

#[tokio::test]
async fn the_fourth_failure_to_start_goes_to_a_person_with_the_error() {
    let (mut rig, fake, _) = start(no_start(), 0).await;
    rig.machine.pass().await;
    for _ in 0..2 {
        retry_now(&mut rig, &fake, no_start()).await;
        rig.machine.pass().await;
        assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    }
    retry_now(&mut rig, &fake, no_start()).await;
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 4, "three retries");
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), None);
    let record = rig.lp(0).store.runs()["a"].clone();
    assert_eq!(record.needs_human_reason.as_deref(), Some("starting claude: busy"));
    assert!(rig.lp(0).store.own_failures().is_empty());
    assert!(rig.machine.claim_paused_until.is_none());
}

#[tokio::test]
async fn a_first_failure_before_work_still_hands_back_with_a_claim_pause_and_is_counted() {
    let (mut rig, _fake, asked) = start(SessionResult::Failed("lane prep".into()), 0).await;
    rig.machine.pass().await;
    assert!(ops(&asked).contains(&"release".to_string()) && rig.lp(0).runs.is_empty());
    assert!(rig.machine.claim_paused_until.is_some());
    assert_eq!(rig.lp(0).store.own_failures()["a"], 1);
}

#[tokio::test]
async fn the_fourth_failure_before_work_in_a_row_goes_to_a_person_instead_of_a_release() {
    let (mut rig, _fake, asked) = start(SessionResult::Failed("lane prep".into()), 3).await;
    rig.machine.pass().await;
    assert!(!ops(&asked).contains(&"release".to_string()));
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert!(rig.machine.claim_paused_until.is_none());
    assert!(rig.lp(0).store.own_failures().is_empty());
}

#[tokio::test]
async fn a_session_that_answers_clears_the_count() {
    let (mut rig, _fake, _) = start(done(ImplementerOutcome::Finished), 2).await;
    rig.machine.pass().await;
    assert!(rig.lp(0).store.own_failures().is_empty());
}
