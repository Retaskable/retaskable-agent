use serde_json::json;

use super::*;
use crate::backlog::tests::store;

#[tokio::test]
async fn facts_get_sends_kinds_and_reads_the_rows_by_kind() {
    let (base, seen) = store(
        200,
        json!({ "value": { "smoke": { "kind": "smoke", "data": { "ok": true }, "updatedAt": 7, "by": "alice", "machine": "pc" } } }),
    )
    .await;
    let facts = Client::new(base).facts_get(1, None, &["smoke", "vet"]).await.unwrap();
    assert_eq!(
        seen.lock().unwrap()[0],
        json!({ "op": "facts_get", "args": { "kinds": ["smoke", "vet"] } })
    );
    assert_eq!(facts.len(), 1, "a kind with no row is simply absent");
    let smoke = &facts["smoke"];
    assert_eq!(smoke.data, json!({ "ok": true }));
    assert_eq!(
        (smoke.updated_at, smoke.by.as_deref(), smoke.machine.as_deref()),
        (7, Some("alice"), Some("pc"))
    );
}

#[tokio::test]
async fn an_error_is_an_error_never_an_empty_map() {
    for status in [400, 500] {
        let (base, _) = store(status, json!({ "error": "boom" })).await;
        assert!(
            Client::new(base).facts_get(1, None, &["smoke"]).await.is_err(),
            "{status}"
        );
    }
}

#[tokio::test]
async fn audiences_asks_for_the_kind_and_reads_the_book() {
    let (base, seen) = store(
        200,
        json!({ "value": { "audiences": { "kind": "audiences", "updatedAt": 1, "by": "a", "data": {
            "added": [ { "id": "dev", "name": "Developers", "who": "Write code", "part": "platform", "trial": false } ],
            "edits": { "admin": { "name": "Boss" } } } } } }),
    )
    .await;
    let list = Client::new(base).audiences(1, None).await.unwrap();
    assert_eq!(
        seen.lock().unwrap()[0],
        json!({ "op": "facts_get", "args": { "kinds": ["audiences"] } })
    );
    assert_eq!(list.len(), 9);
    assert_eq!(list[0].name, "Boss");
    assert_eq!(list[8].who, "Write code");
}

#[tokio::test]
async fn audiences_with_no_row_are_the_starters() {
    let (base, _) = store(200, json!({ "value": {} })).await;
    let list = Client::new(base).audiences(1, None).await.unwrap();
    assert_eq!(list.len(), 8);
}

#[tokio::test]
async fn draft_queue_sends_null_audience_and_revises() {
    let (base, seen) = store(200, json!({ "value": { "id": "d7" } })).await;
    let client = Client::new(base);
    let mut draft = QueuedDraft {
        request: "Add charts".into(),
        titles: vec!["Charts".into()],
        themes: vec!["Reports".into()],
        audience: None,
        revises: None,
        ask_id: Some("a1".into()),
        ..Default::default()
    };
    assert_eq!(client.draft_queue(1, None, &draft).await.unwrap(), "d7");
    draft.audience = Some("dev".into());
    draft.revises = Some("item-2".into());
    client.draft_queue(1, None, &draft).await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[0],
        json!({ "op": "draft_queue", "args": { "request": "Add charts", "titles": ["Charts"], "themes": ["Reports"],
            "audience": null, "revises": null, "askId": "a1" } })
    );
    assert_eq!(seen[1]["args"]["audience"], "dev");
    assert_eq!(seen[1]["args"]["revises"], "item-2");
}

#[tokio::test]
async fn project_facts_reads_the_pause_with_who_and_why() {
    let (base, seen) = store(
        200,
        json!({ "value": { "paused": { "by": "james", "at": 1, "reason": "before the demo" }, "log": [] } }),
    )
    .await;
    let facts = Client::new(base).project_facts(1, Some("tok")).await.unwrap();
    assert_eq!(seen.lock().unwrap()[0], json!({ "op": "pause_get", "args": {} }));
    assert!(facts.paused);
    assert_eq!(
        facts.paused_reason.as_deref(),
        Some("Paused by @james for everyone: before the demo")
    );
    let (base, _) = store(200, json!({ "value": { "paused": null, "log": [] } })).await;
    assert_eq!(
        Client::new(base).project_facts(1, None).await.unwrap(),
        ProjectFacts::default()
    );
}

#[tokio::test]
async fn the_pause_audiences_and_draft_queue_fail_loudly() {
    for status in [400, 500, 503] {
        let (base, _) = store(status, json!({ "error": "boom", "message": "no" })).await;
        let client = Client::new(base);
        assert!(client.project_facts(1, None).await.is_err(), "{status}");
        assert!(client.audiences(1, None).await.is_err(), "{status}");
        assert!(
            client.draft_queue(1, None, &QueuedDraft::default()).await.is_err(),
            "{status}"
        );
    }
}

#[tokio::test]
async fn draft_queue_reads_the_id_of_the_draft_the_store_answers() {
    // The store answers `{ draft }`, and a request joining a change already waiting says so beside it.
    let (base, _) = store(200, json!({ "value": { "draft": { "id": "d9" }, "joined": true } })).await;
    assert_eq!(
        Client::new(base)
            .draft_queue(1, None, &QueuedDraft::default())
            .await
            .unwrap(),
        "d9"
    );
    let (base, _) = store(200, json!({ "value": {} })).await;
    assert!(
        Client::new(base)
            .draft_queue(1, None, &QueuedDraft::default())
            .await
            .is_err()
    );
}
