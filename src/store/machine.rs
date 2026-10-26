//! This computer's `.retaskable/machine.json` (agent-settings.md section 3; TypeScript's
//! `MachineStore`). Holds the computer-only settings that aren't in `config.json` — today the
//! tag overrides (computer-tags.md section 3.1). Other keys an older agent wrote (usage, spend,
//! network, …) round-trip untouched until their own ports land here.
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use super::write_json_atomic;
use crate::network::NetworkAccess;
use crate::tags::TagOverrides;

const MACHINE_FILE: &str = "machine.json";

/// Keys setup or the settings page may write while the agent is running: a save keeps a newer
/// value from the file rather than writing back this process's older one (TypeScript's
/// `SET_ELSEWHERE`).
const SET_ELSEWHERE: [&str; 2] = ["tags", "network"];

/// This computer's on-disk machine state.
#[derive(Debug)]
pub struct MachineStore {
    file: PathBuf,
    data: Map<String, Value>,
    /// `SET_ELSEWHERE`'s values as last read or written here.
    seen: String,
}

impl MachineStore {
    /// Loads `<dir>/machine.json` (empty when there's none yet). Does not create the file.
    pub fn load(dir: &Path) -> Result<Self> {
        let file = dir.join(MACHINE_FILE);
        let data = match std::fs::read_to_string(&file) {
            Ok(text) => parse_object(&text).with_context(|| format!("reading {}", file.display()))?,
            Err(e) if e.kind() == ErrorKind::NotFound => Map::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
        };
        let seen = elsewhere(&data);
        Ok(MachineStore { file, data, seen })
    }

    /// Your own changes to the tags this computer finds for itself.
    pub fn tags(&self) -> Option<TagOverrides> {
        self.data.get("tags").and_then(TagOverrides::from_value)
    }

    /// Sets the tag overrides and writes the file (keeping other keys as they are).
    /// Used by setup and the settings page once those land; kept for their tests today.
    #[allow(dead_code)]
    pub fn set_tags(&mut self, tags: TagOverrides) -> Result<()> {
        self.data.insert("tags".to_string(), tags.to_value());
        self.save()
    }

    /// Network access to the settings page, and its password's hash.
    pub fn network(&self) -> Option<NetworkAccess> {
        self.data.get("network").and_then(NetworkAccess::from_value)
    }

    /// Sets network access and writes the file (keeping other keys as they are). Used by setup
    /// and the settings page once those land; kept for their tests today.
    #[allow(dead_code)]
    pub fn set_network(&mut self, network: NetworkAccess) -> Result<()> {
        self.data.insert("network".to_string(), network.to_value());
        self.save()
    }

    /// Test helper: change a key other than tags/network without going through their setters.
    #[cfg(test)]
    fn set_raw(&mut self, key: &str, value: Value) {
        self.data.insert(key.to_string(), value);
    }

    /// Writes the file atomically, compact. If setup wrote `tags` or `network` meanwhile and this
    /// process hasn't changed them since load, the file's values win.
    #[allow(dead_code)]
    pub fn save(&mut self) -> Result<()> {
        if let Ok(text) = std::fs::read_to_string(&self.file)
            && let Ok(disk) = parse_object(&text)
        {
            let disk_elsewhere = elsewhere(&disk);
            if disk_elsewhere != self.seen && elsewhere(&self.data) == self.seen {
                for key in SET_ELSEWHERE {
                    match disk.get(key) {
                        Some(v) => {
                            self.data.insert(key.to_string(), v.clone());
                        }
                        None => {
                            self.data.remove(key);
                        }
                    }
                }
            }
        }
        write_json_atomic(&self.file, &Value::Object(self.data.clone()), true)?;
        self.seen = elsewhere(&self.data);
        Ok(())
    }
}

fn parse_object(text: &str) -> Result<Map<String, Value>> {
    match serde_json::from_str(text)? {
        Value::Object(map) => Ok(map),
        _ => Ok(Map::new()),
    }
}

