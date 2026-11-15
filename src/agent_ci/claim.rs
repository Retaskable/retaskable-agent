//! docs/specs/agent-ci.md section 3.4: several computers, one repo. The store's lease (`ci_claim`, `store.rs`)
//! is the claim; this file decides which commits want a run at all (`wants_run`, a port of `.agent-ci/claim.mjs`)
//! and keeps a running check's claim fresh (`refreshing`).
use std::future::Future;
use std::time::Duration;

use super::report::Status;

/// A running check is heartbeat to the store (and its pending status posted again) this often.
pub const REFRESH: Duration = Duration::from_secs(5 * 60);
/// A pending status of another computer older than this is stale (only asked when no store decides).
pub const STALE_MS: i64 = 8 * 60 * 1000;

/// Whether this computer (`me`, `agent-ci <name>`) should try a commit, given its newest status for the check's
/// context. A status from another CI counts unless it passed; our own `pending` is a run a stopped runner left;
/// another computer's `pending` holds until it goes stale, unless `leased`: the store's lease decides who runs it
/// (`ci_claim`), not the status's age.
pub fn wants_run(status: Option<&Status>, me: &str, now_ms: i64, leased: bool) -> bool {
    let Some(s) = status else { return true };
    if !s.description.starts_with("agent-ci") {
        return s.state != "success";
    }
    if s.state != "pending" {
        return false;
    }
    if leased || s.description.starts_with(&format!("{me}:")) {
        return true;
    }
    let at = chrono::DateTime::parse_from_rfc3339(&s.updated_at)
        .ok()
        .map(|t| t.timestamp_millis());
    // An unreadable time is as good as a stale one: `Date.parse` gives NaN there, and `NaN > x` is false, so
    // the runner leaves it alone. Same here.
    at.is_some_and(|t| now_ms - t > STALE_MS)
}

/// The newest status of `context` among a commit's statuses (the host lists newest first).
pub fn newest<'a>(statuses: &'a [Status], context: &str) -> Option<&'a Status> {
    statuses.iter().find(|s| s.context == context)
}

/// Runs `work`, calling `post` every `every` meanwhile (the heartbeat). Returns `work`'s output.
pub async fn refreshing<T, P, Fut>(every: Duration, post: P, work: impl Future<Output = T>) -> T
where
    P: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::pin!(work);
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    loop {
        tokio::select! {
            out = &mut work => return out,
            _ = tick.tick() => post().await,
        }
    }
}

#[cfg(test)]
mod tests;
