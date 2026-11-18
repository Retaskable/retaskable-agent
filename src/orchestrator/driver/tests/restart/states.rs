//! A restart with a saved run in each state that holds a lease (`readopt`'s table), and an item the store lists as held by
//! this computer with no lease kept here (`own_items.rs`).
use super::*;

const PR: &str = "https://github.com/o/p/pull/777";

/// "a" claimed and saved in `saved`, the computer restarted and the store listing it as `listed` with a change; the first pass is
/// returned.
async fn after(saved: &str, listed: &str) -> (Rig, Asked) {
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let _fake = rig.go_live();
    rig.machine.pass().await;
    let store = &mut rig.lp(0).store;
    let mut runs = store.runs();
    let record = runs.get_mut("a").unwrap();
    record.state = saved.into();
    record.lane = None;
    record.flow.pr_url = Some(PR.into());
    store.set_runs(&runs);
    store.save().unwrap();
    let mut row = item("a", listed);
    row["prUrl"] = json!(PR);
    let (base, asked) = claim_stub(json!([row]), Some(Value::Null)).await;
    let mut rig = restarted(rig, &base);
    let _fake = rig.go_live();
    rig.machine.pass().await;
    (rig, asked)
}

#[tokio::test]
async fn a_restart_keeps_the_lease_of_an_item_in_each_state_its_status_fits() {
    for (saved, listed, state) in [
        ("merging", "merging", ItemState::Merging),
        ("merging", "in_review", ItemState::Merging),
        ("awaiting_ci", "in_review", ItemState::AwaitingCi),
        ("awaiting_merge", "merging", ItemState::AwaitingMerge),
        ("awaiting_playtest", "needs_person", ItemState::AwaitingPlaytest),
        ("needs_clarification", "needs_person", ItemState::NeedsClarification),
        ("needs_human", "needs_person", ItemState::NeedsHuman),
    ] {
        let (mut rig, asked) = after(saved, listed).await;
        let lp = rig.lp(0);
        assert_eq!(
            lp.leases.get("a").map(String::as_str),
            Some("lease-1"),
            "{saved} / {listed}"
        );
        assert_eq!(lp.runs["a"].state, state, "{saved} / {listed}");
        assert!(
            lp.store.runs().contains_key("a"),
            "{saved} / {listed}: its record is kept"
        );
        assert!(!ops(&asked).iter().any(|o| o == "release"), "{saved} / {listed}");
    }
}

#[tokio::test]
async fn a_restart_drops_an_item_the_store_no_longer_has_for_its_state() {
    for (saved, listed) in [
        ("merging", "todo"),
        ("merging", "on_main"),
        ("awaiting_ci", "skipped"),
        ("awaiting_ci", "needs_person"),
        ("awaiting_merge", "held"),
        ("implementing", "in_review"),
        ("checking", "merging"),
    ] {
        let (mut rig, _asked) = after(saved, listed).await;
        let lp = rig.lp(0);
        assert!(!lp.leases.contains_key("a"), "{saved} / {listed}");
        assert!(!lp.runs.contains_key("a"), "{saved} / {listed}");
    }
}

/// The store lists "a" held by `machine`, in `detail`, with a change; this computer has no lease for it and the store answers a
/// claim by name with `lease-new`. The first pass is returned.
async fn held_without_lease(machine: &str, detail: &str) -> (Rig, Asked) {
    held_listed(machine, detail, true).await
}

/// `held_without_lease`, the store answering the claim by name only when `answers` (else it has no answer, and no task
/// comes either).
async fn held_listed(machine: &str, detail: &str, answers: bool) -> (Rig, Asked) {
    let mut row = item("a", "merging");
    row["prUrl"] = json!(PR);
    row["detail"] = json!(detail);
    row["lease"] = json!({ "expiresAt": null });
    row["claimedBy"] = json!({ "agentId": "x", "machine": machine, "environment": "retaskable.com" });
    let answer = json!({ "item": row, "token": "lease-new" });
    let (base, asked) = claim_stub(json!([row]), answers.then_some(answer)).await;
    let mut rig = rig(2, &[&base]);
    let _fake = rig.go_live();
    rig.machine.pass().await;
    (rig, asked)
}

fn claims(asked: &Asked) -> Vec<Value> {
    asked
        .lock()
        .unwrap()
        .iter()
        .filter(|(op, _)| op == "claim")
        .map(|(_, args)| args.clone())
        .collect()
}

#[tokio::test]
async fn an_item_held_here_with_no_lease_is_claimed_by_name_and_resumes_where_it_was() {
    let (mut rig, asked) = held_without_lease("my-pc", "merging").await;
    let first = &claims(&asked)[0];
    assert_eq!(first["id"], "a");
    assert!(
        first.get("because").is_none(),
        "a plain claim, which the store answers its own holder: {first}"
    );
    let lp = rig.lp(0);
    assert_eq!(lp.leases.get("a").map(String::as_str), Some("lease-new"));
    assert_eq!(lp.runs["a"].state, ItemState::Merging);
    assert_eq!(lp.store.runs()["a"].state, "merging");
}

#[tokio::test]
async fn an_item_held_here_that_was_building_starts_again_as_queued() {
    let (mut rig, _asked) = held_without_lease("my-pc", "implementing").await;
    assert_eq!(rig.lp(0).leases.get("a").map(String::as_str), Some("lease-new"));
    assert_ne!(rig.lp(0).runs["a"].state, ItemState::Merging);
}

#[tokio::test]
async fn an_item_another_computer_holds_is_not_claimed() {
    let (_, asked) = held_listed("my-pc", "merging", false).await;
    assert_eq!(claims(&asked).len(), 1, "this computer's own is asked for");
    let (mut rig, asked) = held_listed("other-pc", "merging", false).await;
    assert!(claims(&asked).is_empty());
    assert!(!rig.lp(0).leases.contains_key("a"));
}
