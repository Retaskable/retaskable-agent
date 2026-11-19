//! A small file of results owed to the store, by key, for a part that sends them again after a restart (the owner's
//! outage rule, 2026-11-16: "it should be able to finish what it's doing but it can't pick up the next thing till the
//! store is back."): an entry is put before it is first sent and removed when the store answers. Each part keeps a file
//! of its own, so no two writers share one. Neutral, so any of the three programs may use it.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// How long a result is kept and sent again before it is given up on: by then the store has long since moved on.
pub const KEPT_SECS: i64 = 24 * 60 * 60;

/// Epoch seconds.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The entries owed and the file that holds them. Clones share them.
pub struct OwedFile<T> {
    file: Arc<PathBuf>,
    owed: Arc<Mutex<BTreeMap<String, T>>>,
}

impl<T> Clone for OwedFile<T> {
    fn clone(&self) -> Self {
        Self {
            file: self.file.clone(),
            owed: self.owed.clone(),
        }
    }
}

impl<T: Serialize + DeserializeOwned + Clone + PartialEq> OwedFile<T> {
    /// Reads `file`; a missing or unreadable one is as good as none.
    pub fn open(file: PathBuf) -> Self {
        let owed = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self {
            file: Arc::new(file),
            owed: Arc::new(Mutex::new(owed)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, T>> {
        crate::sync::lock(&self.owed)
    }

    /// Everything owed now.
    pub fn all(&self) -> BTreeMap<String, T> {
        self.lock().clone()
    }

    pub fn get(&self, key: &str) -> Option<T> {
        self.lock().get(key).cloned()
    }

    /// Lists an entry and saves the file when that changed it, before anything is sent.
    pub fn put(&self, key: &str, entry: T) {
        let mut owed = self.lock();
        if owed.get(key) != Some(&entry) {
            owed.insert(key.into(), entry);
            self.save(&owed);
        }
    }

    /// The store answered: the entry is no longer owed.
    pub fn remove(&self, key: &str) {
        let mut owed = self.lock();
        if owed.remove(key).is_some() {
            self.save(&owed);
        }
    }

    fn save(&self, owed: &BTreeMap<String, T>) {
        let value = serde_json::to_value(owed).unwrap_or_default();
        if let Err(e) = crate::fs_util::write_json_atomic(&self.file, &value, false) {
            tracing::warn!("saving {} failed: {e:#}", self.file.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("owed.json")
    }

    #[test]
    fn missing_or_garbled_file_is_as_good_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(OwedFile::<String>::open(file(&dir)).all().is_empty());
        std::fs::write(file(&dir), "not json").unwrap();
        assert!(OwedFile::<String>::open(file(&dir)).all().is_empty());
    }

    #[test]
    fn put_is_saved_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let owed = OwedFile::<String>::open(file(&dir));
        owed.put("a", "one".into());
        owed.put("b", "two".into());
        let again = OwedFile::<String>::open(file(&dir));
        assert_eq!(again.get("a").as_deref(), Some("one"));
        assert_eq!(again.all().len(), 2);
    }

    #[test]
    fn put_replaces_an_entry_and_remove_drops_it_from_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let owed = OwedFile::<String>::open(file(&dir));
        owed.put("a", "one".into());
        owed.put("a", "two".into());
        assert_eq!(OwedFile::<String>::open(file(&dir)).get("a").as_deref(), Some("two"));
        owed.remove("a");
        owed.remove("missing");
        assert!(owed.get("a").is_none());
        assert!(OwedFile::<String>::open(file(&dir)).all().is_empty());
    }

    #[test]
    fn clones_share_entries() {
        let dir = tempfile::tempdir().unwrap();
        let owed = OwedFile::<String>::open(file(&dir));
        let other = owed.clone();
        owed.put("a", "one".into());
        assert_eq!(other.get("a").as_deref(), Some("one"));
        other.remove("a");
        assert!(owed.get("a").is_none());
    }

    #[test]
    fn now_secs_is_after_2020_and_kept_for_a_day() {
        assert!(now_secs() > 1_577_836_800);
        assert_eq!(KEPT_SECS, 86_400);
    }
}
