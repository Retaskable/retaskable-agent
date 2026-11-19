use super::*;
use crate::backlog::tests::store;

#[tokio::test]
async fn intent_list_asks_for_the_kinds_and_reads_each_row() {
    let (base, seen) = store(
        200,
        json!({ "value": [
            { "id": "in-1", "kind": "ask", "args": { "question": "where?", "codeQuestion": true }, "issuedBy": "ana",
              "at": 1, "status": "requested" },
            { "id": "in-2", "kind": "ask", "status": "done", "result": { "answer": "x" } } ] }),
    )
    .await;
    let rows = Client::new(base).intent_list(1, Some("tok"), &["ask"]).await.unwrap();
    assert_eq!(
        seen.lock().unwrap()[0],
        json!({ "op": "intent_list", "args": { "kinds": ["ask"] } })
    );
    assert_eq!(rows[0].issued_by.as_deref(), Some("ana"));
    assert_eq!(rows[0].args["question"], "where?");
    assert!(rows[0].open());
    assert!(!rows[1].open());
    assert_eq!(rows[1].args, Value::Null);
}

#[tokio::test]
async fn a_claim_says_won_or_blocked() {
    let (base, seen) = store(200, json!({ "value": { "won": false, "blocked": true } })).await;
    let claim = Client::new(base).intent_claim(1, None, "in-1", "pc").await.unwrap();
    assert_eq!(
        claim,
        Claim {
            won: false,
            blocked: true
        }
    );
    assert_eq!(
        seen.lock().unwrap()[0],
        json!({ "op": "intent_claim", "args": { "id": "in-1", "machine": "pc", "can": true } })
    );
}

#[tokio::test]
async fn beat_and_finish_send_only_what_they_have_and_read_ok() {
    let (base, seen) = store(200, json!({ "value": { "ok": true } })).await;
    let client = Client::new(base);
    assert!(client.intent_beat(1, None, "in-1").await.unwrap());
    let result = json!({ "answer": "In src/a.rs." });
    assert!(
        client
            .intent_finish(1, None, "in-1", Ending::Done, Some(&result), None)
            .await
            .unwrap()
    );
    client
        .intent_finish(1, None, "in-1", Ending::Refused, None, Some("No."))
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0], json!({ "op": "intent_beat", "args": { "id": "in-1" } }));
    assert_eq!(
        seen[1],
        json!({ "op": "intent_finish", "args": { "id": "in-1", "status": "done", "result": { "answer": "In src/a.rs." } } })
    );
    assert_eq!(
        seen[2],
        json!({ "op": "intent_finish", "args": { "id": "in-1", "status": "refused", "why": "No." } })
    );
}

#[tokio::test]
async fn a_late_beat_is_false_and_a_store_error_is_an_error() {
    let (base, _) = store(200, json!({ "value": { "ok": false } })).await;
    assert!(!Client::new(base).intent_beat(1, None, "in-1").await.unwrap());
    for status in [400, 500] {
        let (base, _) = store(status, json!({ "error": "boom" })).await;
        let client = Client::new(base);
        assert!(client.intent_list(1, None, &[]).await.is_err(), "{status}");
        assert!(client.intent_claim(1, None, "a", "m").await.is_err(), "{status}");
        assert!(client.picture(1, None, "p.png").await.is_err(), "{status}");
    }
}

#[tokio::test]
async fn seen_sends_the_machine_and_only_the_why_it_has_with_until_for_out_of_time_alone() {
    let (base, seen) = store(200, json!({ "value": { "ok": true } })).await;
    let client = Client::new(base);
    client.intent_seen(1, None, "pc", None, Some(5)).await.unwrap();
    client
        .intent_seen(1, None, "pc", Some("paused"), Some(5))
        .await
        .unwrap();
    client
        .intent_seen(1, None, "pc", Some("out_of_time"), Some(1_800_000))
        .await
        .unwrap();
    client
        .intent_seen(1, None, "pc", Some("out_of_time"), None)
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0], json!({ "op": "intent_seen", "args": { "machine": "pc" } }));
    assert_eq!(
        seen[1],
        json!({ "op": "intent_seen", "args": { "machine": "pc", "why": "paused" } })
    );
    assert_eq!(
        seen[2],
        json!({ "op": "intent_seen", "args": { "machine": "pc", "why": "out_of_time", "until": 1_800_000 } })
    );
    assert_eq!(
        seen[3],
        json!({ "op": "intent_seen", "args": { "machine": "pc", "why": "out_of_time" } })
    );
}

#[tokio::test]
async fn a_release_sends_the_id_and_the_note_and_reads_ok() {
    let (base, seen) = store(200, json!({ "value": { "ok": true } })).await;
    let client = Client::new(base);
    assert!(
        client
            .intent_release(1, None, "in-1", Some("Out of Claude time"))
            .await
            .unwrap()
    );
    client.intent_release(1, None, "in-1", None).await.unwrap();
    client.intent_release(1, None, "in-1", Some("")).await.unwrap();
    {
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0],
            json!({ "op": "intent_release", "args": { "id": "in-1", "note": "Out of Claude time" } })
        );
        assert_eq!(seen[1], json!({ "op": "intent_release", "args": { "id": "in-1" } }));
        assert_eq!(seen[2], seen[1], "an empty note is none");
    }
    let (base, _) = store(200, json!({ "value": { "ok": false } })).await;
    assert!(!Client::new(base).intent_release(1, None, "in-1", None).await.unwrap());
}

#[tokio::test]
async fn a_claim_the_store_says_was_released_here_is_a_lost_claim() {
    let (base, _) = store(
        200,
        json!({ "value": { "won": false, "status": "requested", "released": true } }),
    )
    .await;
    let claim = Client::new(base).intent_claim(1, None, "in-1", "pc").await.unwrap();
    assert!(!claim.won && !claim.blocked);
}
