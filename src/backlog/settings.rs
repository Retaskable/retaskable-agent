//! The store's project settings (`settings_get`, Retaskable.com's `shared/projectSettings.ts`). A failed
//! read is an error to the caller, which keeps what it had: no cache file, no fallback.

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{BacklogError, Client};

/// The store's project settings record (`ProjectSettings`); fields this agent doesn't read are left out.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreSettings {
    #[serde(default)]
    pub version: u64,
    /// Rising only when a command changes; 0 before any.
    #[serde(default)]
    pub commands_version: u64,
    #[serde(default)]
    pub values: Map<String, Value>,
    #[serde(default)]
    pub updated_by: Option<String>,
    #[serde(default)]
    pub history: Vec<SettingsChange>,
}

/// One change in the record's history, oldest first.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SettingsChange {
    #[serde(default)]
    pub by: Option<String>,
    #[serde(default)]
    pub fields: Vec<ChangedField>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChangedField {
    pub field: String,
}

/// The settings that are commands (`settingFields("command")`, plus `deploy` and `nfrs`).
const COMMAND_FIELDS: [&str; 9] = [
    "checks",
    "playCommand",
    "promptNotes",
    "ciCommand",
    "galleries",
    "phone",
    "engines",
    "appsScript",
    "deploy",
];

fn is_command_field(field: &str) -> bool {
    field == "nfrs" || COMMAND_FIELDS.contains(&field)
}

/// Who last changed a command field, from the record's history, else `updatedBy`.
pub fn changed_by(record: &StoreSettings) -> Option<String> {
    record
        .history
        .iter()
        .rev()
        .find(|c| c.fields.iter().any(|f| is_command_field(&f.field)))
        .map_or_else(|| record.updated_by.clone(), |c| c.by.clone())
}

impl Client {
    /// `settings_get`: the project's settings, or `None` before anyone set them.
    pub async fn settings_get(&self, repo_id: u64, token: Option<&str>) -> Result<Option<StoreSettings>, BacklogError> {
        let (status, body) = self.send(repo_id, token, "settings_get", json!({})).await?;
        match body.value {
            None | Some(Value::Null) => Ok(None),
            Some(v) => serde_json::from_value(v)
                .map(Some)
                .map_err(|e| BacklogError::BadAnswer {
                    status,
                    code: None,
                    message: format!("couldn't read the project settings it sent: {e}"),
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::tests::store;

    fn record() -> Value {
        json!({
            "version": 7, "commandsVersion": 3, "values": { "checks": [], "playCommand": "npm start" },
            "updatedBy": "owner", "updatedAt": 5,
            "history": [
                { "by": "ann", "at": 1, "fields": [{ "field": "checks" }] },
                { "by": "bob", "at": 2, "fields": [{ "field": "boardColumns" }] },
            ]
        })
    }

    #[tokio::test]
    async fn settings_get_posts_its_op_and_reads_a_full_record() {
        let (base, seen) = store(200, json!({ "value": record() })).await;
        let s = Client::new(base).settings_get(1, Some("tok")).await.unwrap().unwrap();
        let sent = seen.lock().unwrap()[0].clone();
        assert_eq!(sent, json!({ "op": "settings_get", "args": {} }));
        assert_eq!((s.version, s.commands_version), (7, 3));
        assert_eq!(s.values["playCommand"], "npm start");
        assert_eq!(s.history.len(), 2);
        assert_eq!(s.history[0].fields[0].field, "checks");
    }

    #[tokio::test]
    async fn settings_get_reads_null_as_none() {
        let (base, _) = store(200, json!({ "value": null })).await;
        assert_eq!(Client::new(base).settings_get(1, None).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_store_error_is_an_error_not_a_default_record() {
        let (base, _) = store(500, json!({ "error": "boom" })).await;
        assert!(Client::new(base).settings_get(1, None).await.is_err());
    }

    #[test]
    fn changed_by_picks_the_last_command_change_else_updated_by() {
        let mut s: StoreSettings = serde_json::from_value(record()).unwrap();
        assert_eq!(changed_by(&s).as_deref(), Some("ann"));
        s.history.clear();
        assert_eq!(changed_by(&s).as_deref(), Some("owner"));
    }
}
