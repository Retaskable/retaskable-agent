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
use crate::engines::EngineId;
use crate::fs_util::write_json_atomic;
use crate::trial_plan::TrialRecord;
use crate::vet::VetRecord;

const STATE_FILE: &str = "state.json";

/// Top-level keys an older agent kept locally; items and drafts live only in the shared backlog
/// store now (local-state-diet.md section 1), so these are dropped wherever found. There's no
/// time-based trimming left to do here (timelines and merged drafts' files are the store's), so,
/// unlike the TypeScript `trimState(data, now)`, this takes no `now`.
const LEGACY_KEYS: [&str; 2] = ["items", "drafts"];

/// One leased item's run, as the TS agent's item record keeps it (`state`, `lane`, `implSessionId`,
/// `branch`, `slice`): enough to take the item back after a restart.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// The engine that session started on (`implSessionEngine`), kept with its id: a session resumes only on
    /// the engine it started on. A record saved before this reads as Claude.
    #[serde(default)]
    pub impl_session_engine: EngineId,
    /// That session's size as of its last turn (06.2-implementer.md "A session that's grown starts
    /// fresh"; the TS agent's `implContextTokens`), kept so a resume across a restart can still tell
    /// a grown session from a small one. `None` until a session reports one, and cleared whenever
    /// `impl_session_id` is: a fresh or picking-up session starts with no reading of its own yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impl_context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default)]
    pub slice: u32,
    /// How many times a person started this slice over (Start over): the branch is `…-s<slice>-r<attempt>` once
    /// it is above 0. Counts the item's current slice only.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub attempt: u32,
    /// Why the item is at `needs_human` now (the current stop only: a new stop replaces it, and with it the
    /// finding a triager left), for the triage session (06.9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_human_reason: Option<String>,
    /// The state the item failed in, as `item_lifecycle::ItemState` snake case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_in: Option<String>,
    /// Claude already looked at this stop (06.9 bullet 16): a restart never looks again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub triaged: bool,
    /// Automatic decisions made as `(slice, n)`; counted across start-overs, and a mark from an earlier slice
    /// doesn't count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triages: Option<(u32, u32)>,
    /// Looks at this stop that failed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub failed_looks: u32,
    /// When a failed look may be tried again (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_look_at: Option<String>,
    /// Watchdog kills of this item's session in a row, kept so a restart doesn't reset it; cleared when a
    /// session ends with an answer.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub silence_kills: u32,
    /// Cursor usage-limit hits of this item's implementer in a row, kept so a restart doesn't reset it; cleared
    /// by any other session end.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cursor_limit_hits: u32,
    /// The item's session couldn't start and is waiting to be started fresh again: its saved session id never
    /// ran, so a restart must not resume it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub not_started: bool,
    /// Signatures of Retaskable bugs this item's triage already reported (`reported_bug`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reported_bugs: Vec<String>,
}

