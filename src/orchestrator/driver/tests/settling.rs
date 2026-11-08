//! How sessions and steps that ended settle their item's lane and lease (`driver/settle.rs`).
use super::*;
use crate::engines::EngineId;
use crate::orchestrator::driver::machine::poll_period;
use crate::orchestrator::steps::state_name;
use crate::telemetry::{Event, event};

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

#[tokio::test]
async fn a_session_ending_saves_its_context_size_and_a_fresh_start_clears_it() {
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    *fake.context_tokens.lock().unwrap() = Some(42_000);
    fake.script.lock().unwrap().push(done(ImplementerOutcome::Finished));
    rig.machine.pass().await;
    assert_eq!(
        rig.lp(0).records["a"].impl_context_tokens,
        None,
        "a fresh start has no size"
    );
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).records["a"].impl_context_tokens, Some(42_000));
    assert_eq!(rig.lp(0).store.runs()["a"].impl_context_tokens, Some(42_000));
}

#[tokio::test]
async fn a_session_that_hit_its_engines_limit_marks_that_engine_limited_until_its_reset() {
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(SessionResult::Limited {
        why: "hit the usage limit".into(),
        resets_in: Some(Duration::from_secs(600)),
    });
    rig.machine.pass().await;
    rig.machine.pass().await;
    let now = std::time::Instant::now();
    assert!(
        rig.machine
            .engine_limits
            .is_limited(crate::engines::EngineId::Claude, now)
    );
    assert!(
        !rig.machine
            .engine_limits
            .is_limited(crate::engines::EngineId::Claude, now + Duration::from_secs(601))
    );
    assert!(
        !rig.machine
            .engine_limits
            .is_limited(crate::engines::EngineId::Cursor, now)
    );
}

#[tokio::test]
async fn a_run_that_goes_to_needs_human_keeps_the_stops_triage_facts_in_its_record() {
    let (mut rig, _fake, _asked) = ended(SessionResult::NeedsHuman("first problem".into()), &Default::default()).await;
    // No triager here: its notes are the triage tests'.
    rig.lp(0).project.triage = crate::config::OnOff::Off;
    rig.machine.pass().await;
    let lp = rig.lp(0);
    let r = lp.records["a"].clone();
    assert_eq!(r.state, "needs_human");
    assert_eq!(r.needs_human_reason.as_deref(), Some("first problem"));
    assert_eq!(r.failed_in.as_deref(), Some("implementing"));
    assert!(!r.triaged && r.failed_looks == 0 && r.next_look_at.is_none());
    // The marks survive a rewrite of the run's own fields, and the context reads the record.
    lp.records.get_mut("a").unwrap().triages = Some((r.slice, 1));
    lp.records.get_mut("a").unwrap().triaged = true;
    lp.remember("a", None, None, None);
    assert!(lp.records["a"].triaged);
    let ctx = lp.triage_context("a", std::path::Path::new("data")).unwrap();
    assert_eq!(
        (ctx.reason.as_str(), ctx.failed_in.as_str()),
        ("first problem", "implementing")
    );
    assert_eq!(ctx.automatic_decisions, 1);
    assert!(
        ctx.timeline.is_empty(),
        "no noted item_state event yet: {:?}",
        ctx.timeline
    );
    for note in ["older", "newer"] {
        lp.events.add_in(
            "p0",
            Event::ItemState(event::ItemState {
                item: Some("a".into()),
                note: Some(note.into()),
                ..Default::default()
            }),
        );
    }
    let ctx = lp.triage_context("a", std::path::Path::new("data")).unwrap();
    assert_eq!(ctx.timeline, ["newer", "older"], "newest first");
}

