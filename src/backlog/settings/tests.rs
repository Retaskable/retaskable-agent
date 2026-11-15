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
    let got = Client::new(base).settings_get(7, Some("t")).await.unwrap().unwrap();
    assert_eq!(got.version, 4);
    assert_eq!(got.commands_version, 2);
    assert_eq!(got.values, json!({ "playCommand": "npm start" }));
    assert_eq!(got.history, vec![change("bob", &["playCommand"])]);
    assert_eq!(seen.lock().unwrap()[0], json!({ "op": "settings_get", "args": {} }));
}

#[tokio::test]
async fn settings_get_null_is_none() {
    let (base, _) = store(200, json!({ "value": null })).await;
    assert_eq!(Client::new(base).settings_get(7, None).await.unwrap(), None);
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
