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
