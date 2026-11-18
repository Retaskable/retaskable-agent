//! A plan the store lists as held by this computer with no lease listed (`driver/held_drafts/own.rs`): taken back with
//! `draft_take`, then let go with the new lease.
use super::asks::{Listed, ops_named, quiet_passes};
use super::merge_draft::{person, rig_draft};
use super::*;

/// The plan the store lists, waiting for approval, held by `machine` (this rig's computer is `my-pc`).
fn listed(machine: &str) -> Value {
    json!({ "id": "d1", "status": "needs_person", "detail": "awaiting_approval", "titles": ["Gradual rollout"],
        "heldBy": { "agentId": "a1", "machine": machine, "environment": "retaskable.com" } })
}

/// A live project whose store lists `plan` and answers a `draft_take` with the lease `lease-new`.
async fn rig_listed(plan: Value) -> (Rig, Asked, Listed, tempfile::TempDir) {
    let taken = json!({ "token": "lease-new", "draft": { "id": "d1", "status": "needs_person", "detail": "awaiting_approval" } });
    let (rig, asked, listed, _host, root) = rig_draft(taken, person(), vec![]).await;
    listed.drafts.lock().unwrap().push(plan);
    (rig, asked, listed, root)
}

#[tokio::test]
async fn an_own_hold_with_no_lease_is_taken_and_let_go_with_the_new_lease() {
    let (mut rig, asked, _listed, _root) = rig_listed(listed("my-pc")).await;
    quiet_passes(&mut rig).await;
    assert_eq!(ops_named(&asked, "draft_take").len(), 1);
    assert_eq!(
        ops_named(&asked, "draft_release"),
        [json!({ "id": "d1", "token": "lease-new" })]
    );
    assert!(rig.lp(0).held.stranded().is_empty(), "let go, so no longer listed");
}

#[tokio::test]
async fn a_store_that_refuses_the_take_is_asked_once_and_nothing_changes() {
    let (mut rig, asked, listed, _root) = rig_listed(listed("my-pc")).await;
    listed
        .refused
        .lock()
        .unwrap()
        .insert("draft_take".into(), (409, "conflict"));
    for _ in 0..3 {
        quiet_passes(&mut rig).await;
    }
    assert_eq!(ops_named(&asked, "draft_take").len(), 1, "not asked again for minutes");
    assert!(ops_named(&asked, "draft_release").is_empty());
    assert!(rig.lp(0).held.stranded().is_empty());
}

#[tokio::test]
async fn a_draft_held_by_another_computer_is_left_alone() {
    let (mut rig, asked, _listed, _root) = rig_listed(listed("other-pc")).await;
    quiet_passes(&mut rig).await;
    assert!(ops_named(&asked, "draft_take").is_empty() && ops_named(&asked, "draft_release").is_empty());
}

#[tokio::test]
async fn a_draft_asking_a_question_stays_its_writers() {
    let mut plan = listed("my-pc");
    plan["detail"] = json!("needs_clarification");
    let (mut rig, asked, _listed, _root) = rig_listed(plan).await;
    quiet_passes(&mut rig).await;
    assert!(ops_named(&asked, "draft_take").is_empty());
}

#[tokio::test]
async fn a_draft_with_a_known_lease_is_let_go_once_by_the_listed_lease() {
    let (mut rig, asked, _listed, _root) = rig_listed(listed("my-pc")).await;
    rig.lp(0).held.keep("d1", "lease-old");
    quiet_passes(&mut rig).await;
    assert_eq!(
        ops_named(&asked, "draft_release"),
        [json!({ "id": "d1", "token": "lease-old" })]
    );
    assert!(ops_named(&asked, "draft_take").is_empty());
}
