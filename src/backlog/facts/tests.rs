use super::*;
use crate::backlog::tests::store;

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
async fn project_facts_asks_for_the_kind_and_reads_the_pause_and_spend() {
    let (base, seen) = store(
        200,
        json!({ "value": { "paused": true, "pausedReason": "Paused by @james for everyone", "spentTodayUsd": 1.5 } }),
    )
    .await;
    let facts = Client::new(base).project_facts(1, Some("tok")).await.unwrap();
    assert_eq!(
        seen.lock().unwrap()[0],
        json!({ "op": "facts_get", "args": { "kind": "project" } })
    );
    assert!(facts.paused);
    assert_eq!(facts.paused_reason.as_deref(), Some("Paused by @james for everyone"));
    assert_eq!(facts.spent_today_usd, 1.5);
}

#[tokio::test]
async fn a_store_error_or_5xx_is_an_error_never_an_empty_answer() {
    for status in [400, 500, 503] {
        let (base, _) = store(status, json!({ "error": "boom", "message": "no" })).await;
        let client = Client::new(base);
        assert!(client.project_facts(1, None).await.is_err(), "{status}");
        assert!(
            client.draft_queue(1, None, &QueuedDraft::default()).await.is_err(),
            "{status}"
        );
    }
    let (base, _) = store(200, json!({ "ok": true })).await;
    assert!(Client::new(base).project_facts(1, None).await.is_err(), "no value");
}