#[tokio::test]
async fn a_run_that_needs_a_person_saves_its_stop_and_a_second_stop_clears_the_first_ones_marks() {
    let why = "Implementer gave up: the build is broken";
    let (base, _asked) = claim_stub_with(json!([item("a", "claimed")]), handed_out(), Default::default()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(SessionResult::NeedsHuman(why.into()));
    rig.machine.pass().await;
    rig.machine.pass().await;
    let record = rig.lp(0).records["a"].clone();
    assert_eq!(record.state, "needs_human");
    assert_eq!(record.needs_human_reason.as_deref(), Some(why));
    assert_eq!(record.failed_in.as_deref(), Some("implementing"));
    assert!(!record.triaged && record.failed_looks == 0 && record.next_look_at.is_none());
    let back: crate::store::RunRecord = serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
    assert_eq!(back, record);
    assert_eq!(rig.lp(0).store.runs()["a"], record, "saved in the state file");

    // Claude looked at this stop and decided once; a later stop on the same slice clears the look marks
    // but keeps the count, and a new slice resets it.
    let slice = record.slice;
    let r = rig.lp(0).records.get_mut("a").unwrap();
    (r.triaged, r.failed_looks, r.triages) = (true, 2, Some((slice, 1)));
    r.new_stop("merging", "Merge failed", slice);
    assert!(!r.triaged && r.failed_looks == 0);
    assert_eq!(
        (r.needs_human_reason.as_deref(), r.triages),
        (Some("Merge failed"), Some((slice, 1)))
    );
    r.new_stop("merging", "Merge failed again", slice + 1);
    assert_eq!(r.triages, None);
}

#[tokio::test]
async fn a_step_that_escalates_saves_its_own_reason_not_an_earlier_note() {
    for (from, to_reason) in [
        (ItemState::Checking, "The checks kept failing"),
        (ItemState::Reviewing, "The review escalated"),
        (ItemState::Merging, "The merge failed"),
    ] {
        let (mut rig, fake, _asked) = ended(done(ImplementerOutcome::Finished), &Default::default()).await;
        rig.lp(0).events.add_in(
            "p0",
            Event::ItemState(event::ItemState {
                item: Some("a".into()),
                note: Some("an implementer progress note".into()),
                ..Default::default()
            }),
        );
        rig.lp(0).runs.get_mut("a").unwrap().state = from;
        *fake.to.lock().unwrap() = Some(ItemState::NeedsHuman);
        rig.machine.poll().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        rig.machine.poll().await;
        let r = &rig.lp(0).records["a"];
        assert_eq!(r.state, "needs_human", "{from:?}");
        assert!(
            r.needs_human_reason.as_deref().unwrap().starts_with(to_reason),
            "{from:?}: {r:?}"
        );
        assert_eq!(r.failed_in.as_deref(), Some(state_name(from)));
    }
}

/// A rig with `own-failure-handling` set as asked and "a" claimed on lane 1, its first session ended by a
/// watchdog kill (not yet read).
async fn silenced(flag: bool) -> (Rig, Arc<Fake>, Asked) {
    let (base, asked) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(if flag {
        &["own-failure-handling"]
    } else {
        &[]
    }));
    let fake = rig.go_live();
    fake.script
        .lock()
        .unwrap()
        .push(SessionResult::Silenced("went quiet".into()));
    rig.machine.pass().await;
    (rig, fake, asked)
}

/// Makes the pending resume of "a" due now and runs a pass, with the next session ending as `next`.
async fn resume_now(rig: &mut Rig, fake: &Fake, next: SessionResult) {
    fake.script.lock().unwrap().push(next);
    rig.lp(0).silenced.insert("a".into(), std::time::Instant::now());
    rig.machine.pass().await;
}

#[tokio::test]
async fn a_watchdog_kill_keeps_lane_lease_and_run_and_does_not_pause_claiming() {
    let (mut rig, _fake, asked) = silenced(true).await;
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
    assert_eq!(rig.lp(0).store.runs()["a"].silence_kills, 1);
    let events = rig.lp(0).events().recent();
    let ended = events.iter().find(|e| e.kind() == "session_end").unwrap().to_value();
    assert_eq!(ended["subtype"], "killed_for_silence");
}

#[tokio::test]
async fn a_watchdog_kill_is_resumed_by_id_only_after_the_delay() {
    let (mut rig, fake, _) = silenced(true).await;
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1, "not resumed at once");
    resume_now(&mut rig, &fake, done(ImplementerOutcome::Finished)).await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 2);
    assert!(started[1].resume && started[1].session_id == started[0].session_id);
    assert_eq!(started[1].lane, 1);
}

#[tokio::test]
async fn the_fourth_watchdog_kill_in_a_row_goes_to_a_person() {
    let (mut rig, fake, _) = silenced(true).await;
    rig.machine.pass().await;
    for _ in 0..2 {
        resume_now(&mut rig, &fake, SessionResult::Silenced("went quiet".into())).await;
        rig.machine.pass().await;
        assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    }
    resume_now(&mut rig, &fake, SessionResult::Silenced("went quiet again".into())).await;
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 4, "three resumes");
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), None);
    let record = rig.lp(0).store.runs()["a"].clone();
    assert_eq!(record.needs_human_reason.as_deref(), Some("went quiet again"));
    assert_eq!(record.silence_kills, 0);
}

