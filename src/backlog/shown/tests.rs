use serde_json::json;

use super::*;
use crate::backlog::tests::store;
use crate::store::SmokeStatus;

#[tokio::test]
async fn shown_asks_for_smoke_and_vet_and_reads_both() {
    let (base, seen) = store(
        200,
        json!({ "value": {
            "smoke": { "kind": "smoke", "updatedAt": 1, "by": "a",
                "data": { "sha": "abc", "status": "failed", "at": "2026-11-08T00:00:00.000Z", "tail": "boom" } },
            "vet": { "kind": "vet", "updatedAt": 2, "by": "a",
                "data": { "day": "2026-11-08", "view": { "at": "t", "ideas": 2, "duplicates": 1, "binned": 9, "asked": 0, "left": 3 } } } } }),
    )
    .await;
    let shown = Client::new(base).shown(1, None).await.unwrap();
    assert_eq!(
        seen.lock().unwrap()[0],
        json!({ "op": "facts_get", "args": { "kinds": ["smoke", "vet"] } })
    );
    let smoke = shown.smoke.unwrap();
    assert_eq!((smoke.sha.as_str(), smoke.status), ("abc", SmokeStatus::Failed));
    assert_eq!(smoke.tail.as_deref(), Some("boom"));
    assert_eq!(
        shown.vet.unwrap(),
        VetView {
            ideas: 2,
            duplicates: 1,
            binned: 9,
            asked: 0,
            left: 3
        }
    );
}

#[tokio::test]
async fn no_rows_or_unreadable_ones_show_nothing_and_an_error_is_an_error() {
    let (base, _) = store(200, json!({ "value": {} })).await;
    assert_eq!(Client::new(base).shown(1, None).await.unwrap(), Shown::default());
    let (base, _) = store(
        200,
        json!({ "value": { "smoke": { "data": { "status": "nonsense" } }, "vet": { "data": { "view": 4 } } } }),
    )
    .await;
    assert_eq!(Client::new(base).shown(1, None).await.unwrap(), Shown::default());
    let (base, _) = store(500, json!({ "error": "boom" })).await;
    assert!(Client::new(base).shown(1, None).await.is_err());
}
