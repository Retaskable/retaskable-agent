//! A `ci_finish` the store hasn't taken, kept in the CI runner's own data folder (`<data>/ci_finish/`, one small file
//! per record, so checks that end together never share a writer) so a restart sends it again (the owner's outage rule,
//! 2026-11-16: "it should be able to finish what it's doing but it can't pick up the next thing till the store is
//! back."). It is written before the first send and removed when the store answers.
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The folder in the runner's data folder.
const DIR: &str = "ci_finish";

/// What the store is owed: the repo and the `ci_finish` arguments, and when they were first kept (epoch seconds).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub repo: String,
    pub args: Value,
    pub since: i64,
}

impl Record {
    /// How long it has been owed.
    pub fn age(&self) -> Duration {
        Duration::from_secs(crate::owed_file::now_secs().saturating_sub(self.since).max(0) as u64)
    }
}

/// One record's file.
#[derive(Debug, Clone)]
pub struct Kept(PathBuf);

fn word(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

impl Kept {
    /// The file of the record for `repo` and `args` (its commit, context, event and pull request name it).
    pub fn of(data_dir: &Path, repo: &str, args: &Value) -> Self {
        let part = |k: &str| match &args[k] {
            Value::String(s) => word(s),
            Value::Null => String::new(),
            other => word(&other.to_string()),
        };
        let name = format!(
            "{}.{}.{}.{}.{}.json",
            word(repo),
            part("sha"),
            part("context"),
            part("event"),
            part("pr")
        );
        Self(data_dir.join(DIR).join(name))
    }

    /// Lists the record, keeping the time it was first kept, before it is sent.
    pub fn save(&self, repo: &str, args: &Value) -> Record {
        let since = self.read().map_or_else(crate::owed_file::now_secs, |r| r.since);
        let record = Record {
            repo: repo.into(),
            args: args.clone(),
            since,
        };
        let value = serde_json::to_value(&record).unwrap_or_default();
        if let Err(e) = crate::fs_util::write_json_atomic(&self.0, &value, false) {
            tracing::warn!("agent CI: saving the run record owed to the store failed: {e:#}");
        }
        record
    }

    /// The store answered: it is no longer owed.
    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.0);
    }

    fn read(&self) -> Option<Record> {
        serde_json::from_str(&std::fs::read_to_string(&self.0).ok()?).ok()
    }

    /// Every record owed in `data_dir`; a file that can't be read is removed.
    pub fn all(data_dir: &Path) -> Vec<(Kept, Record)> {
        let Ok(entries) = std::fs::read_dir(data_dir.join(DIR)) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let kept = Kept(path);
            match kept.read() {
                Some(record) => found.push((kept, record)),
                None => kept.remove(),
            }
        }
        found
    }
}
