//! How sessions and steps that ended settle their item's lane and lease (`driver/settle.rs`).
use super::*;
use crate::orchestrator::driver::machine::poll_period;

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
    assert_eq!(ops(&asked), ["list", "claim_next", "report", "release"]);
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
async fn an_interrupted_session_keeps_the_item_implementing_and_sends_no_release_or_park() {
    let (mut rig, _fake, asked) = ended(SessionResult::Interrupted, &Default::default()).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
    assert_eq!(rig.lp(0).store.leases()["a"], "lease-1");
    assert!(
        !ops(&asked)
            .iter()
            .any(|o| o == "release" || o == "park" || o == "report")
    );
    let events = rig.lp(0).events().recent();
    let ended = events.iter().find(|e| e.kind() == "session_end").unwrap().to_value();
    assert_eq!(ended["ok"], false);
    assert_eq!(ended["subtype"], "interrupted");
}

#[tokio::test]
async fn an_own_failure_pauses_claiming_on_this_project_for_thirty_minutes() {
    let (mut rig, _fake, asked) = ended(SessionResult::Failed("git is gone".into()), &Default::default()).await;
    rig.machine.pass().await;
    assert!(rig.machine.claim_paused_until.is_some());
    let events = rig.lp(0).events().recent();
    let error = events.iter().find(|e| e.kind() == "error").unwrap().to_value();
    assert_eq!(
        (error["where"].clone(), error["item"].clone()),
        (json!("start"), json!("a"))
    );
    asked.lock().unwrap().clear();
    rig.machine.pass().await;
    assert!(!ops(&asked).contains(&"claim_next".to_string()), "{:?}", ops(&asked));
}

#[tokio::test]
async fn a_claim_pause_that_has_passed_lets_claiming_resume() {
    let (mut rig, _fake, asked) = ended(SessionResult::Failed("git is gone".into()), &Default::default()).await;
    rig.machine.pass().await;
    rig.machine.claim_paused_until = Some(std::time::Instant::now() - Duration::from_secs(1));
    asked.lock().unwrap().clear();
    rig.machine.pass().await;
    assert!(ops(&asked).contains(&"claim_next".to_string()), "{:?}", ops(&asked));
}

#[tokio::test]
async fn an_own_failure_on_one_project_pauses_claiming_for_every_project_on_this_computer() {
    let (base0, asked0) = claim_stub_with(json!([item("a", "claimed")]), handed_out(), Default::default()).await;
    let (base1, asked1) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base0, &base1]);
    let fake = rig.go_live();
    fake.script
        .lock()
        .unwrap()
        .push(SessionResult::Failed("git is gone".into()));
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert!(rig.machine.claim_paused_until.is_some());
    asked0.lock().unwrap().clear();
    asked1.lock().unwrap().clear();
    rig.machine.pass().await;
    assert!(!ops(&asked0).contains(&"claim_next".to_string()), "{:?}", ops(&asked0));
    assert!(
        !ops(&asked1).contains(&"claim_next".to_string()),
        "the pause is computer-wide, not just p0's: {:?}",
        ops(&asked1)
    );
}

#[tokio::test]
async fn a_session_that_finds_claude_signed_out_needs_a_human_and_blocks_new_starts_computer_wide() {
    let why = "The session ended without answering: found Claude signed out on this computer (Not logged in)";
    let (base0, asked0) = claim_stub_with(json!([item("a", "claimed")]), handed_out(), Default::default()).await;
    let (base1, asked1) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base0, &base1]);
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(SessionResult::NeedsHuman(why.into()));
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(
        rig.lp(0).runs["a"].state,
        ItemState::NeedsHuman,
        "still a person's to look at"
    );
    assert_eq!(rig.lp(0).store.leases()["a"], "lease-1", "kept, not released");
    assert!(rig.machine.claude_signed_out_until.is_some());
    asked0.lock().unwrap().clear();
    asked1.lock().unwrap().clear();
    rig.machine.pass().await;
    assert!(!ops(&asked0).contains(&"claim_next".to_string()), "{:?}", ops(&asked0));
    assert!(
        !ops(&asked1).contains(&"claim_next".to_string()),
        "the gate is computer-wide, not just p0's: {:?}",
        ops(&asked1)
    );
}

#[tokio::test]
async fn a_signed_out_pause_that_has_passed_lets_starts_resume() {
    let why = "The session ended without answering: found Claude signed out on this computer (Not logged in)";
    let (base, asked) = claim_stub_with(json!([item("a", "claimed")]), handed_out(), Default::default()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(SessionResult::NeedsHuman(why.into()));
    rig.machine.pass().await;
    rig.machine.pass().await;
    rig.machine.claude_signed_out_until = Some(std::time::Instant::now() - Duration::from_secs(1));
    asked.lock().unwrap().clear();
    rig.machine.pass().await;
    assert!(ops(&asked).contains(&"claim_next".to_string()), "{:?}", ops(&asked));
}

#[tokio::test]
async fn an_ordinary_needs_human_session_leaves_the_signed_out_gate_alone() {
    let questions = ImplementerOutcome::Questions {
        all_already_answered: true,
    };
    let (mut rig, _fake, _asked) = ended(done(questions), &Default::default()).await;
    rig.lp(0).runs.get_mut("a").unwrap().reasks = crate::decisions::MAX_REASKS;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert!(rig.machine.claude_signed_out_until.is_none());
}

#[tokio::test]
async fn a_release_the_store_refuses_is_kept_and_sent_again_on_the_next_pass() {
    let fail = Arc::new(AtomicBool::new(true));
    let (mut rig, _fake, asked) = ended(SessionResult::Failed("x".into()), &fail).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).store.unreleased()["a"].0, "lease-1");
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
    assert_eq!(reloaded.unreleased()["a"], ("lease-1".to_string(), "x".to_string()));
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

#[tokio::test]
async fn a_session_that_needs_a_human_frees_its_lane_and_keeps_the_lease() {
    let (mut rig, _fake, asked) = ended(SessionResult::NeedsHuman("gave up".into()), &Default::default()).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), None);
    assert_eq!(rig.lp(0).store.leases()["a"], "lease-1");
    assert!(!ops(&asked).contains(&"release".to_string()));
    let events = rig.lp(0).events().recent();
    let freed = events.iter().find(|e| e.kind() == "lane_freed").unwrap().to_value();
    assert_eq!(freed["why"], "needs_human: gave up");
    let ended = events.iter().find(|e| e.kind() == "session_end").unwrap().to_value();
    assert_eq!(ended["ok"], false);
    assert!(
        rig.machine.claim_paused_until.is_none(),
        "a needs-human outcome never pauses claiming"
    );
}

#[tokio::test]
async fn a_starter_with_no_step_for_a_state_leaves_the_item_and_logs_nothing() {
    struct NoSteps;
    impl SessionStarter for NoSteps {
        fn start(&self, _request: StartRequest) -> Result<(), String> {
            Ok(())
        }
        fn active(&self) -> u32 {
            0
        }
        fn advance(&self, _request: StepRequest) -> Option<BoxFuture<'static, ItemRun>> {
            None
        }
    }
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.starter = Some(Arc::new(NoSteps));
    at(&mut rig, 0, "a", ItemState::Checking, Some(1));
    rig.machine.poll().await;
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Checking);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
    assert!(!kinds(&mut rig).contains(&"step_start".to_string()));
}