#[tokio::test]
async fn an_answer_between_watchdog_kills_resets_the_count() {
    let (mut rig, fake, _) = silenced(true).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).store.runs()["a"].silence_kills, 1);
    resume_now(&mut rig, &fake, done(ImplementerOutcome::Finished)).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).store.runs()["a"].silence_kills, 0);
}

#[tokio::test]
async fn the_watchdog_kill_count_is_kept_across_a_reload() {
    let (mut rig, _fake, _) = silenced(true).await;
    rig.machine.pass().await;
    let saved = rig.lp(0).store.runs()["a"].clone();
    assert_eq!(saved.silence_kills, 1);
    let json = serde_json::to_string(&saved).unwrap();
    let back: crate::store::RunRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(back.silence_kills, 1);
}

#[tokio::test]
async fn with_the_flag_off_a_watchdog_kill_gives_the_item_back_as_before() {
    let (mut rig, _fake, asked) = silenced(false).await;
    rig.machine.pass().await;
    assert!(ops(&asked).contains(&"release".to_string()) && rig.lp(0).runs.is_empty());
    assert!(rig.machine.claim_paused_until.is_some());
}

/// Lanes 1-2 on Claude and 3-4 on Cursor, `engine-start-rules` On, "a" waiting to be handed out.
async fn mixed_rig() -> (Rig, Arc<Fake>, Asked) {
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(4, &[&base]);
    rig.machine.links.engine_choice =
        serde_json::from_value(json!({ "lanes": ["claude", "claude", "cursor", "cursor"] })).unwrap();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["engine-start-rules"]));
    let fake = rig.go_live();
    (rig, fake, asked)
}

fn out_for(secs: u64) -> std::time::Instant {
    std::time::Instant::now() + Duration::from_secs(secs)
}

#[tokio::test]
async fn with_claude_signed_out_the_item_is_claimed_and_starts_on_a_cursor_lane() {
    let (mut rig, fake, asked) = mixed_rig().await;
    rig.machine.claude_signed_out_until = Some(out_for(600));
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert!(ops(&asked).contains(&"claim_next".to_string()), "{:?}", ops(&asked));
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].engine, EngineId::Cursor);
}

#[tokio::test]
async fn with_cursor_limited_the_item_starts_on_a_claude_lane() {
    let (mut rig, fake, asked) = mixed_rig().await;
    rig.machine.engine_limits.hit(EngineId::Cursor, out_for(600));
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert!(ops(&asked).contains(&"claim_next".to_string()), "{:?}", ops(&asked));
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].engine, EngineId::Claude);
}

