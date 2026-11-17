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
