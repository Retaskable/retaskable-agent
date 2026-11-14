use super::*;
use crate::backlog::tests::store;

#[tokio::test]
async fn asks_list_reads_every_kind_of_ask() {
    let (base, seen) = store(
        200,
        json!({ "value": { "asks": [
            { "id": "a1", "text": "why?", "answer": "because", "from": "person", "about": "item-1", "cleared": true,
              "status": "done", "createdAt": "2026-11-01T00:00:00Z", "images": ["x.png"] },
            { "id": "a2", "text": "Do you want to approve it?", "from": "retaskable", "draftId": "d1", "replyTo": "a1" },
            { "id": "a3", "text": "hi" } ] } }),
    )
    .await;
    let asks = Client::new(base).asks_list(1, Some("tok")).await.unwrap();
    assert_eq!(seen.lock().unwrap()[0], json!({ "op": "asks_list", "args": {} }));
    assert_eq!(asks.len(), 3);
    assert!(asks[0].cleared);
    assert_eq!(asks[0].answer.as_deref(), Some("because"));
    assert_eq!(asks[0].about.as_deref(), Some("item-1"));
    assert_eq!(asks[0].created_at.as_deref(), Some("2026-11-01T00:00:00Z"));
    assert_eq!(asks[0].images.len(), 1);
    assert_eq!(asks[1].from.as_deref(), Some("retaskable"));
    assert_eq!(asks[1].draft_id.as_deref(), Some("d1"));
    assert_eq!(asks[1].reply_to.as_deref(), Some("a1"));
    assert_eq!(asks[2].answer, None);
    assert!(!asks[2].cleared);
    assert!(asks[2].images.is_empty());
}

#[tokio::test]
async fn ask_report_sends_only_the_fields_given() {
    let (base, seen) = store(200, json!({ "value": null })).await;
    let report = AskReport {
        answer: Some("done".into()),
        actions: Some(vec![json!({ "type": "pause", "ok": true })]),
        ..AskReport::default()
    };
    Client::new(base).ask_report(1, None, "a1", &report).await.unwrap();
    assert_eq!(
        seen.lock().unwrap()[0],
        json!({ "op": "ask_report", "args": { "id": "a1", "answer": "done", "actions": [{ "type": "pause", "ok": true }] } })
    );
}

#[tokio::test]
async fn ask_post_sends_a_retaskable_message_into_a_conversation() {
    let (base, seen) = store(200, json!({ "value": { "id": "a9" } })).await;
    let client = Client::new(base);
    assert_eq!(
        client.ask_post(1, None, "Approve?", "d1", Some("a1")).await.unwrap(),
        "a9"
    );
    client.ask_post(1, None, "Approve?", "d1", None).await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[0],
        json!({ "op": "ask_report", "args": { "from": "retaskable", "text": "Approve?", "draftId": "d1", "replyTo": "a1" } })
    );
    assert!(seen[1]["args"].get("replyTo").is_none());
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
async fn a_store_error_or_5xx_is_an_error_never_an_empty_answer() {
    for status in [400, 500, 503] {
        let (base, _) = store(status, json!({ "error": "boom", "message": "no" })).await;
        let client = Client::new(base);
        assert!(client.asks_list(1, None).await.is_err(), "{status}");
        assert!(client.audiences(1, None).await.is_err(), "{status}");
        assert!(
            client.ask_report(1, None, "a", &AskReport::default()).await.is_err(),
            "{status}"
        );
        assert!(client.ask_post(1, None, "t", "d", None).await.is_err(), "{status}");
    }
    let (base, _) = store(200, json!({ "ok": true })).await;
    assert!(Client::new(base).asks_list(1, None).await.is_err(), "no value");
}
