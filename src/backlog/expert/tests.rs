use super::*;
use crate::backlog::tests::store;

fn row(id: &str, state: &str) -> Value {
    json!({ "id": id, "text": "do it", "at": 1_700_000_000_000u64, "by": "ada", "state": state })
}

#[tokio::test]
async fn add_sends_the_sender_and_pictures_only_when_given() {
    let (base, seen) = store(200, json!({ "value": { "id": "q-1" } })).await;
    let c = Client::new(base);
    assert_eq!(
        c.expert_queue_add(3, Some("t"), "e-1", "hi", "ada", &[]).await.unwrap(),
        "q-1"
    );
    c.expert_queue_add(3, Some("t"), "e-1", "hi", "ada", &["p1".to_string()])
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["op"], "expert_queue_add");
    assert_eq!(seen[0]["args"], json!({ "itemId": "e-1", "text": "hi", "by": "ada" }));
    assert_eq!(seen[1]["args"]["pictureIds"], json!(["p1"]));
}

#[tokio::test]
async fn take_reads_the_rows_it_holds_and_sends_ids_only_when_given() {
    let mut taken = row("q-1", "taken");
    taken["claim"] = json!({ "agentId": "a1", "machine": "box", "at": 5 });
    taken["pictureIds"] = json!(["p1"]);
    let (base, seen) = store(200, json!({ "value": { "rows": [taken] } })).await;
    let c = Client::new(base);
    let rows = c.expert_queue_take(3, None, "e-1", None, "box").await.unwrap();
    assert_eq!(rows[0].state, RowState::Taken);
    assert_eq!(rows[0].claim.as_ref().unwrap().machine, "box");
    assert_eq!(rows[0].picture_ids, vec!["p1".to_string()]);
    c.expert_queue_take(3, None, "e-1", Some(&["q-1".to_string()]), "box")
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["args"], json!({ "itemId": "e-1", "machine": "box" }));
    assert_eq!(seen[1]["args"]["ids"], json!(["q-1"]));
}

#[tokio::test]
async fn a_second_computers_take_is_a_conflict_not_an_empty_answer() {
    let (base, _) = store(
        409,
        json!({ "error": "conflict", "message": "Another computer (box) already took that message" }),
    )
    .await;
    let err = Client::new(base)
        .expert_queue_take(3, None, "e-1", None, "other")
        .await
        .unwrap_err();
    assert!(err.is_conflict());
}

#[tokio::test]
async fn done_says_whether_it_held_every_row() {
    let (base, seen) = store(200, json!({ "value": { "ok": false } })).await;
    let ok = Client::new(base)
        .expert_queue_done(3, None, "e-1", &["q-1".to_string()])
        .await
        .unwrap();
    assert!(!ok);
    assert_eq!(
        seen.lock().unwrap()[0]["args"],
        json!({ "itemId": "e-1", "ids": ["q-1"] })
    );
}

#[tokio::test]
async fn clear_and_the_continuation_note_hold_no_path() {
    let (base, seen) = store(200, json!({ "value": { "ok": true } })).await;
    let c = Client::new(base);
    c.expert_queue_clear(3, None, "e-1").await.unwrap();
    let from = ExpertFrom {
        session_id: "abc-123".into(),
        title: "Fix the chart".into(),
        branch: None,
    };
    c.expert_continues_set(3, None, "e-1", &from).await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["op"], "expert_queue_clear");
    assert_eq!(seen[1]["op"], "expert_continues_set");
    assert_eq!(
        seen[1]["args"]["from"],
        json!({ "sessionId": "abc-123", "title": "Fix the chart" })
    );
}

#[tokio::test]
async fn list_reads_every_sessions_rows_and_notes() {
    let (base, seen) = store(
        200,
        json!({ "value": {
            "sessions": { "e-1": [row("q-1", "queued")] },
            "continues": { "e-1": { "sessionId": "s1", "title": "T", "branch": "b" } } } }),
    )
    .await;
    let c = Client::new(base);
    let all = c.expert_queue_list(3, None, None).await.unwrap();
    assert_eq!(all.sessions["e-1"][0].state, RowState::Queued);
    assert_eq!(all.continues["e-1"].branch.as_deref(), Some("b"));
    c.expert_queue_list(3, None, Some("e-1")).await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["args"], json!({}));
    assert_eq!(seen[1]["args"], json!({ "itemId": "e-1" }));
}

#[tokio::test]
async fn an_unreachable_store_is_an_error() {
    let err = Client::new("http://127.0.0.1:1")
        .expert_queue_list(3, None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, BacklogError::Unreachable(_)));
}

#[test]
fn a_session_id_is_word_characters_and_dashes_up_to_100() {
    assert!(ExpertFrom::session_id_ok("abc-123_X"));
    assert!(ExpertFrom::session_id_ok(&"a".repeat(100)));
    assert!(!ExpertFrom::session_id_ok(""));
    assert!(!ExpertFrom::session_id_ok(&"a".repeat(101)));
    assert!(!ExpertFrom::session_id_ok("../etc"));
    assert!(!ExpertFrom::session_id_ok("a b"));
}

#[tokio::test]
async fn add_sends_an_expert_item_claimed_by_this_agent_and_reads_its_lease() {
    let item = json!({ "id": "expert-x", "title": "X", "status": "claimed", "rank": 1 });
    let (base, seen) = store(200, json!({ "value": { "item": item, "token": "lease-1" } })).await;
    let c = Client::new(base);
    let agent = crate::backlog::ClaimNextAgent {
        machine: "box".into(),
        environment: "dev".into(),
        base_branch: "main".into(),
        tags: vec![],
    };
    let it = ExpertItem {
        id: "expert-x".into(),
        title: "X".into(),
        remaining: "do X".into(),
    };
    let added = c.expert_add(3, Some("t"), &it, "ada", &agent).await.unwrap();
    assert_eq!((added.item.id.as_str(), added.token.as_str()), ("expert-x", "lease-1"));
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["op"], "add");
    assert_eq!(seen[0]["args"]["via"], "expert");
    assert_eq!(seen[0]["args"]["startedBy"], "ada");
    assert_eq!(seen[0]["args"]["agent"]["machine"], "box");
    assert_eq!(seen[0]["args"]["items"][0]["remaining"], "do X");
}

#[tokio::test]
async fn a_beat_is_a_report_with_no_note() {
    let (base, seen) = store(200, json!({ "value": null })).await;
    Client::new(base).lease_beat(3, None, "e-1", "lease-1").await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(
        (seen[0]["op"].as_str(), &seen[0]["args"]),
        (Some("report"), &json!({ "id": "e-1", "token": "lease-1" }))
    );
}
