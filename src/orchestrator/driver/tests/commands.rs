//! Item commands the loop carries out (`driver/item_commands.rs`): Continue.
use super::*;
use crate::view::ItemCommand;

/// A live rig whose item "a" stopped (`needs_human`) after two rounds, holding no lane; `session` is the
/// implementer session it left behind.
fn stopped(lanes: u32, session: Option<&str>) -> (Rig, Arc<Fake>) {
    let mut rig = rig(lanes, &["http://127.0.0.1:1"]);
    let fake = rig.go_live();
    rig.queue(0, &["a"]);
    let lp = rig.lp(0);
    let run = lp.runs.get_mut("a").unwrap();
    run.state = ItemState::NeedsHuman;
    run.check_rounds = 2;
    run.review_rounds = 1;
    lp.leases.insert("a".into(), "lease-1".into());
    lp.records.insert(
        "a".into(),
        RunRecord {
            state: "needs_human".into(),
            lane: None,
            impl_session_id: session.map(Into::into),
            impl_context_tokens: None,
            branch: Some("a".into()),
            slice: 0,
        },
    );
    (rig, fake)
}

fn continue_a(rig: &Rig, item: &str, notes: Option<&str>) {
    rig.machine.links.item_commands.push(ItemCommand::Continue {
        project: "p0".into(),
        item: item.into(),
        notes: notes.map(Into::into),
    });
}

#[tokio::test]
async fn continue_with_notes_restarts_the_implementer_on_its_branch_with_the_notes() {
    let (mut rig, fake) = stopped(2, Some("s1"));
    continue_a(&rig, "a", Some("Use the other API."));
    rig.machine.poll().await;
    let started = fake.started.lock().unwrap();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].follow_up.as_deref(), Some("Use the other API."));
    assert!(!started[0].resume);
    drop(started);
    let run = rig.lp(0).runs["a"];
    assert_eq!(run.state, ItemState::Implementing);
    assert_eq!((run.check_rounds, run.review_rounds), (0, 0));
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
    let moved = rig
        .lp(0)
        .events()
        .recent()
        .iter()
        .map(Entry::to_value)
        .find(|e| e["kind"] == "item_state")
        .unwrap();
    assert_eq!(
        (moved["from"].clone(), moved["to"].clone()),
        (json!("needs_human"), json!("implementing"))
    );
}

#[tokio::test]
async fn continue_without_notes_puts_the_item_back_to_run_its_checks() {
    let (mut rig, fake) = stopped(2, Some("s1"));
    continue_a(&rig, "a", None);
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty());
    let run = rig.lp(0).runs["a"];
    assert_eq!((run.state, run.check_rounds), (ItemState::Checking, 0));
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
    assert_eq!(fake.stepped.lock().unwrap().len(), 1, "its checks run");
}

#[tokio::test]
async fn continue_drops_an_item_that_is_unknown_or_not_stopped() {
    let (mut rig, fake) = stopped(2, Some("s1"));
    rig.queue(0, &["b"]);
    rig.lp(0).runs.get_mut("b").unwrap().state = ItemState::Implementing;
    continue_a(&rig, "b", Some("x"));
    continue_a(&rig, "nope", Some("x"));
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty());
    assert_eq!(rig.lp(0).runs["b"].state, ItemState::Implementing);
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert!(rig.lp(0).continuing.is_empty());
    assert_eq!(rig.machine.pool.held_lanes().count(), 0);
}

#[tokio::test]
async fn continue_with_nothing_to_carry_on_from_leaves_the_item_for_start_over() {
    let (mut rig, fake) = stopped(2, None);
    continue_a(&rig, "a", Some("x"));
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty());
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert!(rig.lp(0).continuing.is_empty());
}

#[tokio::test]
async fn continue_waits_for_a_free_lane_and_starts_once_one_frees() {
    let (mut rig, fake) = stopped(1, Some("s1"));
    let holder = Holder::Item {
        project: "p9".into(),
        item: "other".into(),
    };
    assert!(rig.machine.pool.take(1, holder.clone()));
    continue_a(&rig, "a", Some("Go on."));
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty());
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert_eq!(rig.lp(0).continuing.len(), 1);

    assert!(rig.machine.pool.release(1, &holder));
    rig.machine.poll().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert!(rig.lp(0).continuing.is_empty());
}