#[tokio::test]
async fn with_every_lanes_engine_out_nothing_is_asked_for() {
    let (mut rig, fake, asked) = mixed_rig().await;
    rig.machine.claude_signed_out_until = Some(out_for(600));
    rig.machine.engine_limits.hit(EngineId::Cursor, out_for(600));
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert!(!ops(&asked).contains(&"claim_next".to_string()), "{:?}", ops(&asked));
    assert!(fake.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_cursor_lane_with_cursor_limited_starts_its_new_session_on_claude_used_elsewhere_here() {
    let (base, _asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    rig.machine.links.engine_choice = serde_json::from_value(json!({ "lanes": ["cursor", "claude"] })).unwrap();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["engine-start-rules"]));
    rig.machine.engine_limits.hit(EngineId::Cursor, out_for(600));
    let fake = rig.go_live();
    rig.machine.pass().await;
    rig.machine.pass().await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert_eq!((started[0].lane, started[0].engine), (1, EngineId::Claude));
}

#[tokio::test]
async fn a_computer_on_cursor_only_with_cursor_limited_claims_nothing() {
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    rig.machine.links.engine_choice =
        serde_json::from_value(json!({ "default": "cursor", "lanes": ["cursor", "cursor"] })).unwrap();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["engine-start-rules"]));
    rig.machine.engine_limits.hit(EngineId::Cursor, out_for(600));
    let fake = rig.go_live();
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert!(!ops(&asked).contains(&"claim_next".to_string()), "{:?}", ops(&asked));
    assert!(fake.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_sessions_usage_reading_still_lands_in_state_usage() {
    let (base, _asked) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    at(&mut rig, 0, "a", ItemState::Implementing, Some(1));
    let usage = json!({"status": "rejected", "resetsAt": i64::MAX});
    rig.machine.outcome_sender().send(SessionOutcome {
        project: "p0".into(),
        item_id: "a".into(),
        lane: 1,
        result: SessionResult::NeedsHuman("the session ended without answering".into()),
        usage: Some(usage.clone()),
        context_tokens: None,
        notes: vec![],
        session_id: None,
    });
    rig.machine.poll().await;
    assert_eq!(
        rig.lp(0).store.usage(),
        Some(usage),
        "a session that needs a person may still have reported the reading section 7.1 needs most"
    );
    assert!(
        crate::tiers::signal_on(1_700_000_000),
        "saving a five-hour reading turns the machine-wide subscription signal on"
    );
}

/// The 30 minutes are up: Cursor's stamp has lapsed and the resume is due now.
async fn wait_over(rig: &mut Rig, fake: &Fake, next: SessionResult) {
    rig.machine.engine_limits = Default::default();
    resume_now(rig, fake, next).await;
}

fn cursor_limited() -> SessionResult {
    SessionResult::Limited {
        why: "usage limit".into(),
        resets_in: None,
    }
}

/// "a" claimed on a Cursor lane with `cursor-limit-retry` as asked, its first session ended at Cursor's limit.
async fn cursor_hit(flag: bool, engine: &str) -> (Rig, Arc<Fake>, Asked) {
    let (base, asked) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    rig.machine.links.engine_choice = serde_json::from_value(json!({ "lanes": [engine, engine] })).unwrap();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(if flag {
        &["cursor-limit-retry"]
    } else {
        &[]
    }));
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(cursor_limited());
    rig.machine.pass().await;
    rig.machine.pass().await;
    (rig, fake, asked)
}

#[tokio::test]
async fn a_cursor_limit_keeps_the_item_implementing_and_resumes_it_after_30_minutes() {
    let (mut rig, fake, asked) = cursor_hit(true, "cursor").await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
    assert_eq!(rig.lp(0).store.leases()["a"], "lease-1");
    assert!(!ops(&asked).iter().any(|o| o == "release" || o == "park"));
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 1);
    let due = rig.lp(0).silenced["a"];
    let wait = due - std::time::Instant::now();
    assert!(wait > Duration::from_secs(29 * 60) && wait <= Duration::from_secs(30 * 60));
    assert_eq!(fake.started.lock().unwrap().len(), 1, "not resumed before the wait");
    wait_over(&mut rig, &fake, done(ImplementerOutcome::Finished)).await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 2);
    assert!(started[1].resume && started[1].session_id == started[0].session_id);
}

#[tokio::test]
async fn the_fourth_cursor_limit_in_a_row_goes_to_a_person() {
    let (mut rig, fake, _) = cursor_hit(true, "cursor").await;
    for _ in 0..2 {
        wait_over(&mut rig, &fake, cursor_limited()).await;
        rig.machine.pass().await;
        assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    }
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 3);
    wait_over(&mut rig, &fake, cursor_limited()).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    let record = rig.lp(0).store.runs()["a"].clone();
    assert_eq!(
        record.needs_human_reason.as_deref(),
        Some("Cursor's usage limit was hit 3 times in a row")
    );
    assert_eq!(record.cursor_limit_hits, 0);
}

#[tokio::test]
async fn an_answer_between_cursor_limits_resets_the_count_and_it_survives_a_reload() {
    let (mut rig, fake, _) = cursor_hit(true, "cursor").await;
    let saved = rig.lp(0).store.runs()["a"].clone();
    let back: crate::store::RunRecord = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
    assert_eq!(back.cursor_limit_hits, 1);
    wait_over(&mut rig, &fake, done(ImplementerOutcome::Finished)).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 0);
}

#[tokio::test]
async fn a_claude_limit_or_the_flag_off_goes_to_a_person_as_before() {
    for (flag, engine) in [(true, "claude"), (false, "cursor")] {
        let (mut rig, _fake, _) = cursor_hit(flag, engine).await;
        assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman, "{flag} {engine}");
        assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 0);
    }
}

