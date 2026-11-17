//! An item waiting on CI lets go of its lease and keeps being stepped; it is claimed back by name when CI moves it on, and a
//! restart while it is parked takes it up again.
use super::super::flow::{checking, reports};
use super::super::restart::restarted;
use super::super::*;
use crate::lane_pool::Holder;
use crate::store::FlowRecord;

const PR: &str = "https://github.com/o/p/pull/7";

fn stays_in_ci() -> StepDone {
    let mut run = ItemRun::new();
    run.state = ItemState::AwaitingCi;
    StepDone {
        run,
        flow: Some(FlowRecord {
            pr_url: Some(PR.into()),
            pr_number: Some(7),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn goes_on(state: ItemState) -> StepDone {
    let mut done = stays_in_ci();
    done.run.state = state;
    done
}

/// A live rig whose item "a" has just left a lane for `awaiting_ci` (it holds none), its first step scripted to stay there.
async fn in_ci() -> (Rig, Arc<Fake>, Asked) {
    let claim = json!({ "item": item("a", "in_review"), "token": "lease-2" });
    let (base, asked) = claim_stub(json!([item("a", "in_review")]), Some(claim)).await;
    let (mut rig, fake) = checking(&base).await;
    at(&mut rig, 0, "a", ItemState::AwaitingCi, None);
    rig.machine.pool.release(
        1,
        &Holder::Item {
            project: "p0".into(),
            item: "a".into(),
        },
    );
    let record = rig.lp(0).records.get_mut("a").unwrap();
    record.lane = None;
    record.state = "awaiting_ci".into();
    record.flow.pr_url = Some(PR.into());
    *fake.done.lock().unwrap() = Some(stays_in_ci());
    (rig, fake, asked)
}

fn parks(asked: &Asked) -> Vec<Value> {
    let asked = asked.lock().unwrap();
    (asked.iter())
        .filter(|(op, _)| op == "park")
        .map(|(_, a)| a.clone())
        .collect()
}

#[tokio::test]
async fn an_item_in_ci_is_parked_still_stepped_and_claimed_back_when_ci_moves_it_to_the_checks() {
    let (mut rig, fake, asked) = in_ci().await;
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    let parked = parks(&asked);
    assert_eq!(parked.len(), 1, "{:?}", asked.lock().unwrap());
    assert_eq!(
        (
            parked[0]["id"].as_str(),
            parked[0]["token"].as_str(),
            parked[0]["for"].as_str()
        ),
        (Some("a"), Some("lease-1"), Some("ci"))
    );
    assert!(!rig.lp(0).leases.contains_key("a"));
    assert_eq!(rig.lp(0).parked["a"], ("ci".to_string(), "awaiting_ci".to_string()));
    // Still stepped while parked: CI is read on the host, not through the store.
    *fake.done.lock().unwrap() = Some(goes_on(ItemState::Checking));
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    rig.machine.pass().await;
    let calls = super::super::flow::calls(&asked);
    assert!(calls.iter().filter(|c| *c == "claim").count() >= 1, "{calls:?}");
    let lp = rig.lp(0);
    assert_eq!(lp.leases.get("a").map(String::as_str), Some("lease-2"));
    assert!(lp.parked.is_empty());
    assert_eq!(lp.runs["a"].state, ItemState::Checking);
    // The lane for the checks was taken when the step was asked for.
    assert!(rig.machine.pool.lane_of("p0", "a").is_some());
    let told: Vec<_> = reports(&asked).iter().map(|r| r["token"].clone()).collect();
    assert!(told.contains(&json!("lease-2")), "{told:?}");
}

#[tokio::test]
async fn a_restart_while_parked_takes_the_item_up_again_in_the_state_it_waits_in() {
    let (mut rig, _fake, asked) = in_ci().await;
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    assert_eq!(parks(&asked).len(), 1);
    let saved = Store::load(&rig._dir.path().join("0")).unwrap();
    assert_eq!(saved.parked()["a"].0, "ci");
    assert_eq!(saved.runs()["a"].flow.pr_number, Some(7));
    let (base, _) = claim_stub(json!([item("a", "in_review")]), Some(Value::Null)).await;
    let mut rig = restarted(rig, &base);
    let fake = rig.go_live();
    rig.machine.pass().await;
    settle().await;
    let stepped = fake.stepped.lock().unwrap().clone();
    assert_eq!(stepped.len(), 1, "{stepped:?}");
    assert_eq!(stepped[0].run.state, ItemState::AwaitingCi);
    assert_eq!(stepped[0].flow.pr_number, Some(7));
    assert!(rig.lp(0).leases.is_empty() && rig.lp(0).parked.contains_key("a"));
}

#[tokio::test]
async fn a_parked_item_the_store_no_longer_has_waiting_is_dropped_after_a_restart() {
    let (mut rig, _fake, _asked) = in_ci().await;
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    let (base, _) = claim_stub(json!([item("a", "on_main")]), Some(Value::Null)).await;
    let mut rig = restarted(rig, &base);
    let fake = rig.go_live();
    rig.machine.pass().await;
    settle().await;
    assert!(fake.stepped.lock().unwrap().is_empty());
    assert!(rig.lp(0).runs.is_empty() && rig.lp(0).parked.is_empty());
}
