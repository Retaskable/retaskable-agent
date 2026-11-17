//! What the loop does with a step's end for a leased item (`driver/flow.rs`): a send-back resumes the implementer's
//! session with the message (kept across a restart), the store hears each state, and a step's notes join the history.
use super::restart::restarted;
use super::*;
use crate::backlog::decisions::sync::ItemDecisions;
use crate::orchestrator::steps::StepDone;
use crate::store::FlowRecord;

/// A live rig whose item "a" is `checking` in lane 1 with its session "s1" on `branch`.
async fn checking(base: &str) -> (Rig, Arc<Fake>) {
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

fn sent_back(text: &str, rounds: u32) -> StepDone {
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

fn reports(asked: &Asked) -> Vec<Value> {
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
