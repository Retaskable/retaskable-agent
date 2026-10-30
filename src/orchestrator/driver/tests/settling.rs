//! How sessions and steps that ended settle their item's lane and lease (`driver/settle.rs`).
use super::*;

fn done(result: ImplementerOutcome) -> SessionResult {
    SessionResult::Implementer(result)
}

/// A rig that has claimed "a" on lane 1 with its session ended by `result` (not yet read), the store
/// refusing `release` while `fail_release` is set.
async fn ended(result: SessionResult, fail_release: &Arc<AtomicBool>) -> (Rig, Arc<Fake>, Asked) {
    let (base, asked) = claim_stub_with(json!([item("a", "claimed")]), handed_out(), fail_release.clone()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(result);
    rig.machine.pass().await;
    (rig, fake, asked)
}

#[tokio::test]
async fn a_finished_session_parks_the_run_in_checking_keeping_its_lane_and_lease() {
    let (mut rig, _fake, asked) = ended(done(ImplementerOutcome::Finished), &Default::default()).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Checking);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
    assert_eq!(rig.lp(0).store.leases()["a"], "lease-1");
    assert!(!ops(&asked).contains(&"release".to_string()));
}

#[tokio::test]
async fn a_session_with_questions_for_a_person_parks_the_item_and_frees_its_lane() {
    let asking = ImplementerOutcome::Questions {
        all_already_answered: false,
    };
    let (mut rig, _fake, asked) = ended(done(asking), &Default::default()).await;
    rig.machine.pass().await;
    let parked = asked.lock().unwrap().iter().find(|(op, _)| op == "park").cloned();
    assert_eq!(
        parked.unwrap().1,
        json!({ "id": "a", "token": "lease-1", "for": "answer" })
    );
    assert!(!ops(&asked).contains(&"release".to_string()));
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), None);
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsClarification);
    assert!(rig.lp(0).store.leases().is_empty() && rig.lp(0).store.unparked().is_empty());
    let events = rig.lp(0).events().recent();
    assert!(events.iter().any(|e| e.kind() == "lane_freed"));
    let lease = events.iter().rfind(|e| e.kind() == "lease").unwrap().to_value();
    assert_eq!(
        (lease["change"].clone(), lease["state"].clone()),
        (json!("parked"), json!("needs_clarification"))
    );
}

#[tokio::test]
async fn a_park_the_store_refuses_is_kept_and_sent_again_on_the_next_pass() {
    let fail = Arc::new(AtomicBool::new(true));
    let asking = ImplementerOutcome::Questions {
        all_already_answered: false,
    };
    let (mut rig, _fake, asked) = ended(done(asking), &fail).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).store.unparked()["a"].0, "lease-1");
    assert!(rig.lp(0).store.leases().is_empty());
    assert!(
        rig.lp(0)
            .events()
            .recent()
            .iter()
            .all(|e| e.to_value()["change"] != "parked")
    );
    fail.store(false, Ordering::SeqCst);
    rig.machine.pass().await;
    assert!(rig.lp(0).store.unparked().is_empty());
    assert_eq!(ops(&asked).iter().filter(|o| *o == "park").count(), 2);
    assert!(
        rig.lp(0)
            .events()
            .recent()
            .iter()
            .any(|e| e.to_value()["change"] == "parked")
    );
}

#[tokio::test]
async fn a_session_that_needs_a_human_frees_its_lane_but_keeps_its_lease_as_ts_does() {
    let questions = ImplementerOutcome::Questions {
        all_already_answered: true,
    };
    let (mut rig, fake, asked) = ended(done(questions), &Default::default()).await;
    rig.lp(0).runs.get_mut("a").unwrap().reasks = crate::decisions::MAX_REASKS;
    rig.machine.pass().await;
    assert!(fake.started.lock().unwrap().len() == 1);
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), None);
    assert_eq!(
        rig.lp(0).store.leases()["a"],
        "lease-1",
        "still this computer's, not handed out again"
    );
    assert!(!ops(&asked).iter().any(|o| o == "release" || o == "park"));
    let events = rig.lp(0).events().recent();
    let freed = events.iter().find(|e| e.kind() == "lane_freed").unwrap().to_value();
    assert_eq!((freed["item"].clone(), freed["lane"].clone()), (json!("a"), json!(1)));
}

