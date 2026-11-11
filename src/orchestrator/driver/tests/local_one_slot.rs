//! The session holding the local model's slot doesn't use up a `concurrency` slot.
use super::*;
use crate::local_model::LocalModel;

/// One session already running (`active` 1), `concurrency` 1, two queued items and a local model that is up.
async fn rig_with() -> (Rig, Arc<Fake>, LocalModel) {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(4, &[&base]);
    let fake = rig.go_live();
    rig.machine.concurrency = 1;
    let local = LocalModel::up();
    rig.machine.local = local.clone();
    rig.queue(0, &["a", "b"]);
    fake.active.store(1, Ordering::SeqCst);
    (rig, fake, local)
}

fn started(fake: &Fake) -> usize {
    fake.started.lock().unwrap().len()
}

#[tokio::test]
async fn the_local_holder_leaves_room_for_another_session() {
    let (mut rig, fake, local) = rig_with().await;
    let _held = local.try_take().unwrap();
    rig.machine.poll().await;
    assert_eq!(started(&fake), 1);
}

#[tokio::test]
async fn a_session_only_waiting_for_the_slot_still_counts() {
    let (mut rig, fake, local) = rig_with().await;
    let other = local.try_take().unwrap();
    let waiter = local.clone();
    let wait = tokio::spawn(async move { waiter.take().await });
    tokio::task::yield_now().await;
    assert_eq!((local.holding(), local.waiting()), (1, 1));
    drop(other);
    wait.abort();
    let _ = wait.await;
    assert_eq!(local.holding(), 0);
    rig.machine.poll().await;
    assert_eq!(started(&fake), 0);
}
