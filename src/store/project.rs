//! Each project's own `.retaskable/projects/<id>/state.json`, following
//! docs/specs/local-state-diet.md and docs/specs/agent/04-architecture.md ("Data model: items and
//! drafts"). Items and drafts live only in the shared backlog store, never here, so this agent
//! writes none of its own; an older agent's file may still have them, and they're dropped the
//! first time this agent loads it. Nothing else of the project's own is defined yet (later pieces
//! land here one at a time), so what's left of the file round-trips as it is.
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use super::SmokeState;
use crate::fs_util::write_json_atomic;

const STATE_FILE: &str = "state.json";

/// Top-level keys an older agent kept locally; items and drafts live only in the shared backlog
/// store now (local-state-diet.md section 1), so these are dropped wherever found. There's no
/// time-based trimming left to do here (timelines and merged drafts' files are the store's), so,
/// unlike the TypeScript `trimState(data, now)`, this takes no `now`.
const LEGACY_KEYS: [&str; 2] = ["items", "drafts"];

/// One leased item's run, as the TS agent's item record keeps it (`state`, `lane`, `implSessionId`,
/// `branch`, `slice`): enough to take the item back after a restart.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    /// `implementing`, `checking`, `needs_human` or `queued` (`item_lifecycle::ItemState`, snake case).
    pub state: String,
    /// The lane it holds; none once it needs a human.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<u32>,
    /// The implementer session's id, to resume it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impl_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default)]
    pub slice: u32,
}

/// A project's own on-disk state.
#[derive(Debug)]
pub struct Store {
    file: PathBuf,
    data: Map<String, Value>,
}

impl Store {
    /// Loads `<dir>/state.json` (empty when there's none yet), trims any legacy item and draft
    /// records (local-state-diet.md section 3), and saves at once if that dropped anything,
    /// logging the size change. Safe to call on every load: a file with nothing to trim comes
    /// back unchanged, and no file is written when there was none to begin with.
    pub fn load(dir: &Path) -> Result<Self> {
        let file = dir.join(STATE_FILE);
        let (data, before_bytes) = match std::fs::read_to_string(&file) {
            Ok(text) => {
                let data = parse_object(&text).with_context(|| format!("reading {}", file.display()))?;
                (data, text.len())
            }
            Err(e) if e.kind() == ErrorKind::NotFound => (Map::new(), 0),
            Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
        };
        let (data, changed) = trim_state(data);
        let store = Store { file, data };
        if changed {
            store.save()?;
            let after_bytes = std::fs::metadata(&store.file).map(|m| m.len()).unwrap_or(0);
            tracing::info!(
                "state.json trimmed from {:.1} MB to {:.1} MB ({})",
                before_bytes as f64 / 1_000_000.0,
                after_bytes as f64 / 1_000_000.0,
                store.file.display(),
            );
        }
        Ok(store)
    }

    /// Writes the state file atomically (a temp file, then a rename), compact: no indentation,
    /// since nothing reads it by line (local-state-diet.md section 3).
    pub fn save(&self) -> Result<()> {
        write_json_atomic(&self.file, &Value::Object(self.data.clone()), true)
    }

    /// Replaces `state.usage` with a session's latest 5-hour usage reading
    /// (docs/specs/agent/07-orchestrator-loop.md section 7.1: "The latest one is kept in
    /// `state.usage`"). Doesn't save on its own; call `save` after, as every other change here
    /// does.
    pub fn set_usage(&mut self, usage: Value) {
        self.data.insert("usage".to_string(), usage);
    }

    /// The last-saved `state.usage`, if any (`None` when there's no reading yet, which
    /// [`crate::tiers::implementer_tier`] treats as a full window).
    pub fn usage(&self) -> Option<Value> {
        self.data.get("usage").cloned()
    }

    /// Replaces `state.smoke` with the post-merge smoke's latest result
    /// (docs/specs/agent/06.3-review.md: "`State.smoke = {sha, status, at, url, tail?}`").
    /// Doesn't save on its own; call `save` after, as every other change here does.
    pub fn set_smoke(&mut self, smoke: &SmokeState) {
        self.data.insert(
            "smoke".to_string(),
            serde_json::to_value(smoke).expect("SmokeState always serializes"),
        );
    }

