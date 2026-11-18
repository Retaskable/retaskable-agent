//! When the run loop leaves the store alone. The TypeScript agent (`pieceRuns.ts` `wanted` and `retryAt`) asks for a run
//! once, and after an empty answer asks no more until work is pushed (`work_available`) or a run ends; after a failed ask it
//! waits a minute. Asking on every poll instead made one project send thousands of `merge_next` and `run_take`, and the
//! store answered 429, which was then asked again as fast.
use std::time::{Duration, Instant};

use crate::backlog::BacklogError;

/// How long to wait after a failed ask (the TS `RETRY_MS`), at the least.
pub(super) const RETRY: Duration = Duration::from_secs(60);
/// The longest an empty answer is trusted. The store pushes `work_available` for new work, so this only catches a push
/// that was lost, or a thing that changed here (a usage limit that lifted) which nothing pushes.
pub(super) const QUIET_FOR: Duration = Duration::from_secs(60);
/// The longest wait a store may ask for in `Retry-After`, so a wrong number can't silence the loop for a day.
const MOST: Duration = Duration::from_secs(15 * 60);

/// What the loop knows of the store's last answers to its asks.
#[derive(Debug, Default)]
pub(super) struct AskHold {
    /// The store had nothing to hand out: no asking until a wake or a run's end (or [`QUIET_FOR`]).
    quiet_until: Option<Instant>,
    /// An ask failed: no asking until then, whatever is pushed (the TS `retryAt`).
    retry_at: Option<Instant>,
}

impl AskHold {
    /// Whether an ask may be sent at `now`.
    pub(super) fn may_ask(&self, now: Instant) -> bool {
        self.quiet_until.is_none_or(|t| now >= t) && self.retry_at.is_none_or(|t| now >= t)
    }

    /// The store had nothing for this computer.
    pub(super) fn nothing(&mut self, now: Instant) {
        self.quiet_until = Some(now + QUIET_FOR);
    }

    /// Work was pushed or a run ended: the next pass asks (unless an ask failed a moment ago).
    pub(super) fn work_may_be_free(&mut self) {
        self.quiet_until = None;
    }

    /// An ask failed (`429` included): wait a minute, or what the store's `Retry-After` says if that is longer.
    pub(super) fn failed(&mut self, e: &BacklogError, now: Instant) {
        self.retry_at = Some(now + e.retry_after().map_or(RETRY, |w| w.clamp(RETRY, MOST)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn too_many(retry_after: Option<u64>) -> BacklogError {
        BacklogError::too_many("Too many requests to the backlog", retry_after.map(Duration::from_secs))
    }

    #[test]
    fn an_empty_answer_holds_asking_until_a_wake_or_the_minute() {
        let (t0, mut hold) = (Instant::now(), AskHold::default());
        assert!(hold.may_ask(t0));
        hold.nothing(t0);
        assert!(!hold.may_ask(t0 + Duration::from_secs(3)));
        hold.work_may_be_free();
        assert!(hold.may_ask(t0 + Duration::from_secs(3)));
        hold.nothing(t0);
        assert!(hold.may_ask(t0 + QUIET_FOR));
    }

    #[test]
    fn a_failed_ask_waits_a_minute_and_a_wake_does_not_end_it() {
        let (t0, mut hold) = (Instant::now(), AskHold::default());
        hold.failed(&too_many(None), t0);
        hold.work_may_be_free();
        assert!(!hold.may_ask(t0 + Duration::from_secs(59)));
        assert!(hold.may_ask(t0 + RETRY));
    }

    #[test]
    fn retry_after_counts_when_longer_and_not_when_shorter_or_absurd() {
        let t0 = Instant::now();
        for (said, waits) in [(Some(5), 60), (Some(300), 300), (Some(10_000_000), 900), (None, 60)] {
            let mut hold = AskHold::default();
            hold.failed(&too_many(said), t0);
            let waits = Duration::from_secs(waits);
            assert!(!hold.may_ask(t0 + waits - Duration::from_secs(1)), "{said:?}");
            assert!(hold.may_ask(t0 + waits), "{said:?}");
        }
    }

    #[test]
    fn a_5xx_waits_a_minute_too() {
        let (t0, mut hold) = (Instant::now(), AskHold::default());
        hold.failed(&BacklogError::Unreachable("down".into()), t0);
        assert!(!hold.may_ask(t0 + Duration::from_secs(30)));
    }
}
