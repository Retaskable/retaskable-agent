//! What a computer writes down for the site to read (docs/specs/intent-rows.md section 5.2; `keptFacts.ts`, and
//! `storeLastRelease` and `storeMainPrDraft` in the TS agent): a draft's files (`draft_files`), the last answer of the
//! Environments read (`environments`), the last release (`release_last`) and what Open PR to main would send
//! (`main_pr_draft`). Each is a `fact_set` of the project's shared backlog, so the site shows it with no computer asked.
//!
//! The TS agent holds each fact in memory beside its state and sends a change on the next save, keeping it and trying
//! again while the store can't be reached. Here the writers call `keep_*`, which lists the value (in a file of the
//! project's folder once [`KeptFacts::attach`] says where, so a restart still owes it), and the loop's [`KeptFacts::flush`]
//! sends what is listed after each fetch. Only the store's word ends a debt: unreachable, a 5xx or an unreadable answer
//! keep it; a refusal that names a code (the value is over its cap or out of shape) drops it, as the TS agent's
//! `refused` set does. Nothing is written on a guess: with no link there is no send (fail closed).
//!
//! Divergences from the TS agent (improvements while porting):
//! - `draft_files` is the row of every computer's drafts, so a write reads the row first and sends it back over that
//!   version (`ifUpdatedAt`), merging this computer's drafts into it. TS keeps the whole row in memory and replaces it,
//!   which can drop another computer's drafts' files written since it last read.
//! - `release_last` and `main_pr_draft` are compared with the row the store has (read once, [`KeptFacts::seed`]), so a
//!   restart neither writes an unchanged last release again nor leaves an old "ahead" draft standing.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::backlog::{BacklogError, Client};

pub mod last;

pub const DRAFT_FILES: &str = "draft_files";
pub const ENVIRONMENTS: &str = "environments";
pub const RELEASE_LAST: &str = "release_last";
pub const MAIN_PR_DRAFT: &str = "main_pr_draft";
/// The results this computer sees of the base branch (`StoreFacts` kinds in the TypeScript agent, spec 03.16): the checks
/// run on its commit, the code host's CI on it and whether CI runs for changes into it.
pub const BASELINE: &str = "baseline";
pub const MAIN_CI: &str = "main_ci";
pub const CI_COVERAGE: &str = "ci_coverage";
/// What the last check of the project's issue tracker found (`issue_import`, `{ error?, at }`): the page of another
/// computer shows why the tracker could not be read.
pub const ISSUE_IMPORT: &str = "issue_import";

/// `FACT_MAX_BYTES` (shared/storeFacts.ts): the most a kind may be as UTF-8 JSON; more is refused, not cut.
const DRAFT_FILES_MAX: usize = 512 * 1024;
const ENVIRONMENTS_MAX: usize = 64 * 1024;
/// The caps of the result kinds (`FACT_MAX_BYTES`).
const BASELINE_MAX: usize = 32 * 1024;
const RESULT_MAX: usize = 8 * 1024;
const ISSUE_IMPORT_MAX: usize = 2 * 1024;
/// `DRAFT_FILES_MAX_BYTES`: one draft's files in `draft_files`; a bigger change is read from a computer.
pub const ONE_DRAFT_MAX: usize = 256 * 1024;
/// Room left under a cap, as the TS agent leaves it.
const MARGIN: usize = 1024;
/// How many times a `draft_files` write that lost a race is read and sent again in one flush.
const RACES: usize = 3;
/// The prefix of a draft's entry among the facts owed.
const DRAFT: &str = "draft_files/";

/// The size the store counts: UTF-8 bytes of the JSON.
pub fn size(v: &Value) -> usize {
    v.to_string().len()
}

/// A fact the store is owed. `seq` orders the drafts written, so the oldest go first when the row is over its cap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Owed {
    value: Value,
    seq: u64,
}

#[derive(Default)]
struct Inner {
    /// What the store is owed, by `environments`, `release_last`, `main_pr_draft` or `draft_files/<id>` (a `null`
    /// value there takes the draft out of the row).
    owed: BTreeMap<String, Owed>,
    seq: u64,
    file: Option<PathBuf>,
    /// What the store has (or is owed) for `release_last` and `main_pr_draft`, once read.
    held: BTreeMap<&'static str, Value>,
    seeded: bool,
    /// When the last release was last looked up.
    release_read: Option<std::time::Instant>,
}

/// The facts one project keeps for the site. Clones share them.
#[derive(Clone, Default)]
pub struct KeptFacts(Arc<Mutex<Inner>>);