impl RunRecord {
    /// The run reached a new `needs_human` stop from `from` for `reason`: the stop's facts replace the old
    /// ones, and the look marks start clean. `triages` is kept for the same slice and reset for a new one.
    pub fn new_stop(&mut self, from: &str, reason: &str, slice: u32) {
        self.needs_human_reason = Some(reason.to_string());
        self.failed_in = Some(from.to_string());
        self.triaged = false;
        self.failed_looks = 0;
        self.next_look_at = None;
        self.triages = self.triages.filter(|(s, _)| *s == slice);
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A project's own on-disk state.
#[derive(Debug)]
pub struct Store {
    file: PathBuf,
    data: Map<String, Value>,
}

/// Counts every saved trial record, across projects, so the cloud link can look again at its own project's.
static TRIAL_CHANGED: std::sync::LazyLock<tokio::sync::watch::Sender<u64>> =
    std::sync::LazyLock::new(|| tokio::sync::watch::Sender::new(0));

/// Subscribes to saved trial records (`Store::set_trial`); the value only counts them.
pub fn subscribe_trial_changes() -> tokio::sync::watch::Receiver<u64> {
    TRIAL_CHANGED.subscribe()
}

/// Counts every save of a state that holds a smoke, across projects.
static SMOKE_SAVED: std::sync::LazyLock<tokio::sync::watch::Sender<u64>> =
    std::sync::LazyLock::new(|| tokio::sync::watch::Sender::new(0));

/// Subscribes to saves of a state holding a smoke (`set_smoke`, then `save`); the value only counts them.
pub fn subscribe_smoke_changes() -> tokio::sync::watch::Receiver<u64> {
    SMOKE_SAVED.subscribe()
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
        write_json_atomic(&self.file, &Value::Object(self.data.clone()), true)?;
        if self.data.contains_key("smoke") {
            SMOKE_SAVED.send_modify(|n| *n += 1);
        }
        Ok(())
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

    /// Replaces `state.weekUsage`, the last reading that carried the weekly window (kept apart from `usage`,
    /// which `implementer_tier` reads). Doesn't save on its own; call `save` after.
    pub fn set_week_usage(&mut self, usage: Value) {
        self.data.insert("weekUsage".to_string(), usage);
    }

    /// The last-saved `state.weekUsage`, if any.
    pub fn week_usage(&self) -> Option<Value> {
        self.data.get("weekUsage").cloned()
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

    /// When each `unreleased` entry was first listed, in epoch seconds. An entry with no time (a state file
    /// from before it was kept) counts as listed at `now`, so it gets a full day.
    pub fn unreleased_since(&self, now: i64) -> std::collections::BTreeMap<String, i64> {
        let saved: std::collections::BTreeMap<String, i64> = self
            .data
            .get("unreleased_since")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        self.unreleased()
            .into_keys()
            .map(|id| {
                let at = saved.get(&id).copied().unwrap_or(now);
                (id, at)
            })
            .collect()
    }

    /// Replaces `state.unreleased_since`. Doesn't save on its own; call `save` after.
    pub fn set_unreleased_since(&mut self, since: &std::collections::BTreeMap<String, i64>) {
        self.data
            .insert("unreleased_since".to_string(), serde_json::json!(since));
    }

    /// Items a person skipped here whose `skip` the store hasn't taken yet (sent after their `release`, again
    /// on later passes until it answers). Empty when missing.
    pub fn unskipped(&self) -> std::collections::BTreeSet<String> {
        self.data
            .get("unskipped")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Replaces `state.unskipped`. Doesn't save on its own; call `save` after.
    pub fn set_unskipped(&mut self, unskipped: &std::collections::BTreeSet<String>) {
        self.data.insert("unskipped".to_string(), serde_json::json!(unskipped));
    }

    /// A person's playtest answers for the store's try-out queue that the store hasn't taken yet: the id the
    /// command named to (passed, notes). Sent again on later passes. Empty when missing.
    pub fn unanswered(&self) -> std::collections::BTreeMap<String, (bool, Option<String>)> {
        self.data
            .get("unanswered")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Replaces `state.unanswered`. Doesn't save on its own; call `save` after.
    pub fn set_unanswered(&mut self, unanswered: &std::collections::BTreeMap<String, (bool, Option<String>)>) {
        self.data
            .insert("unanswered".to_string(), serde_json::json!(unanswered));
    }

    /// The `playtests` value (config.json's) this project last saw, as its config name; `None` before the first pass.
    pub fn playtests_seen(&self) -> Option<String> {
        self.data
            .get("playtestsSeen")
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    /// Whether a `releasePlaytests` is owed to the store (`playtests` switched to off, not yet taken).
    pub fn release_owed(&self) -> bool {
        self.data.get("releaseOwed").and_then(|v| v.as_bool()).unwrap_or(false)
    }

    /// Replaces `state.playtestsSeen` and `state.releaseOwed`. Doesn't save on its own; call `save` after.
    pub fn set_playtests_seen(&mut self, seen: Option<&str>, owed: bool) {
        self.data.insert("playtestsSeen".to_string(), serde_json::json!(seen));
        self.data.insert("releaseOwed".to_string(), serde_json::json!(owed));
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

    /// `state.ownFailures`: each item's own failures in a row (a session that wouldn't start, or a hand-back),
    /// kept past the item's lease since a hand-back drops its run record.
    pub fn own_failures(&self) -> std::collections::BTreeMap<String, u32> {
        self.data
            .get("ownFailures")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Replaces `state.ownFailures`. Doesn't save on its own; call `save` after.
    pub fn set_own_failures(&mut self, failures: &std::collections::BTreeMap<String, u32>) {
        self.data.insert("ownFailures".to_string(), serde_json::json!(failures));
    }

    /// `state.vet`: the daily vet pass's last run and recent verdicts; none before a run or when unreadable.
    pub fn vet(&self) -> Option<VetRecord> {
        self.data
            .get("vet")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Replaces `state.vet`. Doesn't save on its own; call `save` after.
    pub fn set_vet(&mut self, record: &VetRecord) {
        self.data.insert("vet".to_string(), serde_json::json!(record));
    }

    /// `state.trialPlan`; not started when missing or unreadable.
    pub fn trial(&self) -> TrialRecord {
        self.data
            .get("trialPlan")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Replaces `state.trialPlan` and saves at once, so a restart sees it before the session starts.
    pub fn set_trial(&mut self, record: TrialRecord) -> Result<()> {
        self.data.insert("trialPlan".to_string(), serde_json::json!(record));
        self.save()?;
        TRIAL_CHANGED.send_modify(|n| *n += 1);
        Ok(())
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
    fn a_run_keeps_its_session_engine_and_an_old_one_reads_as_claude() {
        let record = |json: &str| serde_json::from_str::<RunRecord>(json).unwrap();
        let cursor = record(r#"{"state":"implementing","implSessionId":"s","implSessionEngine":"cursor"}"#);
        assert_eq!(cursor.impl_session_engine, EngineId::Cursor);
        assert_eq!(record(&serde_json::to_string(&cursor).unwrap()), cursor);
        let old = record(r#"{"state":"implementing","implSessionId":"s"}"#);
        assert_eq!(old.impl_session_engine, EngineId::Claude);
    }

    #[test]
    fn the_trial_reads_as_not_started_until_set_beside_other_keys() {
        use crate::trial_plan::TrialState;
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Store::load(dir.path()).unwrap().trial(), TrialRecord::default());
        std::fs::write(dir.path().join(STATE_FILE), r#"{"other":{"a":1}}"#).unwrap();
        let mut store = Store::load(dir.path()).unwrap();
        assert_eq!(store.trial().state, TrialState::NotStarted);
        store.set_trial(TrialRecord::opened("https://x/pr/1")).unwrap();
        let again = Store::load(dir.path()).unwrap();
        assert_eq!(again.trial(), TrialRecord::opened("https://x/pr/1"));
        assert!(read(dir.path()).contains(r#""other":{"a":1}"#));
        assert!(read(dir.path()).contains("\"trialPlan\""));
    }

    #[test]
    fn an_old_unreleased_entry_without_a_time_reads_as_listed_now_and_a_saved_time_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(STATE_FILE),
            r#"{"unreleased":{"a":["t","note"],"b":["u","n"]}}"#,
        )
        .unwrap();
        let mut store = Store::load(dir.path()).unwrap();
        assert_eq!(
            store.unreleased_since(500),
            [("a".to_string(), 500), ("b".to_string(), 500)].into()
        );
        store.set_unreleased_since(&[("a".to_string(), 100)].into());
        store.save().unwrap();
        let again = Store::load(dir.path()).unwrap();
        let since = again.unreleased_since(500);
        assert_eq!((since["a"], since["b"]), (100, 500));
    }

    #[test]
    fn the_vet_record_round_trips_beside_other_keys() {
        use crate::vet::{PastVerdict, VetRecord};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STATE_FILE), r#"{"other":1}"#).unwrap();
        let mut store = Store::load(dir.path()).unwrap();
        assert_eq!(store.vet(), None);
        let record = VetRecord {
            day: "2026-11-03".into(),
            counts: [("spam".to_string(), 35)].into(),
            recent: (0..35)
                .map(|i| PastVerdict {
                    verdict: crate::backlog::suggestions::Verdict::Spam,
                    reason: format!("r{i}"),
                })
                .collect(),
        };
        store.set_vet(&record);
        store.save().unwrap();
        let again = Store::load(dir.path()).unwrap().vet().unwrap();
        assert_eq!(again, record);
        assert!(read(dir.path()).contains(r#""other":1"#));
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
    fn week_usage_round_trips_and_leaves_usage_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::load(dir.path()).unwrap();
        assert_eq!(store.week_usage(), None);
        let week = serde_json::json!({"rateLimitType": "seven_day", "utilization": 0.96});
        store.set_week_usage(week.clone());
        store.save().unwrap();
        let store = Store::load(dir.path()).unwrap();
        assert_eq!(store.week_usage(), Some(week));
        assert_eq!(store.usage(), None);
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
