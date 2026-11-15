//! This computer's Claude week as the store takes it (`agent.week`, backlog/06.6-handing-out-work.md section 6.6.5),
//! and the store's `{ passed: { to, retryMs } }` answer when another computer with more of its week left takes the
//! work instead.

use serde::Serialize;
use serde_json::{Map, Value};

/// `agent.week`: the part of the account's weekly limit used (0..1, whole percent) and when the week resets (ms).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeekUse {
    pub used: f64,
    pub resets_at: i64,
}

impl WeekUse {
    /// The first reading that carries the weekly figure and a reset still ahead of `now_secs`; `None` otherwise
    /// (no reading, a status-only one, or a week that has reset), so the store counts this computer's room unknown.
    pub fn of<'a>(readings: impl IntoIterator<Item = &'a Value>, now_secs: i64) -> Option<Self> {
        readings.into_iter().find_map(|reading| {
            let (Some(used), Some(resets_at)) = crate::tiers::week_reading(reading)? else {
                return None;
            };
            ((0.0..=1.0).contains(&used) && resets_at > now_secs).then(|| Self {
                used: (used * 100.0).round() / 100.0,
                resets_at: resets_at * 1000,
            })
        })
    }
}

/// The computer the store left the work for, and when to ask again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passed {
    pub to: String,
    pub retry_ms: u64,
}

impl Passed {
    /// `{ passed: { to, retryMs } }` as an answer's object; `None` when it holds no `passed`.
    pub fn of(answer: &Map<String, Value>) -> Option<Self> {
        let p = answer.get("passed")?;
        Some(Self {
            to: p
                .get("to")
                .and_then(Value::as_str)
                .unwrap_or("another computer")
                .to_string(),
            retry_ms: p.get("retryMs").and_then(Value::as_u64).unwrap_or(60_000),
        })
    }
}

#[cfg(test)]
mod tests;
