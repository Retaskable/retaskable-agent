//! The owner's GitHub REST allowance (5000 an hour, shared by every program on the account), as the agent last
//! saw it. Every `gh api -i` answer, a 304 too, carries `X-RateLimit-*`, so reading the count costs no call. Below 20%
//! left the agent stops asking for what it already has (a cached answer is reused for a minute) and its polls slow;
//! below 5% it holds until the window resets, so the account never reaches 0 and blinds the other programs on it.
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Below this share of the limit the budget is low.
const LOW_PERCENT: u64 = 20;
/// Below this share it is critical.
const CRITICAL_PERCENT: u64 = 5;
/// The longest a cached answer is held back from a new request, however far the reset is.
const CRITICAL_HOLD_MAX: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Normal,
    Low,
    Critical,
}

#[derive(Debug, Clone, Copy)]
struct Seen {
    limit: u64,
    remaining: u64,
    /// Epoch seconds the window resets.
    reset: u64,
}

#[derive(Default)]
pub struct Budget {
    seen: Mutex<Option<Seen>>,
}

fn epoch_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The process's budget: what every `gh api` answer has said of the account.
pub fn global() -> &'static Budget {
    static B: OnceLock<Budget> = OnceLock::new();
    B.get_or_init(Budget::default)
}

impl Budget {
    /// Takes the `x-ratelimit-*` lines of a `gh api -i` answer's head. Another resource than `core` (search,
    /// graphql) has its own allowance and is ignored.
    pub fn record_head(&self, head: &str) {
        let (mut limit, mut remaining, mut reset, mut core) = (None, None, None, true);
        for line in head.lines() {
            let Some((k, v)) = line.split_once(':') else { continue };
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            match k.as_str() {
                "x-ratelimit-limit" => limit = v.parse().ok(),
                "x-ratelimit-remaining" => remaining = v.parse().ok(),
                "x-ratelimit-reset" => reset = v.parse().ok(),
                "x-ratelimit-resource" => core = v == "core",
                _ => {}
            }
        }
        if let (true, Some(limit), Some(remaining), Some(reset)) = (core, limit, remaining, reset) {
            self.record(limit, remaining, reset);
        }
    }

    pub fn record(&self, limit: u64, remaining: u64, reset: u64) {
        *self.seen.lock().unwrap() = Some(Seen {
            limit,
            remaining,
            reset,
        });
    }

    /// What is known now: nothing once the window it was read in has reset.
    fn current(&self, now: u64) -> Option<Seen> {
        self.seen.lock().unwrap().filter(|s| s.reset > now && s.limit > 0)
    }

    pub fn level_at(&self, now: u64) -> Level {
        match self.current(now) {
            Some(s) if s.remaining * 100 < s.limit * CRITICAL_PERCENT => Level::Critical,
            Some(s) if s.remaining * 100 < s.limit * LOW_PERCENT => Level::Low,
            _ => Level::Normal,
        }
    }

    pub fn level(&self) -> Level {
        self.level_at(epoch_s())
    }

    /// How long an answer already held is used again instead of asked for again: nothing while the budget is
    /// normal, a minute while low, and until the reset (at most 15 minutes) while critical.
    pub fn reuse_for_at(&self, now: u64) -> Duration {
        match self.level_at(now) {
            Level::Normal => Duration::ZERO,
            Level::Low => Duration::from_secs(60),
            Level::Critical => self.current(now).map_or(Duration::ZERO, |s| {
                Duration::from_secs(s.reset - now).min(CRITICAL_HOLD_MAX)
            }),
        }
    }

    pub fn reuse_for(&self) -> Duration {
        self.reuse_for_at(epoch_s())
    }

    /// How many times longer a poll waits: 1 normally, 3 while low, 10 while critical.
    pub fn slow_factor(&self) -> u32 {
        match self.level() {
            Level::Normal => 1,
            Level::Low => 3,
            Level::Critical => 10,
        }
    }

    /// For `/api/health` and the status file: `None` until an answer has said.
    pub fn report(&self) -> Option<Value> {
        let s = self.current(epoch_s())?;
        let level = match self.level() {
            Level::Normal => "ok",
            Level::Low => "low",
            Level::Critical => "critical",
        };
        Some(json!({
            "remaining": s.remaining,
            "limit": s.limit,
            "resetsAt": s.reset * 1000,
            "level": level,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000;

    #[test]
    fn the_level_follows_the_share_left() {
        let b = Budget::default();
        assert_eq!(b.level_at(NOW), Level::Normal, "nothing known");
        b.record(5000, 4000, NOW + 600);
        assert_eq!(b.level_at(NOW), Level::Normal);
        b.record(5000, 999, NOW + 600);
        assert_eq!(b.level_at(NOW), Level::Low);
        b.record(5000, 249, NOW + 600);
        assert_eq!(b.level_at(NOW), Level::Critical);
        assert_eq!(b.level_at(NOW + 600), Level::Normal, "the window has reset");
    }

    #[test]
    fn cached_answers_are_reused_as_the_budget_drops() {
        let b = Budget::default();
        b.record(5000, 4000, NOW + 600);
        assert_eq!(b.reuse_for_at(NOW), Duration::ZERO);
        b.record(5000, 500, NOW + 600);
        assert_eq!(b.reuse_for_at(NOW), Duration::from_secs(60));
        b.record(5000, 10, NOW + 600);
        assert_eq!(b.reuse_for_at(NOW), Duration::from_secs(600));
        b.record(5000, 10, NOW + 7200);
        assert_eq!(b.reuse_for_at(NOW), CRITICAL_HOLD_MAX);
    }

    #[test]
    fn the_headers_of_a_gh_answer_are_read_and_another_resource_is_not() {
        let b = Budget::default();
        let far = epoch_s() + 3600;
        b.record_head(&format!(
            "HTTP/2.0 304 Not Modified\nX-Ratelimit-Limit: 5000\nX-Ratelimit-Remaining: 123\nX-Ratelimit-Reset: {far}\nX-Ratelimit-Resource: core\n"
        ));
        assert_eq!(b.level(), Level::Critical);
        assert_eq!(b.report().unwrap()["remaining"], 123);
        let other = Budget::default();
        other.record_head(&format!(
            "HTTP/2.0 200 OK\nX-Ratelimit-Limit: 30\nX-Ratelimit-Remaining: 1\nX-Ratelimit-Reset: {far}\nX-Ratelimit-Resource: search\n"
        ));
        assert!(other.report().is_none());
    }
}
