use super::*;
use crate::backlog::tests::store;

fn agent() -> ClaimNextAgent {
    ClaimNextAgent {
        machine: "box".into(),
        environment: "Production".into(),
        base_branch: "main".into(),
        tags: vec![],
    }
}

#[tokio::test]
async fn claims_by_name_as_a_person_and_reads_what_it_waited_for() {
    let (base, seen) = store(
        200,
        json!({ "value": { "item": { "id": "4", "title": "T", "status": "needs_person", "rank": 1, "branch": "b" },
            "token": "t1", "wasWaiting": "help", "wasOwner": { "login": "ana", "machine": "ana-pc" } } }),
    )
    .await;
    let got = Client::new(base)
        .take_over(1, Some("tok"), "4", &agent())
        .await
        .unwrap();
    assert_eq!(got.token, "t1");
    assert_eq!(got.was_waiting.as_deref(), Some("help"));
    assert_eq!(got.item.branch.as_deref(), Some("b"));
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "claim");
    assert_eq!(sent["args"]["id"], "4");
    assert_eq!(sent["args"]["because"], "person");
    assert_eq!(sent["args"]["did"], "took_over");
    assert_eq!(sent["args"]["agent"]["machine"], "box");
}

#[tokio::test]
async fn a_claim_of_an_item_that_was_not_parked_has_no_wait() {
    let (base, _) = store(
        200,
        json!({ "value": { "item": { "id": "4", "title": "T", "status": "claimed", "rank": 1 }, "token": "t1" } }),
    )
    .await;
    let got = Client::new(base).take_over(1, None, "4", &agent()).await.unwrap();
    assert_eq!(got.was_waiting, None);
}

#[tokio::test]
async fn a_computer_working_on_it_is_refused_naming_who() {
    let (base, _) = store(
        409,
        json!({ "error": "conflict", "message": "4 is taken by ana on ana-pc",
            "why": { "reason": "held", "login": "ana", "machine": "ana-pc" } }),
    )
    .await;
    let why = Client::new(base).take_over(1, None, "4", &agent()).await.unwrap_err();
    assert_eq!(
        why,
        "@ana's computer (ana-pc) is working on this right now. It'll come back to you if it needs you."
    );
}

#[tokio::test]
async fn an_owner_online_within_two_hours_keeps_it() {
    let (base, _) = store(
        409,
        json!({ "error": "held_by_owner", "message": "x",
            "why": { "reason": "held", "login": "ana", "machine": "ana-pc" } }),
    )
    .await;
    let why = Client::new(base).take_over(1, None, "4", &agent()).await.unwrap_err();
    assert_eq!(
        why,
        "ana-pc has been online in the last 2 hours, so it's still theirs. Carry it on, or ask @ana."
    );
}

#[tokio::test]
async fn other_refusals_and_an_unreachable_store_say_so_in_words() {
    let (base, _) = store(
        409,
        json!({ "error": "conflict", "message": "4 can't be claimed while it is on_main" }),
    )
    .await;
    let why = Client::new(base).take_over(1, None, "4", &agent()).await.unwrap_err();
    assert_eq!(
        why,
        "It can't be taken over in the shared backlog: 4 can't be claimed while it is on_main"
    );
    let why = Client::new("http://127.0.0.1:1")
        .take_over(1, None, "4", &agent())
        .await
        .unwrap_err();
    assert!(why.starts_with("Couldn't reach the shared backlog"), "{why}");
}
