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