/// The store's two facts calls for one project and this computer. Faked in tests.
pub trait FactStore: Send + Sync {
    /// The row of `kind`: its data and `updatedAt`, or none when there is no row.
    fn get(&self, kind: &'static str) -> BoxFuture<'_, Result<Option<(Value, u64)>, BacklogError>>;
    /// Replaces the row; with `if_updated_at` only over that version (0 for none yet), else `conflict`.
    fn set(
        &self,
        kind: &'static str,
        data: Value,
        if_updated_at: Option<u64>,
    ) -> BoxFuture<'_, Result<(), BacklogError>>;
}

/// The real store.
pub struct StoreFacts {
    pub client: Arc<Client>,
    pub repo: u64,
    pub token: String,
    /// This computer's name, which the store notes on what it wrote.
    pub machine: String,
}

impl FactStore for StoreFacts {
    fn get(&self, kind: &'static str) -> BoxFuture<'_, Result<Option<(Value, u64)>, BacklogError>> {
        async move {
            let mut rows = self.client.facts_get(self.repo, Some(&self.token), &[kind]).await?;
            Ok(rows.remove(kind).map(|f| (f.data, f.updated_at)))
        }
        .boxed()
    }

    fn set(
        &self,
        kind: &'static str,
        data: Value,
        if_updated_at: Option<u64>,
    ) -> BoxFuture<'_, Result<(), BacklogError>> {
        async move {
            self.client
                .fact_set(self.repo, Some(&self.token), kind, &data, &self.machine, if_updated_at)
                .await
        }
        .boxed()
    }
}

impl std::fmt::Debug for KeptFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KeptFacts({} owed)", self.lock().owed.len())
    }
}

/// The facts of a project, by the project's own folder (where it keeps its state), shared by the loop, the read rows
/// and the drafter of that project.
pub fn shared(project_dir: &Path) -> KeptFacts {
    static ALL: OnceLock<Mutex<BTreeMap<String, KeptFacts>>> = OnceLock::new();
    let mut all = ALL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    all.entry(project_dir.to_string_lossy().into_owned())
        .or_default()
        .clone()
}

impl KeptFacts {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Keeps what is owed in `kept_facts.pending.json` in the project's folder from now on, and takes up what an earlier
    /// run left there. Done once; a later call changes nothing.
    pub fn attach(&self, dir: &Path) {
        let mut me = self.lock();
        if me.file.is_some() {
            return;
        }
        let file = dir.join("kept_facts.pending.json");
        let earlier: BTreeMap<String, Owed> = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        for (key, owed) in earlier {
            me.seq = me.seq.max(owed.seq);
            // What was listed since this start is newer than what the file kept.
            me.owed.entry(key).or_insert(owed);
        }
        me.file = Some(file);
        me.save();
    }

    /// Lists a draft's files for the site (`keepDraftFiles`). Over [`ONE_DRAFT_MAX`] they are not kept, and an older copy
    /// goes from the row. `[]` is what a merged draft keeps.
    pub fn keep_draft_files(&self, id: &str, files: &impl Serialize) {
        let value = serde_json::to_value(files).unwrap_or(Value::Null);
        let value = if value.is_array() && size(&value) <= ONE_DRAFT_MAX {
            value
        } else {
            Value::Null
        };
        self.list(&format!("{DRAFT}{id}"), value);
    }

    /// Lists a draft's changed files, as the site's `DraftFile`s (`keepDraftFiles` of what `changedFiles` listed): each
    /// gets `isNew`, a file the draft added, which is one listed in full with no old path, no patch and not deleted
    /// (the listing doesn't say it outright: a binary file that was edited is listed in full too, and reads as new).
    pub fn keep_drafted_files(&self, id: &str, files: &[crate::drafts::files::DraftFile]) {
        let listed: Vec<Value> = files
            .iter()
            .map(|f| {
                let mut v = serde_json::to_value(f).unwrap_or(Value::Null);
                v["isNew"] = json!(f.old_path.is_none() && f.patch.is_none() && !f.deleted);
                v
            })
            .collect();
        self.keep_draft_files(id, &listed);
    }

    /// Lists the Environments read's answer with its age, so the panel shows it at once (`keepEnvironments`); a view
    /// that is too big isn't kept.
    pub fn keep_environments(&self, view: &Value) {
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let kept = json!({ "at": at, "view": view });
        if size(&kept) <= ENVIRONMENTS_MAX - MARGIN {
            self.list(ENVIRONMENTS, kept);
        }
    }

    /// Lists a result of the base branch (`baseline`, `main_ci` or `ci_coverage`): the newest value of each kind is the one
    /// kept, written when the store can be reached. A value over its kind's cap is not kept (the store would refuse it).
    pub fn keep_result(&self, kind: &'static str, value: &Value) {
        let cap = match kind {
            BASELINE => BASELINE_MAX,
            MAIN_CI | CI_COVERAGE => RESULT_MAX,
            ISSUE_IMPORT => ISSUE_IMPORT_MAX,
            _ => return,
        };
        if size(value) <= cap - MARGIN {
            self.list(kind, value.clone());
        }
    }

