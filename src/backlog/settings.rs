//! The store's project settings record (Retaskable.com's `shared/projectSettings.ts`, `ProjectSettings`), read with the
//! `settings_get` op. A failed read is an error to the caller, which keeps what it had: no cache file, no fallback.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{BacklogError, Client};

/// The settings fields whose values run on the computer (`COMMAND_FIELDS` there, by top-level name).
const COMMAND_FIELDS: [&str; 10] = [
    "checks",
    "playCommand",
    "promptNotes",
    "ciCommand",
    "galleries",
    "phone",
    "engines",
    "appsScript",
    "deploy",
    "nfrs",
];

/// One field a change touched.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChangedField {
    pub field: String,
}

/// One change in the record's history.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SettingsChange {
    pub by: String,
    #[serde(default)]
    pub fields: Vec<ChangedField>,
}

/// The project's settings as the store holds them.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreSettings {
    pub version: u64,
    /// The version of the command fields, which the computer's owner approves (0 before any).
    #[serde(default)]
    pub commands_version: u64,
    /// The settings values, as the store keeps them.
    #[serde(default)]
    pub values: Value,
    #[serde(default)]
    pub updated_by: Option<String>,
    #[serde(default)]
    pub history: Vec<SettingsChange>,
}

/// Who last changed a command field, from the history, else who last changed the record.
pub fn changed_by(settings: &StoreSettings) -> Option<String> {
    settings
        .history
        .iter()
        .rev()
        .find(|c| c.fields.iter().any(|f| COMMAND_FIELDS.contains(&f.field.as_str())))
        .map(|c| c.by.clone())
        .or_else(|| settings.updated_by.clone())
}

impl Client {
    /// `settings_get`: the project's settings, or `None` when no one has set any yet.
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
mod tests;
