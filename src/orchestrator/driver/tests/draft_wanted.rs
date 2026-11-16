//! The site's Add spec and Build this rows on a project's loop (`driver/draft_wanted.rs`): claimed in `claim` and in
//! `runs` mode, not while the project is paused (this computer's pause, or the store's) or the computer is signed out
//! of Claude, and finished with the new draft's id. The row is what the hub writes (`cloud/test/hubStoreCommands.test.ts`).
use super::asks::{Listed, finishes, ops_named, push, quiet_passes, store_failing, until};
use super::*;

fn add_spec() -> Value {
    json!({ "id": "in-1", "kind": "draft_wanted", "status": "requested", "issuedBy": "ana",
        "args": { "request": "Add a dark mode" } })
}

async fn rig_wanting() -> (Rig, Asked, Listed) {
    let (base, asked, listed) = store_failing("").await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.wanted = true;
    rig.lp(0).wanted.poll_every = Duration::ZERO;
    push(&listed, add_spec());
    (rig, asked, listed)
}

fn claims(asked: &Asked) -> usize {
    ops_named(asked, "intent_claim").len()
}

#[tokio::test]
async fn add_spec_is_claimed_queued_as_a_draft_and_finished_with_its_id() {
    let (mut rig, asked, _) = rig_wanting().await;
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["id"], "in-1");
    assert_eq!(sent[0]["status"], "done");
    assert_eq!(sent[0]["result"], json!({ "id": "draft-new" }));
    let queued = ops_named(&asked, "draft_queue");
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["request"], "Add a dark mode");
    assert_eq!(queued[0]["requestedBy"], "ana");
    assert_eq!(queued[0]["assignee"], "ana");
    let lists = ops_named(&asked, "intent_list");
    assert!(lists.iter().any(|a| a["kinds"] == json!(["draft_wanted"])));
}

#[tokio::test]
async fn it_is_taken_in_runs_mode_too() {
    let (mut rig, asked, _) = rig_wanting().await;
    rig.go_runs();
    let sent = rig.runs_until(&asked, 1).await;
    assert_eq!(sent[0]["status"], "done");
    assert_eq!(ops_named(&asked, "draft_queue").len(), 1);
}

#[tokio::test]
async fn a_paused_project_claims_none_until_it_is_resumed() {
    let (mut rig, asked, _) = rig_wanting().await;
    rig.pause("p0", true);
    quiet_passes(&mut rig).await;
    assert_eq!(claims(&asked), 0);
    assert!(ops_named(&asked, "draft_queue").is_empty());
    rig.pause("p0", false);
    until(&mut rig, &asked, 1).await;
    assert_eq!(claims(&asked), 1);
}

#[tokio::test]
async fn a_computer_signed_out_of_claude_claims_none_until_it_is_signed_in() {
    let (mut rig, asked, _) = rig_wanting().await;
    rig.machine.claude_signed_out_until = Some(std::time::Instant::now() + Duration::from_secs(3600));
    quiet_passes(&mut rig).await;
    assert_eq!(claims(&asked), 0);
    rig.machine.claude_signed_out_until = None;
    until(&mut rig, &asked, 1).await;
    assert_eq!(finishes(&asked).len(), 1);
}

#[tokio::test]
async fn several_rows_are_each_taken_and_finished() {
    let (mut rig, asked, listed) = rig_wanting().await;
    for n in 2..=3 {
        let mut row = add_spec();
        row["id"] = json!(format!("in-{n}"));
        push(&listed, row);
    }
    // Two run at once (`MAX_WORK_ROWS`); the third is taken on a later pass, none dropped.
    until(&mut rig, &asked, 3).await;
    assert_eq!(ops_named(&asked, "draft_queue").len(), 3);
}