async fn settle_usage(flag_on: bool, usage: Value) -> Option<Value> {
    let (base, _asked) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    if flag_on {
        rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["weekly-limit-reading"]));
    }
    rig.go_live();
    at(&mut rig, 0, "a", ItemState::Implementing, Some(1));
    rig.lp(0).store.set_usage(json!({"status": "allowed"}));
    rig.machine.outcome_sender().send(SessionOutcome {
        project: "p0".into(),
        item_id: "a".into(),
        lane: 1,
        result: SessionResult::NeedsHuman("the weekly limit".into()),
        usage: Some(usage),
        context_tokens: None,
        notes: vec![],
        session_id: None,
    });
    rig.machine.poll().await;
    rig.lp(0).store.usage()
}

#[tokio::test]
async fn a_weekly_rejection_is_saved_as_the_usage_reading_with_the_week_as_its_reset() {
    let week = json!({"status": "rejected", "rateLimitType": "seven_day", "resetsAt": 4_000_000_000u64});
    assert_eq!(settle_usage(true, week.clone()).await, Some(week.clone()));
    // Claude counts limited until the week's reset, not a 30-minute pause.
    let wait = crate::engines::engine::resets_in(Some(&week), true).unwrap();
    assert!(wait > std::time::Duration::from_secs(24 * 3600));
    // Off: today's behaviour, the lone weekly reading is dropped.
    assert_eq!(settle_usage(false, week).await, Some(json!({"status": "allowed"})));
}

#[tokio::test]
async fn a_non_rejected_weekly_reading_does_not_overwrite_the_usage_reading() {
    let week = json!({"status": "allowed", "rateLimitType": "seven_day", "resetsAt": 4_000_000_000u64});
    assert_eq!(settle_usage(true, week).await, Some(json!({"status": "allowed"})));
}

#[tokio::test]
async fn a_weekly_reading_at_the_cutoff_limits_claude_until_the_week_resets() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let reading = |u: f64| {
        json!({"unifiedWindows": {
            "five_hour": {"utilization": 0.1, "resetsAt": now + 3600},
            "seven_day": {"utilization": u, "resetsAt": now + 86_400}}})
    };
    for (u, limited) in [(0.96, true), (0.94, false)] {
        let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
        let mut rig = rig(2, &[&base]);
        rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["weekly-limit-reading"]));
        rig.go_live();
        at(&mut rig, 0, "a", ItemState::Implementing, Some(1));
        rig.machine.outcome_sender().send(SessionOutcome {
            project: "p0".into(),
            item_id: "a".into(),
            lane: 1,
            result: SessionResult::NeedsHuman("x".into()),
            usage: Some(reading(u)),
            context_tokens: None,
            notes: vec![],
            session_id: None,
        });
        rig.machine.poll().await;
        let t = std::time::Instant::now();
        let id = crate::engines::EngineId::Claude;
        assert_eq!(rig.machine.engine_limits.is_limited(id, t), limited);
        assert!(
            !rig.machine
                .engine_limits
                .is_limited(id, t + Duration::from_secs(86_402))
        );
    }
}

#[tokio::test]
async fn a_shorter_five_hour_limit_does_not_shorten_the_weekly_cutoff() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["weekly-limit-reading"]));
    rig.go_live();
    at(&mut rig, 0, "a", ItemState::Implementing, Some(1));
    rig.machine.outcome_sender().send(SessionOutcome {
        project: "p0".into(),
        item_id: "a".into(),
        lane: 1,
        result: SessionResult::Limited {
            why: "five hour".into(),
            resets_in: Some(Duration::from_secs(3600)),
        },
        usage: Some(json!({"unifiedWindows": {"seven_day": {"utilization": 0.96, "resetsAt": now + 86_400}}})),
        context_tokens: None,
        notes: vec![],
        session_id: None,
    });
    rig.machine.poll().await;
    let later = std::time::Instant::now() + Duration::from_secs(7200);
    assert!(
        rig.machine
            .engine_limits
            .is_limited(crate::engines::EngineId::Claude, later)
    );
}

#[tokio::test]
async fn a_saved_reading_over_the_cutoff_limits_claude_on_the_first_poll() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for (u, limited) in [(0.96, true), (0.94, false)] {
        let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
        let mut rig = rig(2, &[&base]);
        rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["weekly-limit-reading"]));
        rig.lp(0)
            .store
            .set_usage(json!({"unifiedWindows": {"seven_day": {"utilization": u, "resetsAt": now + 86_400}}}));
        rig.go_live();
        rig.machine.poll().await;
        let t = std::time::Instant::now();
        assert_eq!(
            rig.machine
                .engine_limits
                .is_limited(crate::engines::EngineId::Claude, t),
            limited
        );
    }
}
