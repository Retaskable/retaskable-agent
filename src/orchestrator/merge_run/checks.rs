//! The head's CI, read right before a change lands (`gh pr view --json statusCheckRollup`). The host has no branch
//! protection to refuse a red merge, and a passed change that is only behind its base merges as it is, so landing
//! relies on CI having passed on this exact head: a failing check ends the run `red` (`ci_failed` to the store, which
//! sends the piece to a fix), a pending one is waited for, and a head with no checks at all (no CI) merges.
use serde::Deserialize;
use serde_json::json;

use super::{Job, Outcome, failed};
use crate::backlog::runs::RunOutcome;

/// The longest a head's checks are waited on when the store gives no `mergeUnknownMinutes` to bound it (or a longer one).
const PENDING_MAX: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// One entry of `statusCheckRollup`: a check run (`name`, `status`, `conclusion`) or a commit status (`context`, `state`).
#[derive(Deserialize, Default)]
#[serde(default)]
struct Entry {
    name: String,
    context: String,
    status: String,
    conclusion: String,
    state: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Rollup {
    status_check_rollup: Vec<Entry>,
}

/// What the head's checks add up to.
#[derive(Debug, PartialEq)]
pub(super) enum Ci {
    /// No checks, or all of them passed (or are skipped or neutral).
    Clear,
    Pending,
    /// The names of the checks that failed, errored, timed out or were cancelled.
    Red(Vec<String>),
}

impl Rollup {
    fn ci(&self) -> Ci {
        let mut red = Vec::new();
        let mut pending = false;
        for e in &self.status_check_rollup {
            let name = if e.name.is_empty() { &e.context } else { &e.name };
            let (conclusion, state) = (e.conclusion.to_uppercase(), e.state.to_uppercase());
            if matches!(
                conclusion.as_str(),
                "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "STARTUP_FAILURE" | "ACTION_REQUIRED"
            ) || matches!(state.as_str(), "FAILURE" | "ERROR")
            {
                red.push(name.clone());
            } else if (!e.status.is_empty() && !e.status.eq_ignore_ascii_case("COMPLETED"))
                || matches!(state.as_str(), "PENDING" | "EXPECTED")
            {
                pending = true;
            }
        }
        if !red.is_empty() {
            Ci::Red(red)
        } else if pending {
            Ci::Pending
        } else {
            Ci::Clear
        }
    }
}

impl Job {
    /// `None` when the change may land; otherwise the end of the run: `red` with the failing checks' names, or the turn
    /// given back (`interrupted`) when the checks stay pending past the wait or can't be read.
    pub(super) async fn ci_gate(&self, pr: &str) -> Result<(), Outcome> {
        let asked = tokio::time::Instant::now();
        let cap = self.unknown_for().min(PENDING_MAX);
        loop {
            let rollup = match self
                .gh_json::<Rollup>(&["pr", "view", pr, "--json", "statusCheckRollup,headRefOid"])
                .await
            {
                Ok(r) => r,
                // Fail closed: checks that can't be read are not a pass.
                Err(why) => return Err(failed(why)),
            };
            if self.stopped() {
                return Err(None);
            }
            match rollup.ci() {
                Ci::Clear => return Ok(()),
                Ci::Red(names) => return Err(Some((RunOutcome::Red, json!({ "checks": names })))),
                Ci::Pending if asked.elapsed() >= cap => {
                    tracing::info!("the checks on {pr} are still pending: the turn goes back");
                    return Err(self.lost());
                }
                Ci::Pending => {
                    tokio::time::sleep(self.delays.ask_every).await;
                    if self.stopped() {
                        return Err(None);
                    }
                }
            }
        }
    }
}