/// A stable fingerprint of the keys setup may write elsewhere, so a running save can tell when
/// the file moved under it.
fn elsewhere(data: &Map<String, Value>) -> String {
    let mut obj = Map::new();
    for key in SET_ELSEWHERE {
        if let Some(v) = data.get(key) {
            obj.insert(key.to_string(), v.clone());
        }
    }
    serde_json::to_string(&Value::Object(obj)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(dir: &Path) -> String {
        std::fs::read_to_string(dir.join(MACHINE_FILE)).unwrap()
    }

    #[test]
    fn a_missing_file_loads_as_empty_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = MachineStore::load(dir.path()).unwrap();
        assert!(store.tags().is_none());
        assert!(!dir.path().join(MACHINE_FILE).exists());
    }

    #[test]
    fn tags_round_trip_and_other_keys_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MACHINE_FILE),
            r#"{"usage":{"pct":10},"tags":{"added":["games"],"removed":["gpu"]}}"#,
        )
        .unwrap();
        let mut store = MachineStore::load(dir.path()).unwrap();
        let tags = store.tags().unwrap();
        assert_eq!(tags.added, vec!["games"]);
        assert_eq!(tags.removed, vec!["gpu"]);
        store
            .set_tags(TagOverrides {
                added: vec!["web".to_string()],
                removed: vec![],
            })
            .unwrap();
        let saved: Value = serde_json::from_str(&read(dir.path())).unwrap();
        assert_eq!(saved["usage"], serde_json::json!({ "pct": 10 }));
        assert_eq!(saved["tags"]["added"], serde_json::json!(["web"]));
        assert_eq!(saved["tags"]["removed"], serde_json::json!([]));
        assert!(!read(dir.path()).contains('\n'));
    }

    #[test]
    fn a_running_save_keeps_tags_setup_wrote_meanwhile() {
        // agent-settings.md section 5 / TypeScript MachineStore.save: setup may write tags while
        // the agent is running; the agent's next save must not overwrite them with its older copy.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MACHINE_FILE),
            r#"{"tags":{"added":["old"],"removed":[]}}"#,
        )
        .unwrap();
        let mut store = MachineStore::load(dir.path()).unwrap();
        std::fs::write(
            dir.path().join(MACHINE_FILE),
            r#"{"tags":{"added":["from-setup"],"removed":["gpu"]},"usage":1}"#,
        )
        .unwrap();
        // Touch a different key so save runs, without changing tags in memory.
        store.set_raw("usage", Value::from(2));
        store.save().unwrap();
        let saved: Value = serde_json::from_str(&read(dir.path())).unwrap();
        assert_eq!(saved["tags"]["added"], serde_json::json!(["from-setup"]));
        assert_eq!(saved["tags"]["removed"], serde_json::json!(["gpu"]));
        assert_eq!(saved["usage"], Value::from(2));
    }

    #[test]
    fn network_round_trips_and_a_running_save_keeps_what_setup_wrote_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MachineStore::load(dir.path()).unwrap();
        assert!(store.network().is_none());
        store
            .set_network(NetworkAccess {
                enabled: true,
                password_hash: Some("scrypt$16384$8$1$aa$bb".to_string()),
            })
            .unwrap();
        assert_eq!(
            store.network().unwrap(),
            NetworkAccess {
                enabled: true,
                password_hash: Some("scrypt$16384$8$1$aa$bb".to_string()),
            }
        );
        // Setup turns it off while the agent is running (keeping the hash); the agent's next save
        // of a different key must not overwrite that with its older, still-on copy.
        std::fs::write(
            dir.path().join(MACHINE_FILE),
            r#"{"network":{"enabled":false,"passwordHash":"scrypt$16384$8$1$aa$bb"},"tags":{}}"#,
        )
        .unwrap();
        store.set_raw("usage", Value::from(1));
        store.save().unwrap();
        let saved: Value = serde_json::from_str(&read(dir.path())).unwrap();
        assert_eq!(saved["network"]["enabled"], Value::Bool(false));
        assert_eq!(saved["usage"], Value::from(1));
    }

    #[test]
    fn a_broken_file_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MACHINE_FILE), "{").unwrap();
        assert!(format!("{:#}", MachineStore::load(dir.path()).unwrap_err()).contains(MACHINE_FILE));
    }
}
