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
