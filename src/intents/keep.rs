//! The finishes of rows the store hasn't taken, kept in a file of the puller's own in the project's folder so a restart
//! sends them again (the owner's outage rule, 2026-11-16: "it should be able to finish what it's doing but it can't pick
//! up the next thing till the store is back."). One file per puller, named for the kinds it runs
//! (`intent_finish.<kinds>.json`), so no two writers share one. A row's finish is listed before it is first sent and
//! removed when the store answers, whatever it says: the store takes the finish of a claim that lapsed while nobody else
//! took the row, and `ok: false` says another computer has it. Rows still running when the process dies are not kept:
//! the store opens them again once the claim goes stale.
use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backlog::intents::Ending;
use crate::owed_file::OwedFile;

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

pub(super) use crate::owed_file::now_secs;

/// The finishes owed, by row id, and the file that holds them. Clones share them.
#[derive(Clone)]
pub struct Keep(OwedFile<Owed>);

impl Keep {
    /// Reads `intent_finish.<kinds>.json` in `dir`.
    pub fn open(dir: &Path, kinds: &[&str]) -> Self {
        Self(OwedFile::open(
            dir.join(format!("intent_finish.{}.json", kinds.join("+"))),
        ))
    }

    /// What is owed now.
    pub fn owed(&self) -> BTreeMap<String, Owed> {
        self.0.all()
    }

    /// Lists a finish (it keeps the time it was first listed) and saves the file before anything is sent.
    pub fn add(&self, id: &str, ending: Ending, result: &Option<Value>, why: &Option<String>) {
        let since = self.0.get(id).map_or_else(now_secs, |o| o.since);
        let entry = Owed {
            ending: word(ending).into(),
            result: result.clone(),
            why: why.clone(),
            since,
        };
        self.0.put(id, entry);
    }

    /// The store answered: the finish is no longer owed.
    pub fn done(&self, id: &str) {
        self.0.remove(id);
    }
}
