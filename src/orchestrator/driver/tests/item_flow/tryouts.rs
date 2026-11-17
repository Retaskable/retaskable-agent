//! A leased item's try-outs: the `playtest` and `pendingPlaytest` details the review's choice becomes, the deferred try-out
//! sent with `tryout_add` once the change merged, a person's answer to the one of an item held here, and a problem found
//! after the merge reopening the item.
use super::super::flow::checking;
use super::super::*;
use crate::store::{FlowRecord, TryOut};

fn try_out(what: &str) -> TryOut {
    TryOut {
        what: what.into(),
        steps: vec!["Open it".into(), "Click the chart".into()],
    }
}

fn reviewed(state: ItemState, flow: FlowRecord) -> StepDone {
    let mut run = ItemRun::new();
    run.state = state;
    StepDone {
        run,
        flow: Some(flow),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_review_that_asks_for_a_tryout_writes_it_to_the_items_details_and_one_that_defers_it_writes_the_pending_one()
{
    let (base, _asked) = claim_stub(json!([item("a", "in_review")]), Some(Value::Null)).await;
    let (mut rig, fake) = checking(&base).await;
    at(&mut rig, 0, "a", ItemState::Reviewing, None);
    let flow = FlowRecord {
        pr_url: Some("https://github.com/o/p/pull/7".into()),
        playtest: Some(try_out("the chart")),
        ..Default::default()
    };
    *fake.done.lock().unwrap() = Some(reviewed(ItemState::AwaitingPlaytest, flow));
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    let held = &rig.lp(0).decisions["a"];
    assert_eq!(held.now.playtest, Some(try_out("the chart")));
    assert_eq!(held.now.pending_playtest, None);
    assert!(!held.dirty(), "the store took the details");
}

#[tokio::test]
async fn a_review_that_defers_the_tryout_writes_the_pending_one_and_leaves_the_other_clear() {
    let (base, _asked) = claim_stub(json!([item("a", "in_review")]), Some(Value::Null)).await;
    let (mut rig, fake) = checking(&base).await;
    at(&mut rig, 0, "a", ItemState::Reviewing, None);
    let flow = FlowRecord {
        pr_url: Some("https://github.com/o/p/pull/7".into()),
        pending_playtest: Some(try_out("after")),
        ..Default::default()
    };
    *fake.done.lock().unwrap() = Some(reviewed(ItemState::Merging, flow));
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    let held = &rig.lp(0).decisions["a"];
    assert_eq!(
        (held.now.playtest.clone(), held.now.pending_playtest.clone()),
        (None, Some(try_out("after")))
    );
    assert!(!held.dirty());
}

use super::super::restart::restarted;
use super::merge::{merged, merging, merging_with, ops};
use std::sync::atomic::{AtomicBool, Ordering};

/// A rig whose item "a" is `merging` with a try-out deferred until after the merge.
async fn merging_with_a_deferred_tryout(
    down: Arc<AtomicBool>,
    tryout_down: Arc<AtomicBool>,
) -> (Rig, Arc<Fake>, Asked) {
    let (mut rig, fake, asked) = merging_with(down, tryout_down).await;
    let record = rig.lp(0).records.get_mut("a").unwrap();
    record.flow.pending_playtest = Some(try_out("the chart"));
    record.flow.playtest = Some(try_out("the chart"));
    *fake.done.lock().unwrap() = Some(merged("deadbeef"));
    (rig, fake, asked)
}

#[tokio::test]
async fn a_merged_change_with_a_deferred_tryout_sends_it_after_the_completion_and_remembers_who_built_it() {
    let (mut rig, _fake, asked) = merging_with_a_deferred_tryout(Arc::default(), Arc::default()).await;
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    rig.machine.pass().await;
    let calls = super::super::flow::calls(&asked);
    let at = |op: &str| {
        calls
            .iter()
            .position(|c| c == op)
            .unwrap_or_else(|| panic!("no {op} in {calls:?}"))
    };
    assert!(at("complete") < at("tryout_add"), "{calls:?}");
    let sent = ops(&asked, "tryout_add");
    assert_eq!(sent.len(), 1, "{calls:?}");
    let add = &sent[0];
    assert_eq!(
        (
            add["itemId"].as_str(),
            add["title"].as_str(),
            add["what"].as_str(),
            add["sha"].as_str()
        ),
        (Some("a"), Some("a"), Some("the chart"), Some("deadbeef"))
    );
    assert_eq!(add["steps"], json!(["Open it", "Click the chart"]));
    assert_eq!(add["prUrl"], json!("https://github.com/o/p/pull/7"));
    assert!(add["mergedAt"].as_str().is_some_and(|m| !m.is_empty()) && add["machine"].is_string());
    // Spent: nothing owed, and the key is kept with the session that built the slice (a problem goes back to it).
    let lp = rig.lp(0);
    assert!(lp.outbox.is_empty());
    let built = lp.store.tryouts_built();
    assert_eq!(built.len(), 1);
    assert_eq!(built[0].key, format!("a@{}", add["mergedAt"].as_str().unwrap()));
    assert_eq!(built[0].session_id.as_deref(), Some("s1"));
}

#[tokio::test]
async fn a_tryout_the_store_does_not_take_is_kept_across_a_restart_and_sent_once_it_answers() {
    let tryout_down = Arc::new(AtomicBool::new(true));
    let (mut rig, _fake, asked) = merging_with_a_deferred_tryout(Arc::default(), tryout_down.clone()).await;
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(ops(&asked, "complete").len(), 1);
    // The completion was taken, the try-out wasn't: listed and saved, and not asked from the first step again.
    let saved = Store::load(&rig._dir.path().join("0")).unwrap();
    let owed = saved.complete_outbox()["a"].clone();
    assert!(owed.completed && owed.tryout.as_ref().is_some_and(|t| t.what == "the chart"));
    assert!(saved.tryouts_built().is_empty());
    let (base, asked) = super::merge::merge_store(json!([item("a", "merging")]), Arc::default()).await;
    let mut rig = restarted(rig, &base);
    rig.go_live();
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(ops(&asked, "complete").len(), 0, "the completion is not sent again");
    let sent = ops(&asked, "tryout_add");
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]["mergedAt"].as_str(),
        Some(owed.tryout.as_ref().unwrap().merged_at.as_str())
    );
    assert!(rig.lp(0).outbox.is_empty());
    tryout_down.store(false, Ordering::SeqCst);
}