#[tokio::test]
async fn a_start_the_starter_refuses_releases_the_item_at_once() {
    let (base, asked) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    *fake.refuse.lock().unwrap() = Some("no room".into());
    rig.machine.pass().await;
    assert_eq!(ops(&asked), ["list", "claim_next", "release"]);
    assert!(rig.lp(0).runs.is_empty() && rig.machine.pool.held_lanes().count() == 0);
    assert!(rig.lp(0).store.leases().is_empty() && rig.lp(0).store.unreleased().is_empty());
    let freed = rig
        .lp(0)
        .events()
        .recent()
        .iter()
        .find(|e| e.kind() == "lane_freed")
        .unwrap()
        .to_value();
    assert!(freed["why"].as_str().unwrap().contains("no room"));
}

#[tokio::test]
async fn a_failed_session_gives_the_item_back() {
    let (mut rig, _fake, asked) = ended(SessionResult::Failed("git is gone".into()), &Default::default()).await;
    rig.machine.pass().await;
    assert!(ops(&asked).contains(&"release".to_string()) && rig.lp(0).runs.is_empty());
    assert_eq!(rig.machine.pool.held_lanes().count(), 0);
}

#[tokio::test]
async fn a_release_the_store_refuses_is_kept_and_sent_again_on_the_next_pass() {
    let fail = Arc::new(AtomicBool::new(true));
    let (mut rig, _fake, asked) = ended(SessionResult::Failed("x".into()), &fail).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).store.unreleased()["a"], "lease-1");
    assert!(rig.lp(0).store.leases().is_empty());
    assert_eq!(rig.machine.pool.held_lanes().count(), 0, "the lane is free either way");
    fail.store(false, Ordering::SeqCst);
    rig.machine.pass().await;
    assert!(rig.lp(0).store.unreleased().is_empty());
    let releases = ops(&asked).iter().filter(|o| *o == "release").count();
    assert_eq!(releases, 2, "once refused, once taken");
}

#[tokio::test]
async fn the_unreleased_list_survives_a_restart() {
    let fail = Arc::new(AtomicBool::new(true));
    let (mut rig, _fake, _asked) = ended(SessionResult::Failed("x".into()), &fail).await;
    rig.machine.pass().await;
    let dir = rig._dir.path().join("0");
    let reloaded = Store::load(&dir).unwrap();
    assert_eq!(reloaded.unreleased()["a"], "lease-1");
    assert!(reloaded.leases().is_empty());
}

#[tokio::test]
async fn a_zero_poll_interval_is_a_second_not_a_panic() {
    assert_eq!(poll_period(Duration::ZERO), Duration::from_secs(1));
    assert_eq!(poll_period(Duration::from_secs(10)), Duration::from_secs(10));
    drop(tokio::time::interval(poll_period(Duration::ZERO)));
}

#[tokio::test]
async fn a_finished_session_is_stepped_next_and_a_step_that_lands_frees_the_lane_keeping_the_lease() {
    let (mut rig, fake, _asked) = ended(done(ImplementerOutcome::Finished), &Default::default()).await;
    assert!(
        fake.stepped.lock().unwrap().is_empty(),
        "an implementing item's step is its session"
    );
    *fake.to.lock().unwrap() = Some(ItemState::AwaitingMerge);
    rig.machine.poll().await;
    let stepped = fake.stepped.lock().unwrap().clone();
    assert_eq!(
        (stepped.len(), stepped[0].run.state, stepped[0].token.as_str()),
        (1, ItemState::Checking, "lease-1")
    );
    settle().await;
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::AwaitingMerge);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), None);
    assert_eq!(rig.lp(0).store.leases()["a"], "lease-1");
    let freed = (rig.lp(0).events().recent().iter())
        .rfind(|e| e.kind() == "lane_freed")
        .unwrap()
        .to_value();
    assert_eq!(freed["why"], "awaiting_merge: the step ended");
}

#[tokio::test]
async fn a_step_that_leaves_questions_for_a_person_parks_the_item() {
    let (base, asked) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    at(&mut rig, 0, "a", ItemState::Reviewing, Some(1));
    rig.lp(0).leases.insert("a".into(), "lease-9".into());
    *fake.to.lock().unwrap() = Some(ItemState::NeedsClarification);
    rig.machine.poll().await;
    settle().await;
    rig.machine.poll().await;
    let parked = asked.lock().unwrap().iter().find(|(op, _)| op == "park").cloned();
    assert_eq!(
        parked.unwrap().1,
        json!({ "id": "a", "token": "lease-9", "for": "answer" })
    );
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), None);
}
