//! A person's playtest answer on an `awaiting_playtest` item (`driver/item_commands.rs`).
use super::*;
use crate::view::ItemCommand;

/// A live rig whose item "a" waits for a playtest on branch "a", with `lanes` lanes and one review round used.
fn awaiting(lanes: u32) -> (Rig, Arc<Fake>) {
    let mut rig = rig(lanes, &["http://127.0.0.1:1"]);
    let fake = rig.go_live();
    rig.queue(0, &["a"]);
    let lp = rig.lp(0);
    let run = lp.runs.get_mut("a").unwrap();
    run.state = ItemState::AwaitingPlaytest;
    run.review_rounds = 1;
    lp.leases.insert("a".into(), "lease-1".into());
    lp.records.insert(
        "a".into(),
        RunRecord {
            state: "awaiting_playtest".into(),
            impl_session_id: Some("s1".into()),
            branch: Some("a".into()),
            ..Default::default()
        },
    );
    (rig, fake)
}

fn answer(rig: &Rig, item: &str, passed: bool, notes: Option<&str>) {
    rig.machine.links.item_commands.push(ItemCommand::Playtest {
        project: "p0".into(),
        id: item.into(),
        passed,
        notes: notes.map(Into::into),
        merged_at: None,
    });
}

#[tokio::test]
async fn a_pass_moves_the_item_to_merging_and_starts_no_session() {
    let (mut rig, fake) = awaiting(2);
    answer(&rig, "a", true, None);
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Merging);
    assert!(fake.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_fail_resumes_the_implementer_on_its_branch_with_the_notes() {
    let (mut rig, fake) = awaiting(2);
    answer(&rig, "a", false, Some("The button does nothing."));
    rig.machine.poll().await;
    let started = fake.started.lock().unwrap();
    assert_eq!(started.len(), 1);
    assert!(
        started[0]
            .follow_up
            .as_deref()
            .unwrap()
            .contains("The button does nothing.")
    );
    drop(started);
    let run = &rig.lp(0).runs["a"];
    assert_eq!((run.state, run.review_rounds), (ItemState::Implementing, 2));
    assert!(rig.machine.pool.lane_of("p0", "a").is_some());
}

#[tokio::test]
async fn a_fail_with_blank_notes_still_resumes() {
    let (mut rig, fake) = awaiting(2);
    answer(&rig, "a", false, Some("  "));
    rig.machine.poll().await;
    let started = fake.started.lock().unwrap();
    assert!(started[0].follow_up.as_deref().unwrap().contains("found a problem"));
}

#[tokio::test]
async fn a_fail_past_the_review_rounds_stops_the_item_and_starts_nothing() {
    let (mut rig, fake) = awaiting(2);
    let max = rig.lp(0).project.max_review_rounds;
    rig.lp(0).runs.get_mut("a").unwrap().review_rounds = max;
    answer(&rig, "a", false, Some("Broken."));
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert!(fake.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_fail_with_no_free_lane_waits_and_starts_on_a_later_pass() {
    let (mut rig, _) = awaiting(1);
    rig.queue(0, &["b"]);
    rig.lp(0).runs.get_mut("b").unwrap().state = ItemState::Implementing;
    let held = rig.machine.pool.claim(&crate::lane_pool::Want {
        project: "p0".into(),
        item: "b".into(),
        chore: false,
        last_lane: None,
        fix_queued: false,
    });
    assert!(held.is_some());
    answer(&rig, "a", false, Some("Broken."));
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::AwaitingPlaytest);
    assert_eq!(rig.lp(0).playtest_fails.len(), 1);
    rig.machine.pool.release(
        held.unwrap(),
        &crate::lane_pool::Holder::Item {
            project: "p0".into(),
            item: "b".into(),
        },
    );
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert!(rig.lp(0).playtest_fails.is_empty());
}

#[tokio::test]
async fn an_answer_for_an_item_not_awaiting_a_playtest_changes_nothing() {
    let (mut rig, fake) = awaiting(2);
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::Implementing;
    answer(&rig, "a", true, None);
    answer(&rig, "nope", false, Some("x"));
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert!(rig.lp(0).playtest_fails.is_empty());
    assert!(fake.started.lock().unwrap().is_empty());
}