    /// Lists how the last check of the issue tracker ended (`keepIssueImport`): `{ error, at }` (the error cut to fit the margin under the store's 2 KiB), or `{ at }` once a check
    /// read it again, so a row that held an error is cleared for the other computers.
    pub fn keep_issue_import(&self, error: Option<&str>) {
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut v = json!({ "at": at });
        if let Some(e) = error {
            v["error"] = json!(e.chars().take(900).collect::<String>());
        }
        self.keep_result(ISSUE_IMPORT, &v);
    }

    fn list(&self, key: &str, value: Value) {
        let mut me = self.lock();
        me.seq += 1;
        let seq = me.seq;
        me.owed.insert(key.to_string(), Owed { value, seq });
        me.save();
    }

    /// Whether the store's `release_last` and `main_pr_draft` have been read, so a change can be told from what it holds.
    pub fn seeded(&self) -> bool {
        self.lock().seeded
    }

    /// Reads the store's `release_last` and `main_pr_draft` once (what a TS computer reads at its start). A failure is
    /// only logged; [`seeded`](Self::seeded) stays false and the next pass tries again.
    pub async fn seed(&self, store: &dyn FactStore) {
        if self.seeded() {
            return;
        }
        for kind in [RELEASE_LAST, MAIN_PR_DRAFT] {
            match store.get(kind).await {
                Ok(row) => {
                    let mut me = self.lock();
                    if let Some((data, _)) = row {
                        me.held.entry(kind).or_insert(data);
                    }
                }
                Err(e) => {
                    tracing::warn!("facts: couldn't read {kind} from the shared backlog: {e}");
                    return;
                }
            }
        }
        self.lock().seeded = true;
    }

    /// Whether the last release is due to be looked up again (at most every `every`), and takes the turn if so.
    pub fn release_due(&self, every: std::time::Duration) -> bool {
        let mut me = self.lock();
        if me.release_read.is_some_and(|at| at.elapsed() < every) {
            return false;
        }
        me.release_read = Some(std::time::Instant::now());
        true
    }

    /// Lists the last release (`{ last }`, `null` for none) when it is not what the store has.
    pub fn keep_release_last(&self, last: Option<&last::Last>) {
        let value = json!({ "last": last });
        if self.lock().held.get(RELEASE_LAST) == Some(&value) {
            return;
        }
        self.lock().held.insert(RELEASE_LAST, value.clone());
        self.list(RELEASE_LAST, value);
    }

    /// The `ahead` count and base head the stored `main_pr_draft` was made for, if there is one.
    pub fn held_main_pr(&self) -> Option<(u64, Option<String>)> {
        let me = self.lock();
        let v = me.held.get(MAIN_PR_DRAFT)?;
        Some((
            v.get("ahead")?.as_u64()?,
            v.get("sha").and_then(Value::as_str).map(str::to_string),
        ))
    }

    /// Lists what Open PR to main would send. With none ahead an empty draft (`ahead: 0`) goes, since a fact left unset
    /// is never sent and the store would keep the old one (`storeMainPrDraft`); `None` there when none is held or it is
    /// empty already.
    pub fn keep_main_pr_draft(&self, draft: Option<(&str, &str, u64, Option<&str>)>) {
        let value = match draft {
            Some((title, body, ahead, sha)) => {
                let mut v = json!({ "title": title, "body": body, "ahead": ahead });
                if let Some(sha) = sha.filter(|s| !s.is_empty()) {
                    v["sha"] = json!(sha);
                }
                v
            }
            None => json!({ "title": "", "body": "", "ahead": 0 }),
        };
        {
            let mut me = self.lock();
            let empty = |v: &Value| v.get("ahead").and_then(Value::as_u64) == Some(0);
            let same = match me.held.get(MAIN_PR_DRAFT) {
                Some(have) => *have == value || (draft.is_none() && empty(have)),
                None => draft.is_none(),
            };
            if same {
                return;
            }
            me.held.insert(MAIN_PR_DRAFT, value.clone());
        }
        self.list(MAIN_PR_DRAFT, value);
    }

    /// What is owed under `key` now.
    pub fn owed(&self, key: &str) -> Option<Value> {
        self.lock().owed.get(key).map(|o| o.value.clone())
    }

    /// What is owed now, by key.
    pub fn owing(&self) -> Vec<String> {
        self.lock().owed.keys().cloned().collect()
    }

