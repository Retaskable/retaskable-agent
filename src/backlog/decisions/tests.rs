use serde_json::json;

use super::*;

#[test]
fn reads_only_decision_fields_and_ignores_the_rest() {
    let d = Decisions::from_details(&json!({
        "attempt": 2, "reviewedSha": "abc", "triages": { "slice": 3, "n": 1 }, "triaged": true,
        "timeline": [1, 2], "stats": {}
    }));
    assert_eq!(d.attempt, Some(2));
    assert_eq!(d.reviewed_sha.as_deref(), Some("abc"));
    assert_eq!(d.triages, Some(Triages { slice: 3, n: 1 }));
    assert_eq!(d.triaged, Some(true));
    assert_eq!(d.reasks, None);
}

#[test]
fn a_field_of_the_wrong_shape_reads_as_absent() {
    let d = Decisions::from_details(&json!({ "attempt": "two", "triaged": 1 }));
    assert_eq!(d, Decisions::default());
}

#[test]
fn diff_has_changed_fields_only_and_nulls_what_went() {
    let old = Decisions::from_details(&json!({ "attempt": 1, "reasks": 2, "failedIn": "ci" }));
    let new = Decisions {
        attempt: Some(2),
        reasks: Some(2),
        failed_in: None,
        triages: Some(Triages { slice: 1, n: 1 }),
        ..Default::default()
    };
    let patch = new.diff(&old);
    assert_eq!(
        Value::Object(patch),
        json!({ "attempt": 2, "failedIn": null, "triages": { "slice": 1, "n": 1 } })
    );
    assert!(new.diff(&new).is_empty());
}

#[test]
fn there_are_26_decision_fields() {
    assert_eq!(DECISION_FIELDS.len(), 26);
    let all = Decisions {
        attempt: Some(1),
        ..Default::default()
    };
    assert_eq!(all.diff(&Decisions::default()).len(), 1);
}

#[tokio::test]
async fn details_get_sends_the_id_and_reads_the_record() {
    let reply = json!({ "value": { "version": 4, "details": { "attempt": 3 } } });
    let (base, seen) = crate::backlog::tests::store(200, reply).await;
    let got = Client::new(base)
        .details_get(7, Some("tok"), "a")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.version, 4);
    assert_eq!(Decisions::from_details(&got.details).attempt, Some(3));
    let sent = seen.lock().unwrap().clone();
    assert_eq!(sent[0]["op"], "details_get");
    assert_eq!(sent[0]["args"], json!({ "id": "a" }));
}

#[tokio::test]
async fn details_get_with_no_record_is_none_and_a_failure_is_an_error() {
    let (base, _) = crate::backlog::tests::store(200, json!({ "value": null })).await;
    assert!(Client::new(base).details_get(7, None, "a").await.unwrap().is_none());
    let (base, _) = crate::backlog::tests::store(500, json!({ "error": "boom" })).await;
    assert!(Client::new(base).details_get(7, None, "a").await.is_err());
}