    /// The lease token of each item this computer holds in the shared backlog, by item id
    /// (`state.leases`; the TS agent keeps it on the item record as `hosted.lease`). Empty when
    /// missing or unreadable.
    pub fn leases(&self) -> std::collections::BTreeMap<String, String> {
        self.data
            .get("leases")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Replaces `state.leases`. Doesn't save on its own; call `save` after.
    pub fn set_leases(&mut self, leases: &std::collections::BTreeMap<String, String>) {
        self.data.insert("leases".to_string(), serde_json::json!(leases));
    }

    /// Leases given back here that the store hasn't taken yet, by item id: its lease token and the
    /// give-back's note (agent/07.5-what-a-claim-and-a-give-back-send.md's `report`, sent again with
    /// `release` on later passes). Empty when missing.
    pub fn unreleased(&self) -> std::collections::BTreeMap<String, (String, String)> {
        self.data
            .get("unreleased")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Replaces `state.unreleased`. Doesn't save on its own; call `save` after.
    pub fn set_unreleased(&mut self, unreleased: &std::collections::BTreeMap<String, (String, String)>) {
        self.data
            .insert("unreleased".to_string(), serde_json::json!(unreleased));
    }

    /// Leases let go while their item waits, which the store hasn't taken yet: item id to
    /// (lease token, what it waits for, the item's state). Sent again on later passes.
    pub fn unparked(&self) -> std::collections::BTreeMap<String, (String, String, String)> {
        self.data
            .get("unparked")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Replaces `state.unparked`. Doesn't save on its own; call `save` after.
    pub fn set_unparked(&mut self, unparked: &std::collections::BTreeMap<String, (String, String, String)>) {
        self.data.insert("unparked".to_string(), serde_json::json!(unparked));
    }

    /// What each item in `leases` is doing, by item id (`state.runs`), so a restart can carry on: the
    /// TS agent keeps these on its item record. Empty when missing or unreadable.
    pub fn runs(&self) -> std::collections::BTreeMap<String, RunRecord> {
        self.data
            .get("runs")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Replaces `state.runs`. Doesn't save on its own; call `save` after.
    pub fn set_runs(&mut self, runs: &std::collections::BTreeMap<String, RunRecord>) {
        self.data.insert("runs".to_string(), serde_json::json!(runs));
    }

    /// The last-saved `state.smoke`, if any, or `None` when it's missing or no longer parses
    /// (an older agent's file, say) rather than erroring.
    pub fn smoke(&self) -> Option<SmokeState> {
        self.data
            .get("smoke")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}

fn parse_object(text: &str) -> Result<Map<String, Value>> {
    match serde_json::from_str(text)? {
        Value::Object(map) => Ok(map),
        _ => Ok(Map::new()),
    }
}

/// Drops any of `LEGACY_KEYS` present, answering whether it changed anything.
fn trim_state(mut data: Map<String, Value>) -> (Map<String, Value>, bool) {
    let mut changed = false;
    for key in LEGACY_KEYS {
        if data.remove(key).is_some() {
            changed = true;
        }
    }
    (data, changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(dir: &Path) -> String {
        std::fs::read_to_string(dir.join(STATE_FILE)).unwrap()
    }

    #[test]
    fn a_missing_file_loads_as_empty_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        Store::load(dir.path()).unwrap();
        assert!(!dir.path().join(STATE_FILE).exists());
    }

    #[test]
    fn old_item_and_draft_records_are_dropped_but_the_rest_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(STATE_FILE),
            r#"{"items": [{"id": "a"}], "drafts": [{"id": "b"}], "usage": {"pct": 10}}"#,
        )
        .unwrap();
        Store::load(dir.path()).unwrap();
        let saved: Value = serde_json::from_str(&read(dir.path())).unwrap();
        assert_eq!(saved, serde_json::json!({ "usage": { "pct": 10 } }));
    }

    #[test]
    fn the_saved_file_is_compact_with_no_leading_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STATE_FILE), r#"{"items": [1, 2, 3]}"#).unwrap();
        Store::load(dir.path()).unwrap();
        let text = read(dir.path());
        assert!(
            text.starts_with('{'),
            "expected no newline or space before the first key: {text:?}"
        );
        assert!(!text.contains('\n'));
    }

    #[test]
    fn loading_an_already_trimmed_file_again_leaves_it_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STATE_FILE), r#"{"items": [1], "kept": true}"#).unwrap();
        Store::load(dir.path()).unwrap();
        let once = read(dir.path());
        Store::load(dir.path()).unwrap();
        let twice = read(dir.path());
        assert_eq!(once, twice);
        assert_eq!(once, r#"{"kept":true}"#);
    }

    #[test]
    fn a_file_with_neither_legacy_key_is_left_untouched_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let original = r#"{"kept": true}"#;
        std::fs::write(dir.path().join(STATE_FILE), original).unwrap();
        Store::load(dir.path()).unwrap();
        // Not re-saved (compacted or otherwise): trimming found nothing to change.
        assert_eq!(read(dir.path()), original);
    }

    #[test]
    fn usage_reads_back_the_last_saved_value_and_is_none_before_any_is_set() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::load(dir.path()).unwrap();
        assert_eq!(store.usage(), None);
        store.set_usage(serde_json::json!({"status": "allowed", "resetsAt": 1000}));
        assert_eq!(
            store.usage(),
            Some(serde_json::json!({"status": "allowed", "resetsAt": 1000}))
        );
    }

    #[test]
    fn set_usage_replaces_the_usage_key_once_saved() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::load(dir.path()).unwrap();
        store.set_usage(serde_json::json!({"status": "allowed", "resetsAt": 1000}));
        store.save().unwrap();
        let saved: Value = serde_json::from_str(&read(dir.path())).unwrap();
        assert_eq!(
            saved,
            serde_json::json!({"usage": {"status": "allowed", "resetsAt": 1000}})
        );

        store.set_usage(serde_json::json!({"status": "rejected", "resetsAt": 2000}));
        store.save().unwrap();
        let saved: Value = serde_json::from_str(&read(dir.path())).unwrap();
        assert_eq!(
            saved,
            serde_json::json!({"usage": {"status": "rejected", "resetsAt": 2000}})
        );
    }

    #[test]
    fn set_smoke_replaces_the_smoke_key_once_saved_and_reads_back_the_same_state() {
        use super::super::SmokeStatus;

        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::load(dir.path()).unwrap();
        assert!(store.smoke().is_none());

        let state = SmokeState {
            sha: "abc123".to_string(),
            status: SmokeStatus::Running,
            at: "2026-10-28T00:00:00.000Z".to_string(),
            url: Some("https://staging.example.com".to_string()),
            tail: None,
        };
        store.set_smoke(&state);
        store.save().unwrap();

        let reloaded = Store::load(dir.path()).unwrap();
        assert_eq!(reloaded.smoke(), Some(state));
    }

    #[test]
    fn a_broken_file_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STATE_FILE), "{").unwrap();
        assert!(format!("{:#}", Store::load(dir.path()).unwrap_err()).contains(STATE_FILE));
    }
}
