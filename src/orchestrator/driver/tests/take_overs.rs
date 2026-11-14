//! Take over (`driver/take_overs.rs`): a `take_over_requested` row claims the item by name as a person's action, and
//! the loop holds it as an item that needs help. A fake store lists the rows and answers the claim as scripted.
use super::asks::{Listed, finishes, ops_named, push, quiet_passes, store_failing, until};
use super::*;

fn row(item: &str) -> Value {
    json!({ "id": "in-1", "kind": "take_over_requested", "status": "requested", "issuedBy": "ana",
        "args": { "itemId": item } })
}

fn claimed(waiting: &str) -> (u16, Value) {
    (
        200,
        json!({ "value": { "token": "lease-1", "wasWaiting": waiting,
            "wasOwner": { "login": "bo", "machine": "bo-pc" },
            "item": { "id": "4", "title": "T", "status": "claimed", "rank": 1, "branch": "retaskable/four" } } }),
    )
}

async fn rig_with_store(answer: (u16, Value)) -> (Rig, Asked, Listed) {
    let (base, asked, listed) = store_failing("").await;
    *listed.person_claim.lock().unwrap() = Some(answer);
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.take_overs = true;
    rig.lp(0).take_overs.poll_every = Duration::ZERO;
    (rig, asked, listed)
}

#[tokio::test]
async fn an_item_waiting_for_help_becomes_this_computers_and_the_row_is_done() {
    let (mut rig, asked, listed) = rig_with_store(claimed("help")).await;
    push(&listed, row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        sent[0],
        json!({ "id": "in-1", "status": "done", "result": { "itemId": "4" } })
    );
    let claim = &ops_named(&asked, "claim")[0];
    assert_eq!(claim["id"], "4");
    assert_eq!(claim["because"], "person");
    assert_eq!(claim["did"], "took_over");
    assert_eq!(
        ops_named(&asked, "intent_list")[0],
        json!({ "kinds": ["take_over_requested"] })
    );
    rig.machine.pass().await;
    let lp = rig.lp(0);
    assert_eq!(lp.runs["4"].state, ItemState::NeedsHuman);
    assert_eq!(lp.leases["4"], "lease-1");
    assert_eq!(lp.items["4"].branch.as_deref(), Some("retaskable/four"));
    assert_eq!(lp.records["4"].state, "needs_human");
    assert!(lp.records["4"].lane.is_none(), "it holds no lane");
}

#[tokio::test]
async fn a_refusal_ends_the_row_in_the_stores_words_and_holds_nothing() {
    let refusal = (
        409,
        json!({ "error": "held_by_owner", "message": "x",
            "why": { "reason": "held", "login": "bo", "machine": "bo-pc" } }),
    );
    let (mut rig, asked, listed) = rig_with_store(refusal).await;
    push(&listed, row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "refused");
    assert_eq!(
        sent[0]["why"],
        "bo-pc has been online in the last 2 hours, so it's still theirs. Carry it on, or ask @bo."
    );
    rig.machine.pass().await;
    assert!(rig.lp(0).runs.is_empty());
    assert!(rig.lp(0).leases.is_empty());
}

#[tokio::test]
async fn an_item_that_waits_for_something_else_is_given_back() {
    let (mut rig, asked, listed) = rig_with_store(claimed("merge")).await;
    push(&listed, row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "refused");
    let released = ops_named(&asked, "release");
    assert_eq!(released.len(), 1);
    assert_eq!(
        (released[0]["id"].as_str(), released[0]["token"].as_str()),
        (Some("4"), Some("lease-1"))
    );
    rig.machine.pass().await;
    assert!(rig.lp(0).runs.is_empty());
}

#[tokio::test]
async fn an_item_held_here_already_is_refused_with_no_claim() {
    let (mut rig, asked, listed) = rig_with_store(claimed("help")).await;
    rig.queue(0, &["4"]);
    push(&listed, row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["why"], "It's on this computer already.");
    assert!(ops_named(&asked, "claim").is_empty());
    assert_eq!(finishes(&asked).len(), 1);
}

#[tokio::test]
async fn a_row_is_taken_over_once_though_the_store_keeps_listing_it() {
    let (mut rig, asked, listed) = rig_with_store(claimed("help")).await;
    push(&listed, row("4"));
    until(&mut rig, &asked, 1).await;
    listed.rows.lock().unwrap()[0]["status"] = json!("running");
    quiet_passes(&mut rig).await;
    assert_eq!(ops_named(&asked, "claim").len(), 1);
    assert_eq!(finishes(&asked).len(), 1);
}