#[tokio::test]
async fn a_merge_with_no_deferred_tryout_sends_none() {
    let (mut rig, fake, asked) = merging(Arc::default()).await;
    *fake.done.lock().unwrap() = Some(merged("deadbeef"));
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert!(ops(&asked, "tryout_add").is_empty());
    assert_eq!(ops(&asked, "complete").len(), 1);
}

use crate::lane_pool::Holder;
use crate::view::ItemCommand;

/// A live rig whose item "a" waits for its try-out parked: no lease (the store has it as this computer's), no lane, its change on
/// the host and the try-out the review asked for in its record. The store gives the lease back on a claim by name.
async fn parked_for_tryout() -> (Rig, Arc<Fake>, Asked) {
    let claim = json!({ "item": item("a", "needs_person"), "token": "lease-2" });
    let (base, asked) = claim_stub(json!([item("a", "needs_person")]), Some(claim)).await;
    let (mut rig, fake) = checking(&base).await;
    at(&mut rig, 0, "a", ItemState::AwaitingPlaytest, None);
    rig.machine.pool.release(
        1,
        &Holder::Item {
            project: "p0".into(),
            item: "a".into(),
        },
    );
    let lp = rig.lp(0);
    let record = lp.records.get_mut("a").unwrap();
    record.lane = None;
    record.state = "awaiting_playtest".into();
    record.flow.pr_url = Some("https://github.com/o/p/pull/7".into());
    record.flow.playtest = Some(try_out("the chart"));
    lp.leases.remove("a");
    lp.decisions.remove("a");
    lp.parked
        .insert("a".into(), ("tryout".into(), "awaiting_playtest".into()));
    lp.runs.get_mut("a").unwrap().review_rounds = 1;
    (rig, fake, asked)
}

fn answer(rig: &Rig, passed: bool, notes: &str) {
    rig.machine.links.item_commands.push(ItemCommand::Playtest {
        project: "p0".into(),
        id: "a".into(),
        passed,
        notes: Some(notes.into()),
        attached: None,
        merged_at: None,
    });
}

#[tokio::test]
async fn a_problem_found_in_the_tryout_of_a_parked_item_claims_it_back_then_starts_a_session_on_a_lane_it_takes() {
    let (mut rig, fake, asked) = parked_for_tryout().await;
    answer(&rig, false, "The chart is empty.");
    rig.machine.pass().await;
    settle().await;
    let calls = super::super::flow::calls(&asked);
    assert!(calls.contains(&"claim".to_string()), "{calls:?}");
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1, "{calls:?}");
    // A new session on the pushed branch (it held no lane), told what they checked and what they found.
    let said = started[0].follow_up.as_deref().unwrap();
    assert!(said.contains("picking up an existing PR"), "{said}");
    assert!(said.contains("The chart is empty."), "{said}");
    assert!(
        said.contains("What they checked: the chart\n1. Open it\n2. Click the chart"),
        "{said}"
    );
    assert!(!started[0].resume);
    let lp = rig.lp(0);
    assert_eq!(lp.leases.get("a").map(String::as_str), Some("lease-2"));
    assert!(lp.parked.is_empty());
    assert_eq!(lp.runs["a"].state, ItemState::Implementing);
    // The question is answered: it is no longer asked.
    assert_eq!(lp.records["a"].flow.playtest, None);
    assert!(rig.machine.pool.lane_of("p0", "a").is_some());
}

#[tokio::test]
async fn a_pass_on_the_tryout_of_a_parked_item_moves_it_to_the_merge_and_claims_it_back_on_the_next_pass() {
    let (mut rig, fake, asked) = parked_for_tryout().await;
    answer(&rig, true, "");
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Merging);
    assert_eq!(rig.lp(0).records["a"].flow.playtest, None);
    rig.machine.pass().await;
    settle().await;
    let calls = super::super::flow::calls(&asked);
    assert!(calls.contains(&"claim".to_string()), "{calls:?}");
    assert!(fake.started.lock().unwrap().is_empty());
    let lp = rig.lp(0);
    assert_eq!(lp.leases.get("a").map(String::as_str), Some("lease-2"));
    assert!(lp.parked.is_empty());
}
