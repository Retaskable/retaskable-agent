//! Writes to `.retaskable/config.json` from the settings page (agent-settings.md section 4):
//! keeps every key this agent doesn't parse (`engines`, `cursor`, `github`, a future agent's own)
//! untouched, the same way `store::machine::MachineStore` keeps `machine.json`'s unknown keys.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use crate::store::write_json_atomic;

const CONFIG_FILE: &str = "config.json";

/// The config file's raw JSON, for saving a single setting without disturbing the rest.
#[derive(Debug)]
pub struct ConfigStore {
    file: PathBuf,
    data: Map<String, Value>,
}

impl ConfigStore {
    /// Loads `<dir>/config.json`, or a fresh version-2 file with no projects when there's none
    /// yet: an empty object would otherwise read as a version-1 file with one empty project.
    pub fn load(dir: &Path) -> Result<Self> {
        let file = dir.join(CONFIG_FILE);
        let data = match std::fs::read_to_string(&file) {
            Ok(text) => parse_object(&text).with_context(|| format!("reading {}", file.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => empty_config(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
        };
        Ok(ConfigStore { file, data })
    }

    /// Sets a top-level key's value, kept in memory until `save`.
    pub fn set(&mut self, key: &str, value: Value) {
        self.data.insert(key.to_string(), value);
    }

    /// Writes the file atomically, compact.
    pub fn save(&self) -> Result<()> {
        write_json_atomic(&self.file, &Value::Object(self.data.clone()), true)
    }
}

fn empty_config() -> Map<String, Value> {
    let mut data = Map::new();
    data.insert("version".to_string(), Value::from(2));
    data.insert("projects".to_string(), Value::Array(vec![]));
    data
}

fn parse_object(text: &str) -> Result<Map<String, Value>> {
    match serde_json::from_str(text)? {
        Value::Object(map) => Ok(map),
        _ => Ok(empty_config()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_loads_as_an_empty_version_2_file_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::load(dir.path()).unwrap();
        assert_eq!(store.data.get("version"), Some(&Value::from(2)));
        assert!(!dir.path().join(CONFIG_FILE).exists());
    }

    #[test]
    fn a_changed_key_is_saved_and_other_keys_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            r#"{"version":2,"port":4100,"engines":{"custom":true},"projects":[]}"#,
        )
        .unwrap();
        let mut store = ConfigStore::load(dir.path()).unwrap();
        store.set("port", Value::from(4200));
        store.save().unwrap();
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap()).unwrap();
        assert_eq!(saved["port"], Value::from(4200));
        assert_eq!(saved["engines"], serde_json::json!({ "custom": true }));
    }
}
