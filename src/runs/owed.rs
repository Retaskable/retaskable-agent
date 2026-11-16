//! A finished run's `run_done` (or a merge turn's `merge_result`) the store hasn't taken, kept in the state file so a
//! restart sends it again (the owner's outage rule, 2026-11-16: "it should be able to finish what it's doing but it
//! can't pick up the next thing till the store is back."). Only work that has ended is kept; a run still going when
//! the process dies is gone with it (pieces.md section 3.3). The store takes the end of the same agent and token after
//! a lapse while nobody touched the work; `stale_run` says it moved on.
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::PendingEnd;
use crate::backlog::runs::RunOutcome;

/// How long an end is kept before it is given up on: by then the store has long since moved the run on.
pub const OWED_KEPT_SECS: i64 = 24 * 60 * 60;

/// One end in `state.run_ends_owed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OwedEnd {
    pub token: String,
    /// [`RunOutcome::to_json`].
    pub outcome: Value,
    pub result: Value,
    /// A merge turn's end: sent as `merge_result`.
    #[serde(default)]
    pub merge: bool,
    /// When it was first kept, in epoch seconds.
    pub since: i64,
}

impl OwedEnd {
    pub fn of(end: &PendingEnd, since: i64) -> Self {
        Self {
            token: end.token.clone(),
            outcome: end.outcome.to_json(),
            result: end.result.clone(),
            merge: end.merge,
            since,
        }
    }

    /// The end to send again, due at once; `None` when it is past [`OWED_KEPT_SECS`] or unreadable.
    pub fn restore(&self, now_secs: i64, due: Instant) -> Option<PendingEnd> {
        if now_secs.saturating_sub(self.since) >= OWED_KEPT_SECS {
            return None;
        }
        Some(PendingEnd {
            token: self.token.clone(),
            outcome: RunOutcome::from_json(&self.outcome)?,
            result: self.result.clone(),
            due,
            merge: self.merge,
            restored: true,
        })
    }
}
