//! What the loop does with a step's end for a leased item (`driver/flow.rs`): a send-back resumes the implementer's
//! session with the message (kept across a restart), the store hears each state, and a step's notes join the history.
use super::restart::restarted;
use super::*;
use crate::backlog::decisions::sync::ItemDecisions;
use crate::orchestrator::steps::StepDone;
use crate::store::FlowRecord;

/// A live rig whose item "a" is `checking` in lane 1 with its session "s1" on `branch`.
pub(super) async fn checking(base: &str) -> (Rig, Arc<Fake>) {
    let mut rig = rig(2, &[base]);
    let fake = rig.go_live();
    rig.queue(0, &["a"]);
    at(&mut rig, 0, "a", ItemState::Checking, Some(1));
    let lp = rig.lp(0);
    // Held since before this pass: nothing to take back from a restart.
    lp.adopted = true;
    lp.leases.insert("a".into(), "lease-1".into());
    lp.records.insert(
        "a".into(),
        RunRecord {
            state: "checking".into(),
            lane: Some(1),
            impl_session_id: Some("s1".into()),
            branch: Some("retaskable/a-a-s0".into()),
            ..Default::default()
        },
    );
    lp.decisions.insert("a".into(), ItemDecisions::default());
    (rig, fake)
}

pub(super) fn sent_back(text: &str, rounds: u32) -> StepDone {
    let mut run = ItemRun::new();
    run.state = ItemState::Implementing;
    run.check_rounds = rounds;
    StepDone {
        run,
        notes: vec!["Blocking checks failed: unit".into()],
        send_back: Some(text.into()),
        flow: Some(FlowRecord {
            pr_url: Some("https://github.com/o/p/pull/7".into()),
            pr_number: Some(7),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The store's calls in order, not counting the progress reports (`report` with a status): those follow the item's
/// states, and the tests of what a give-back or a start sends are about the rest.
pub(super) fn calls(asked: &Asked) -> Vec<String> {
    let asked = asked.lock().unwrap();
    let is_status = |op: &str, args: &Value| op == "report" && args.get("status").is_some();
    asked
        .iter()
        .filter(|(op, args)| !is_status(op, args))
        .map(|(op, _)| op.clone())
        .collect()
}

pub(super) fn reports(asked: &Asked) -> Vec<Value> {
    let asked = asked.lock().unwrap();
    asked
        .iter()
        .filter(|(op, _)| op == "report")
        .map(|(_, a)| a.clone())
        .collect()
}

#[tokio::test]
async fn a_send_back_resumes_the_session_with_the_message_and_the_store_hears_the_state() {
    let (base, asked) = claim_stub(json!([item("a", "in_review")]), Some(Value::Null)).await;
    let (mut rig, fake) = checking(&base).await;
    *fake.done.lock().unwrap() = Some(sent_back("Fix the unit tests.", 1));
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    rig.machine.pass().await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1, "{started:?}");
    assert!(started[0].resume, "{:?} {:?}", started[0].follow_up, kinds(&mut rig));
    assert_eq!(started[0].session_id, "s1");
    assert_eq!(started[0].follow_up.as_deref(), Some("Fix the unit tests."));
    let run = rig.lp(0).runs["a"];
    assert_eq!((run.state, run.check_rounds), (ItemState::Implementing, 1));
    let told: Vec<_> = reports(&asked)
        .iter()
        .map(|r| (r["status"].clone(), r["detail"].clone()))
        .collect();
    assert!(told.contains(&(json!("building"), json!("implementing"))), "{told:?}");
    let report = reports(&asked).remove(0);
    assert_eq!(
        (
            report["id"].as_str(),
            report["token"].as_str(),
            report["branch"].as_str()
        ),
        (Some("a"), Some("lease-1"), Some("retaskable/a-a-s0"))
    );
    // The message is spent: the next start of this session is a plain resume.
    assert_eq!(rig.lp(0).records["a"].flow.follow_up, None);
    assert!(kinds(&mut rig).len() > 1);
}

#[tokio::test]
async fn the_message_and_the_rounds_survive_a_restart_before_the_session_starts() {
    let (base, _) = claim_stub(json!([item("a", "in_review")]), Some(Value::Null)).await;
    let (mut rig, fake) = checking(&base).await;
    *fake.done.lock().unwrap() = Some(sent_back("Fix the unit tests.", 2));
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    // Saved: the follow-up, the change, the rounds.
    let saved = Store::load(&rig._dir.path().join("0")).unwrap().runs();
    assert_eq!(saved["a"].flow.follow_up.as_deref(), Some("Fix the unit tests."));
    assert_eq!(saved["a"].flow.check_rounds, 2);
    assert_eq!(saved["a"].flow.pr_number, Some(7));
    let (base, _) = claim_stub(json!([item("a", "building")]), Some(Value::Null)).await;
    let mut rig = restarted(rig, &base);
    let fake = rig.go_live();
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1, "{started:?}");
    assert!(started[0].resume);
    assert_eq!(started[0].follow_up.as_deref(), Some("Fix the unit tests."));
    assert_eq!(rig.lp(0).runs["a"].check_rounds, 2);
}

pub(super) fn merging() -> StepDone {
    let mut run = ItemRun::new();
    run.state = ItemState::Merging;
    run.approved_at_head = true;
    StepDone {
        run,
        flow: Some(FlowRecord {
            approved_sha: Some("abc".into()),
            ..Default::default()
        }),
        notes: vec!["Opus review: approved".into()],
        ..Default::default()
    }
}

#[tokio::test]
async fn a_review_that_approves_tells_the_store_merging_and_keeps_the_approval() {
    let (base, asked) = claim_stub(json!([item("a", "in_review")]), Some(Value::Null)).await;
    let (mut rig, fake) = checking(&base).await;
    at(&mut rig, 0, "a", ItemState::Reviewing, None);
    *fake.done.lock().unwrap() = Some(merging());
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    let told: Vec<_> = reports(&asked)
        .iter()
        .map(|r| (r["status"].clone(), r["detail"].clone()))
        .collect();
    assert!(told.contains(&(json!("merging"), json!("merging"))), "{told:?}");
    let record = rig.lp(0).records["a"].clone();
    assert_eq!(record.flow.approved_sha.as_deref(), Some("abc"));
    assert!(record.flow.approved_at_head);
    // A reviewing item has no lane to give up while merging waits: the lane is freed.
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), None);
}

