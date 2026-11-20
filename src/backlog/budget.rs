//! One budget for every call this computer makes to a store, however many projects it serves.
//!
//! The store limits operations per agent (120 a minute, Retaskable.com `backlog/src/rateLimits.ts`) and per person
//! (600 a minute), and an agent is a computer, not a project: five projects each polling on their own add up to more
//! than the minute allows, and the store answers 429 to all of them. So every call of every project goes through the
//! one [`Budget`] of its store: a token bucket that paces them under [`PER_MINUTE`] (a margin under the limit, which
//! is counted per Cloudflare location, so only roughly), first come first served, and a `429` from any call holds
//! all of them for the store's `Retry-After` (the limit is shared, so one project's refusal is every project's).
//! A call that would wait longer than [`LONGEST_WAIT`] is refused here with the same kind of 429, so its caller's own
//! hold takes over, instead of queueing behind a long one without end. A store on this computer (a test, a local
//! store) is not limited.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::BacklogError;

/// Calls a minute this computer sends to one store, under its 120 an agent.
pub const PER_MINUTE: u32 = 90;
/// Calls that may go at once after a quiet time (a pass of one project is about this many).
pub const BURST: u32 = 12;
/// The longest a call waits for its turn; past this it is refused as `429` (the wait to retry in the message).
pub const LONGEST_WAIT: Duration = Duration::from_secs(90);
/// The hold after a `429` that named no `Retry-After`, and its least: the store's own wording is "wait a minute".
pub const HOLD_LEAST: Duration = Duration::from_secs(60);
/// The most a `Retry-After` may hold the store for, so a wrong number can't silence this computer for a day.
pub const HOLD_MOST: Duration = Duration::from_secs(5 * 60);

/// What the bucket knows at one moment; split out so tests drive it with their own clock.
#[derive(Debug)]
pub struct Bucket {
    interval: Duration,
    burst: u32,
    /// When the next call would be due if every call so far had gone on its schedule (the generic cell rate
    /// algorithm: the bucket is as full as `tat` is far behind `now`).
    tat: Instant,
    /// No call before this: a `429`'s `Retry-After` from any call.
    held_until: Instant,
}

impl Bucket {
    pub fn new(per_minute: u32, burst: u32, now: Instant) -> Self {
        Self {
            interval: Duration::from_secs(60) / per_minute.max(1),
            burst: burst.max(1),
            tat: now,
            held_until: now,
        }
    }

    /// How long a call arriving at `now` waits, and its place is kept; or `Err(wait)` when the wait is over
    /// [`LONGEST_WAIT`] (nothing is kept).
    pub fn take(&mut self, now: Instant) -> Result<Duration, Duration> {
        let tat = self.tat.max(now.max(self.held_until));
        let early = self.interval * (self.burst - 1);
        let due = tat.checked_sub(early).unwrap_or(tat).max(self.held_until);
        let wait = due.saturating_duration_since(now);
        if wait > LONGEST_WAIT {
            return Err(wait);
        }
        self.tat = tat + self.interval;
        Ok(wait)
    }

    /// The store answered 429: no call from anyone until `retry_after` (or a minute) has passed, then the calls that queued go
    /// at the bucket's pace, [`BURST`] at once and the rest a call every `60 / PER_MINUTE` seconds.
    pub fn limited(&mut self, now: Instant, retry_after: Option<Duration>) {
        let hold = retry_after.map_or(HOLD_LEAST, |w| w.clamp(HOLD_LEAST, HOLD_MOST));
        self.held_until = self.held_until.max(now + hold);
        self.tat = self.tat.max(self.held_until);
    }

    /// How long a call arriving at `now` would wait for the hold alone.
    pub fn held_for(&self, now: Instant) -> Duration {
        self.held_until.saturating_duration_since(now)
    }
}

/// A store's bucket, shared by every `Client` that talks to it. `None` for a store on this computer.
#[derive(Debug)]
pub struct Budget(Option<Mutex<Bucket>>);

impl Budget {
    pub(crate) fn limited_store() -> Self {
        Self(Some(Mutex::new(Bucket::new(PER_MINUTE, BURST, Instant::now()))))
    }

    /// No limit: a store on this computer.
    pub fn none() -> Self {
        Self(None)
    }

    /// Waits for this call's turn. A call that would wait too long is refused with a 429 naming the wait.
    pub async fn wait(&self) -> Result<(), BacklogError> {
        let Some(bucket) = &self.0 else { return Ok(()) };
        let taken = crate::sync::lock(bucket).take(Instant::now());
        match taken {
            Ok(wait) if wait.is_zero() => Ok(()),
            Ok(wait) => {
                tokio::time::sleep(wait).await;
                Ok(())
            }
            Err(wait) => Err(BacklogError::too_many(
                "Too many requests to the backlog: this computer is holding its calls back",
                Some(wait),
            )),
        }
    }

    /// A call was answered: a 429 holds every call for its `Retry-After`, which is returned (`None` for any other answer).
    pub fn answered(&self, res: &reqwest::Response) -> Option<Duration> {
        if res.status().as_u16() != 429 {
            return None;
        }
        let retry_after = retry_after(res);
        self.limited(retry_after);
        retry_after
    }

    /// A call was answered 429 (`retry_after`: its `Retry-After`).
    pub fn limited(&self, retry_after: Option<Duration>) {
        if let Some(bucket) = &self.0 {
            let mut bucket = crate::sync::lock(bucket);
            let was = bucket.held_for(Instant::now());
            bucket.limited(Instant::now(), retry_after);
            if was.is_zero() {
                tracing::warn!(
                    "the store limited this computer's calls: all projects wait {} s",
                    bucket.held_for(Instant::now()).as_secs()
                );
            }
        }
    }
}

/// The budget of the store at `base_url`: one for the whole process, whoever builds a client for it.
pub fn for_store(base_url: &str) -> Arc<Budget> {
    static BUDGETS: OnceLock<Mutex<HashMap<String, Arc<Budget>>>> = OnceLock::new();
    let key = base_url.trim_end_matches('/').to_ascii_lowercase();
    let mut budgets = crate::sync::lock(BUDGETS.get_or_init(Default::default));
    budgets
        .entry(key.clone())
        .or_insert_with(|| {
            Arc::new(if on_this_computer(&key) {
                Budget::none()
            } else {
                Budget::limited_store()
            })
        })
        .clone()
}

fn on_this_computer(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split('/').next().unwrap_or("");
    host.starts_with("[::1]") || matches!(host.split(':').next(), Some("127.0.0.1" | "localhost"))
}

/// A response's `Retry-After`, in whole seconds (an HTTP date is not read: the hold is then the minute's floor).
fn retry_after(res: &reqwest::Response) -> Option<Duration> {
    let v = res.headers().get(reqwest::header::RETRY_AFTER)?;
    v.to_str().ok()?.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod client_tests;
#[cfg(test)]
mod tests;
