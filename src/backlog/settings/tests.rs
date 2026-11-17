use super::*;
use crate::backlog::tests::store;

fn change(by: &str, fields: &[&str]) -> SettingsChange {
    SettingsChange {
        by: by.into(),
        fields: fields.iter().map(|f| ChangedField { field: (*f).into() }).collect(),
    }
}

fn record(history: Vec<SettingsChange>, updated_by: Option<&str>) -> StoreSettings {
    StoreSettings {
        version: 3,
        commands_version: 1,
        values: json!({}),
        updated_by: updated_by.map(String::from),
        history,
    }
}

#[tokio::test]
async fn settings_get_posts_the_op_and_reads_the_record() {
    let (base, seen) = store(
        200,
        json!({ "value": {
            "version": 4, "commandsVersion": 2, "values": { "playCommand": "npm start" },
            "updatedBy": "ann", "updatedAt": "2026-11-01T10:00:00Z",
            "history": [{ "version": 4, "at": "x", "by": "bob", "fields": [{ "field": "playCommand" }] }] } }),
    )
    .await;
    let got = Client::new(base)
        .settings_get(7, Some("t"))
        .await
        .unwrap()
        .record
        .unwrap();
    assert_eq!(got.version, 4);
    assert_eq!(got.commands_version, 2);
    assert_eq!(got.values, json!({ "playCommand": "npm start" }));
    assert_eq!(got.history, vec![change("bob", &["playCommand"])]);
    assert_eq!(seen.lock().unwrap()[0], json!({ "op": "settings_get", "args": {} }));
}

#[tokio::test]
async fn settings_get_null_is_no_record_and_no_effective() {
    let (base, _) = store(200, json!({ "value": null })).await;
    let got = Client::new(base).settings_get(7, None).await.unwrap();
    assert_eq!((got.record, got.effective), (None, None));
}

#[tokio::test]
async fn an_answer_with_no_version_is_no_record_but_its_effective_is_taken() {
    let effective = crate::store_effective::StoreEffective::test_json();
    let (base, _) = store(200, json!({ "value": { "effective": effective } })).await;
    let got = Client::new(base).settings_get(7, None).await.unwrap();
    assert_eq!(got.record, None, "nothing saved yet");
    assert_eq!(got.effective.unwrap().max_review_rounds, 3);
    let (base, _) = store(
        200,
        json!({ "value": { "version": 1, "values": {}, "effective": effective } }),
    )
    .await;
    let got = Client::new(base).settings_get(7, None).await.unwrap();
    assert!(got.record.is_some() && got.effective.is_some(), "with a record, both");
}

#[tokio::test]
async fn settings_get_store_error_is_an_error() {
    let (base, _) = store(500, json!({ "error": "boom" })).await;
    assert!(Client::new(base).settings_get(7, None).await.is_err());
}

#[test]
fn changed_by_picks_the_last_command_change_else_updated_by() {
    let r = record(
        vec![
            change("a", &["checks"]),
            change("b", &["deploy"]),
            change("c", &["name"]),
        ],
        Some("d"),
    );
    assert_eq!(changed_by(&r).as_deref(), Some("b"));
    assert_eq!(
        changed_by(&record(vec![change("c", &["name"])], Some("d"))).as_deref(),
        Some("d")
    );
    assert_eq!(changed_by(&record(vec![], None)), None);
}
