//! Finish (`driver/finish_steps.rs`): a `finish_requested` row builds an item that needs help here, with nothing on a
//! branch, at the base branch's head. A fake store lists the rows and takes the `complete`; a fake host reads the head.
use super::asks::{Listed, finishes, ops_named, push, quiet_passes, store_failing, until};
use super::*;
use crate::orchestrator::driver::finish_steps::FinishHost;

struct Head(Result<(String, bool), String>);

impl FinishHost for Head {
    fn base_head(
        &self,
        _repo: std::path::PathBuf,
        _github_repo: String,
        _base: String,
    ) -> futures_util::future::BoxFuture<'static, anyhow::Result<(String, bool)>> {
        let head = self.0.clone().map_err(|e| anyhow::anyhow!(e));
        Box::pin(async move { head })
    }
}

fn row(item: &str) -> Value {
    json!({ "id": "in-1", "kind": "finish_requested", "status": "requested", "issuedBy": "ana",
        "args": { "itemId": item } })
}

/// A project holding item 4 as needing help, with its lease.
async fn rig_holding(fail: &'static str, head: Result<(String, bool), String>) -> (Rig, Asked, Listed) {
    let (base, asked, listed) = store_failing(fail).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.finish_host = Some(Arc::new(Head(head)));
    rig.queue(0, &["4"]);
    let lp = rig.lp(0);
    lp.runs.get_mut("4").unwrap().state = ItemState::NeedsHuman;
    lp.leases.insert("4".into(), "lease-1".into());
    lp.adopted = true;
    lp.finishing.poll_every = Duration::ZERO;
    (rig, asked, listed)
}

fn built() -> Result<(String, bool), String> {
    Ok(("abc123".into(), true))
}

#[tokio::test]
async fn an_item_that_needs_help_with_nothing_on_a_branch_is_built_at_the_base_head() {
    let (mut rig, asked, listed) = rig_holding("", built()).await;
    push(&listed, row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        sent[0],
        json!({ "id": "in-1", "status": "done", "result": { "itemId": "4", "sha": "abc123" } })
    );
    let done = &ops_named(&asked, "complete")[0];
    assert_eq!(done["id"], "4");
    assert_eq!(done["token"], "lease-1");
    assert_eq!(done["built"], true);
    assert_eq!(done["mergeCommit"], "abc123");
    assert_eq!(done["onDefaultBranch"], true);
    assert_eq!(done["remaining"], "");
    let lp = rig.lp(0);
    assert!(lp.runs.is_empty() && lp.items.is_empty() && lp.leases.is_empty() && lp.records.is_empty());
}

#[tokio::test]
async fn an_item_with_a_branch_is_refused_and_nothing_is_completed() {
    let (mut rig, asked, listed) = rig_holding("", built()).await;
    rig.lp(0).items.get_mut("4").unwrap().branch = Some("retaskable/four".into());
    push(&listed, row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "refused");
    assert_eq!(
        sent[0]["why"],
        "This item isn't waiting for a person's steps, and has work on its branch."
    );
    assert!(ops_named(&asked, "complete").is_empty());
    assert!(rig.lp(0).runs.contains_key("4"));
}

#[tokio::test]
async fn an_item_this_computer_doesnt_hold_is_refused() {
    let (mut rig, asked, listed) = rig_holding("", built()).await;
    push(&listed, row("9"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["why"], "It isn't on this computer.");
    assert!(ops_named(&asked, "complete").is_empty());
}

#[tokio::test]
async fn an_item_that_is_not_waiting_for_a_person_is_refused() {
    let (mut rig, asked, listed) = rig_holding("", built()).await;
    rig.lp(0).runs.get_mut("4").unwrap().state = ItemState::Implementing;
    push(&listed, row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["why"], "It isn't waiting for a person on this computer.");
    assert!(ops_named(&asked, "complete").is_empty());
}

#[tokio::test]
async fn a_base_head_that_cant_be_read_refuses_and_keeps_the_item() {
    let (mut rig, asked, listed) = rig_holding("", Err("offline".into())).await;
    push(&listed, row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "refused");
    assert!(sent[0]["why"].as_str().unwrap().contains("head couldn't be read"));
    assert!(ops_named(&asked, "complete").is_empty());
    assert_eq!(rig.lp(0).leases["4"], "lease-1");
}

#[tokio::test]
async fn a_store_that_refuses_the_complete_refuses_the_row_and_keeps_the_item() {
    let (mut rig, asked, listed) = rig_holding("complete", built()).await;
    push(&listed, row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "refused");
    assert!(
        sent[0]["why"]
            .as_str()
            .unwrap()
            .starts_with("The shared backlog didn't take it")
    );
    assert!(rig.lp(0).runs.contains_key("4"));
    assert_eq!(rig.lp(0).leases["4"], "lease-1");
}

#[tokio::test]
async fn a_row_is_finished_once_though_the_store_keeps_listing_it() {
    let (mut rig, asked, listed) = rig_holding("", built()).await;
    push(&listed, row("4"));
    until(&mut rig, &asked, 1).await;
    listed.rows.lock().unwrap()[0]["status"] = json!("running");
    quiet_passes(&mut rig).await;
    assert_eq!(ops_named(&asked, "complete").len(), 1);
    assert_eq!(finishes(&asked).len(), 1);
}
