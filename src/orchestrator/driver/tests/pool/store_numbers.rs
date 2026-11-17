//! The store's numbers reach the steps of an item taken from a `health` answer: the step is given the project as the loop
//! holds it, with `health`'s `tuning` laid on, not the starter's copy from the file (which has none).
use super::*;

#[tokio::test]
async fn an_item_taken_from_health_is_stepped_with_the_stores_numbers() {
    let (mut rig, fake, _asked, _plan) = live(vec![task("task-1", "a")]).await;
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1, "taken from the health answer");
    // Its session ended: the next step is checks.
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::Checking;
    rig.lp(0).pool.every = Duration::ZERO;
    rig.machine.pass().await;
    settle().await;
    let stepped = fake.stepped.lock().unwrap().clone();
    assert_eq!(stepped.len(), 1, "{stepped:?}");
    let config = stepped[0]
        .config
        .as_ref()
        .expect("the loop's project goes with the step");
    assert_eq!(config.tuning.store, Some(crate::store_tuning::StoreTuning::test()));
}

#[tokio::test]
async fn numbers_the_store_never_sent_leave_the_step_without_them() {
    let (mut rig, fake, _asked, plan) = live(vec![task("task-1", "a")]).await;
    plan.lock().unwrap().no_tuning = true;
    rig.machine.pass().await;
    assert!(
        fake.started.lock().unwrap().is_empty(),
        "nothing starts without the numbers"
    );
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::Checking;
    at(&mut rig, 0, "a", ItemState::Checking, Some(1));
    rig.machine.pass().await;
    settle().await;
    for request in fake.stepped.lock().unwrap().iter() {
        assert_eq!(request.config.as_ref().unwrap().tuning.store, None, "fails closed");
    }
}