#[tokio::test]
async fn a_step_that_asked_to_wait_is_not_run_again_before_its_time() {
    let (base, _) = claim_stub(json!([item("a", "in_review")]), Some(Value::Null)).await;
    let (mut rig, fake) = checking(&base).await;
    rig.lp(0).clock = Some(1_000);
    rig.lp(0).records.get_mut("a").unwrap().flow.retry_after_ms = Some(1_000_000 + 5_000);
    rig.machine.pass().await;
    settle().await;
    assert!(fake.stepped.lock().unwrap().is_empty(), "stepped before its time");
    rig.lp(0).clock = Some(1_006);
    rig.machine.pass().await;
    assert_eq!(fake.stepped.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_restart_in_review_carries_on_reviewing_with_what_the_record_kept() {
    let (base, _) = claim_stub(json!([item("a", "in_review")]), Some(Value::Null)).await;
    let (mut rig, _fake) = checking(&base).await;
    {
        let record = rig.lp(0).records.get_mut("a").unwrap();
        record.state = "reviewing".into();
        record.flow.reviewed_sha = Some("r1".into());
        record.flow.review_rounds = 1;
    }
    rig.lp(0).save_leases();
    let mut rig = restarted(rig, &base);
    let fake = rig.go_live();
    rig.machine.pass().await;
    settle().await;
    let stepped = fake.stepped.lock().unwrap().clone();
    assert_eq!(stepped.len(), 1, "{stepped:?}");
    assert_eq!(stepped[0].run.state, ItemState::Reviewing);
    assert_eq!(stepped[0].run.review_rounds, 1);
    assert_eq!(stepped[0].flow.reviewed_sha.as_deref(), Some("r1"));
}

#[tokio::test]
async fn claude_is_on_a_stopped_item_only_while_its_triage_job_waits_or_runs_or_a_decision_waits() {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let (mut rig, _) = checking(&base).await;
    let lp = rig.lp(0);
    lp.records.get_mut("a").unwrap().state = "needs_human".into();
    assert!(
        !lp.claude_on_it("a"),
        "no job and no decision: the store decides whether a look happens"
    );
    lp.triaging.insert("a".into());
    assert!(lp.claude_on_it("a"), "its triage job waits or runs");
    lp.triaging.clear();
    lp.claude_pending.insert("a".into());
    assert!(lp.claude_on_it("a"), "a decision of Claude's waits for a lane");
    lp.records.get_mut("a").unwrap().state = "building".into();
    assert!(!lp.claude_on_it("a"), "not a stopped item");
    assert!(!lp.claude_on_it("missing"));
}
