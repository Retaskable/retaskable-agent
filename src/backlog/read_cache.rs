//! The reads of one pass, made once. A project's loop asks for its items (`list`), its drafts and its intent rows from
//! several places in one pass (the held drafts, the landed report, the main PR, the eight intent pullers), and each ask
//! was a call against the store's limit. Here the answer of one read is kept for [`FRESH`] and given to whoever asks
//! next, a read already under way is waited for rather than sent again, and anything this computer writes to the
//! project drops what was kept of it, so a read after a change is never from before it. An error is never kept.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{BacklogError, Client};

/// How long a read is shared. Tests read every time, as their fake stores answer.
const FRESH: Duration = if cfg!(test) {
    Duration::ZERO
} else {
    Duration::from_secs(5)
};

/// The ops that change nothing a kept read holds, so they leave it kept. Any other op on the project drops it.
const LEAVES_READS: [&str; 8] = [
    "list",
    "drafts",
    "intent_list",
    "settings_get",
    "health",
    "intent_beat",
    "run_beat",
    "intent_seen",
];

type Slot = Arc<tokio::sync::Mutex<Option<(Instant, Value)>>>;

/// The reads kept, by project and op.
#[derive(Default)]
pub struct Reads {
    kept: Mutex<HashMap<(u64, &'static str), Slot>>,
    /// How long they last when a test says (`None`: [`FRESH`]).
    pub(super) fresh: Option<Duration>,
}

impl Reads {
    fn slot(&self, repo: u64, op: &'static str) -> Slot {
        crate::sync::lock(&self.kept).entry((repo, op)).or_default().clone()
    }

    /// `op` is about to be sent for `repo`: what it may change is no longer what was read.
    pub(super) fn written(&self, repo: u64, op: &str) {
        if LEAVES_READS.contains(&op) {
            return;
        }
        for ((r, _), slot) in crate::sync::lock(&self.kept).iter() {
            if *r == repo
                && let Ok(mut held) = slot.try_lock()
            {
                *held = None;
            }
        }
    }
}

impl Client {
    /// `op` with no arguments, answered from what was read within [`FRESH`] when there is such an answer.
    pub(super) async fn read_cached<T: DeserializeOwned>(
        &self,
        repo_id: u64,
        token: Option<&str>,
        op: &'static str,
        what: &str,
    ) -> Result<T, BacklogError> {
        let slot = self.reads.slot(repo_id, op);
        let mut held = slot.lock().await;
        let fresh = self.reads.fresh.unwrap_or(FRESH);
        if !held.as_ref().is_some_and(|(at, _)| at.elapsed() < fresh) {
            let value: Value = self.op(repo_id, token, op, json!({}), what).await?;
            *held = Some((Instant::now(), value));
        }
        let Some((_, value)) = held.as_ref() else {
            unreachable!("read just above")
        };
        serde_json::from_value(value.clone()).map_err(|e| BacklogError::BadAnswer {
            status: 200,
            code: None,
            message: format!("couldn't read the {what} it sent: {e}"),
        })
    }
}

#[cfg(test)]
mod tests;
