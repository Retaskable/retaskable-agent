//! What the loop does with the CI step's ends (`awaiting_ci`).
use super::super::flow::{checking, reports, sent_back};
use super::super::*;

#[tokio::test]
async fn a_red_ci_sends_the_item_back_to_a_new_session_on_a_lane_it_waits_for() {
    let (base, asked) = claim_stub(json!([item("a", "in_review")]), Some(Value::Null)).await;
    let (mut rig, fake) = checking(&base).await;
    // In `awaiting_ci` it holds no lane: CI runs on the host, not in the lane.
    at(&mut rig, 0, "a", ItemState::AwaitingCi, None);
    rig.machine.pool.release(
        1,
        &crate::lane_pool::Holder::Item {
            project: "p0".into(),
            item: "a".into(),
        },
    );
    rig.lp(0).records.get_mut("a").unwrap().lane = None;
    rig.lp(0).records.get_mut("a").unwrap().state = "awaiting_ci".into();
    let mut done = sent_back("CI failed: build.", 1);
    done.notes = vec!["CI failed: build".into()];
    *fake.done.lock().unwrap() = Some(done);
    rig.machine.pass().await;
    settle().await;
    // The step's end sends the item back; the next pass finds it a lane and starts its implementer.
    rig.machine.pass().await;
    rig.machine.pass().await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1, "{started:?}");
    assert!(!started[0].resume);
    assert!(
        started[0]
            .follow_up
            .as_deref()
            .is_some_and(|f| f.contains("CI failed: build.")),
        "{:?}",
        started[0].follow_up
    );
    assert!(rig.machine.pool.lane_of("p0", "a").is_some());
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    let told: Vec<_> = reports(&asked).iter().map(|r| r["status"].clone()).collect();
    assert!(told.contains(&json!("building")), "{told:?}");
    assert_eq!(rig.lp(0).records["a"].flow.follow_up, None);
}
