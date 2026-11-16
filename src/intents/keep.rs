//! The finishes of rows the store hasn't taken, kept in a file of the puller's own in the project's folder so a restart
//! sends them again (the owner's outage rule, 2026-11-16: "it should be able to finish what it's doing but it can't pick
//! up the next thing till the store is back."). One file per puller, named for the kinds it runs
//! (`intent_finish.<kinds>.json`), so no two writers share one. A row's finish is listed before it is first sent and
//! removed when the store answers, whatever it says: the store takes the finish of a claim that lapsed while nobody else
//! took the row, and `ok: false` says another computer has it. Rows still running when the process dies are not kept:
//! the store opens them again once the claim goes stale.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backlog::intents::Ending;

/// One finish owed to the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Owed {
    pub ending: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// When it was first listed, in epoch seconds.
    pub since: i64,
}

impl Owed {
    pub fn ending(&self) -> Option<Ending> {
        Some(match self.ending.as_str() {
            "done" => Ending::Done,
            "refused" => Ending::Refused,
            "failed" => Ending::Failed,
            _ => return None,
        })
    }
}

fn word(ending: Ending) -> &'static str {
    match ending {
        Ending::Done => "done",
        Ending::Refused => "refused",
        Ending::Failed => "failed",
    }
}

pub(super) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The finishes owed, by row id, and the file that holds them. Clones share them.
#[derive(Clone)]
pub struct Keep {
    file: Arc<PathBuf>,
    owed: Arc<Mutex<BTreeMap<String, Owed>>>,
}

impl Keep {
    /// Reads `intent_finish.<kinds>.json` in `dir`; an unreadable file is as good as none.
    pub fn open(dir: &Path, kinds: &[&str]) -> Self {
        let file = dir.join(format!("intent_finish.{}.json", kinds.join("+")));
        let owed = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self {
            file: Arc::new(file),
            owed: Arc::new(Mutex::new(owed)),
        }
    }

    /// What is owed now.
    pub fn owed(&self) -> BTreeMap<String, Owed> {
        self.owed.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Lists a finish (it keeps the time it was first listed) and saves the file before anything is sent.
    pub fn add(&self, id: &str, ending: Ending, result: &Option<Value>, why: &Option<String>) {
        let mut owed = self.owed.lock().unwrap_or_else(|e| e.into_inner());
        let since = owed.get(id).map_or_else(now_secs, |o| o.since);
        let entry = Owed {
            ending: word(ending).into(),
            result: result.clone(),
            why: why.clone(),
            since,
        };
        if owed.get(id) != Some(&entry) {
            owed.insert(id.into(), entry);
            self.save(&owed);
        }
    }

    /// The store answered: the finish is no longer owed.
    pub fn done(&self, id: &str) {
        let mut owed = self.owed.lock().unwrap_or_else(|e| e.into_inner());
        if owed.remove(id).is_some() {
            self.save(&owed);
        }
    }

    fn save(&self, owed: &BTreeMap<String, Owed>) {
        let text = serde_json::to_value(owed).unwrap_or_default();
        if let Err(e) = crate::fs_util::write_json_atomic(&self.file, &text, false) {
            tracing::warn!("intents: saving the finishes owed to the store failed: {e:#}");
        }
    }
}