    /// Sends what is owed. Stops at the first answer that isn't the store's (it can't be reached): the rest waits.
    pub async fn flush(&self, store: &dyn FactStore) {
        let owed = self.lock().owed.clone();
        for kind in [
            ENVIRONMENTS,
            RELEASE_LAST,
            MAIN_PR_DRAFT,
            BASELINE,
            MAIN_CI,
            CI_COVERAGE,
            ISSUE_IMPORT,
        ] {
            let Some(entry) = owed.get(kind) else { continue };
            let sent = store.set(kind, entry.value.clone(), None).await;
            if !self.settled(kind, &[kind], &[entry], sent) {
                return;
            }
        }
        let drafts: Vec<(&String, &Owed)> = owed.iter().filter(|(k, _)| k.starts_with(DRAFT)).collect();
        if drafts.is_empty() {
            return;
        }
        let keys: Vec<&str> = drafts.iter().map(|(k, _)| k.as_str()).collect();
        let entries: Vec<&Owed> = drafts.iter().map(|(_, o)| *o).collect();
        let sent = self.send_drafts(store, &drafts).await;
        self.settled(DRAFT_FILES, &keys, &entries, sent);
    }

    /// Reads the `draft_files` row, merges this computer's drafts into it and sends it over that version.
    async fn send_drafts(&self, store: &dyn FactStore, drafts: &[(&String, &Owed)]) -> Result<(), BacklogError> {
        let mut tries = 0;
        loop {
            let row = store.get(DRAFT_FILES).await?;
            let (mut map, version) = match row {
                Some((Value::Object(m), v)) => (m, v),
                Some((_, v)) => (Default::default(), v),
                None => (Default::default(), 0),
            };
            let before = Value::Object(map.clone());
            let mut mine: Vec<(&str, &Owed)> = drafts.iter().map(|(k, o)| (&k[DRAFT.len()..], *o)).collect();
            mine.sort_by_key(|(_, o)| o.seq);
            for (id, o) in &mine {
                map.remove(*id);
                if !o.value.is_null() {
                    map.insert((*id).to_string(), o.value.clone());
                }
            }
            // Over the fact's size, the oldest written go first: other computers' drafts (their order isn't known), then
            // this one's in the order written, never the one written last.
            let newest = mine.last().map(|(id, _)| *id);
            let mut order: Vec<String> = map
                .keys()
                .filter(|k| !mine.iter().any(|(id, _)| id == k))
                .cloned()
                .collect();
            order.extend(
                mine.iter()
                    .map(|(id, _)| id.to_string())
                    .filter(|id| map.contains_key(id)),
            );
            order.retain(|id| Some(id.as_str()) != newest);
            let mut order = order.into_iter();
            while size(&Value::Object(map.clone())) > DRAFT_FILES_MAX - MARGIN {
                let Some(victim) = order.next() else { break };
                map.remove(&victim);
            }
            let after = Value::Object(map);
            if (after == before && version != 0) || (after == json!({}) && version == 0) {
                return Ok(());
            }
            match store.set(DRAFT_FILES, after, Some(version)).await {
                Err(e) if e.code() == Some("conflict") && tries < RACES => tries += 1,
                done => return done,
            }
        }
    }

    /// What the store answered for `kind` (listed under `keys`, as `entries`): taken, so no longer owed unless it was
    /// listed again meanwhile; refused with a code, so dropped (it stays only here, as in the TS agent); anything else
    /// stays owed. False when the store couldn't be reached, which ends the pass.
    fn settled(&self, kind: &str, keys: &[&str], entries: &[&Owed], sent: Result<(), BacklogError>) -> bool {
        let mut me = self.lock();
        let mut drop_all = || {
            for (key, entry) in keys.iter().zip(entries) {
                if me.owed.get(*key) == Some(*entry) {
                    me.owed.remove(*key);
                }
            }
        };
        match sent {
            Ok(()) => {
                drop_all();
                me.save();
                true
            }
            Err(e) if e.is_refusal() && e.code() != Some("conflict") => {
                tracing::warn!("facts: the shared backlog refused this computer's {kind}, so it stays here only: {e}");
                drop_all();
                me.save();
                true
            }
            Err(e) => {
                tracing::warn!("facts: couldn't send {kind} to the shared backlog, trying again soon: {e}");
                false
            }
        }
    }
}

impl Inner {
    fn save(&self) {
        let Some(file) = &self.file else { return };
        let value = serde_json::to_value(&self.owed).unwrap_or_default();
        if let Err(e) = crate::fs_util::write_json_atomic(file, &value, true) {
            tracing::warn!("saving {} failed: {e:#}", file.display());
        }
    }
}

#[cfg(test)]
mod tests;
