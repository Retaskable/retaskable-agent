//! The usage recheck on the tick (docs/specs/small-agent-jobs.md): while Claude's limit holds the computer, one tiny
//! question is asked every ten minutes ([`crate::usage_check`]) and its rate-limit event recorded as a session's
//! would be, so a hold that no longer applies ends on the next tick.
use std::time::Instant;

use serde_json::Value;

use super::machine::Machine;
use super::project::ProjectLoop;
use crate::telemetry::event::{self, Event};
use crate::usage_check::Reading;

impl Machine {
    pub(super) async fn recheck_usage(&mut self) {
        if let Some(reading) = self.usage_check.take().await {
            self.apply_reading(reading);
        }
        let now = Instant::now();
        let signed_out = self.claude_signed_out_until.is_some_and(|until| now < until);
        if !signed_out && self.engine_limits.claude_at_limit(now) && self.usage_check.due(now) {
            self.usage_check.start(&self.links.env, now);
        }
    }

    pub(super) fn apply_reading(&mut self, reading: Reading) {
        let was_waiting = self.engine_limits.claude_at_limit(Instant::now());
        let (status, left_pct) = match &reading {
            Reading::Unread => return,
            Reading::Answered => ("allowed".to_string(), None),
            Reading::Event(usage) => (
                usage["status"].as_str().unwrap_or("allowed").to_string(),
                five_hour_left_pct(usage),
            ),
        };
        if let Reading::Event(usage) = &reading {
            self.record(usage);
        } else {
            self.engine_limits.clear_claude();
        }
        let waiting = self.engine_limits.claude_at_limit(Instant::now());
        for lp in &mut self.projects {
            lp.events.add_in(
                &lp.id,
                Event::UsageRechecked(event::UsageRechecked {
                    status: Some(status.clone()),
                    left_pct,
                    was_waiting: Some(was_waiting),
                    waiting: Some(waiting),
                    ..Default::default()
                }),
            );
        }
    }

    fn record(&mut self, usage: &Value) {
        let week = crate::tiers::carries_week(usage);
        let five_hour = crate::tiers::is_usage_reading(usage);
        if five_hour {
            crate::tiers::note_usage(usage);
        }
        // What was held on an older reading ends, then this one holds again if it still applies.
        let resets = crate::engines::engine::resets_in(Some(usage));
        let rejected = usage["status"] == "rejected";
        if !rejected || resets.is_some() {
            self.engine_limits.clear_claude();
        }
        for lp in &mut self.projects {
            if week {
                lp.store.set_week_usage(usage.clone());
            }
            if five_hour {
                lp.store.set_usage(usage.clone());
            }
            if let Err(e) = lp.store.save() {
                tracing::warn!("{}: saving the usage reading failed: {e:#}", lp.id);
            }
            ProjectLoop::cut_off_for_week(&lp.id, usage, &mut self.engine_limits);
        }
        if rejected && let Some(left) = resets {
            self.engine_limits
                .hit(crate::engines::EngineId::Claude, Instant::now() + left);
        }
    }
}

/// The 5-hour window's share left, in whole percent, when the reading carries one. A reading labeled with another
/// window (a weekly `rateLimitType`) says nothing about the 5-hour one.
fn five_hour_left_pct(usage: &Value) -> Option<i64> {
    let used = crate::view::usage::five_hour(usage)?.get("utilization")?.as_f64()?;
    Some(((1.0 - crate::tiers::frac(used)) * 100.0).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_weekly_reading_is_not_the_five_hour_share_left() {
        let week = json!({"status": "rejected", "rateLimitType": "seven_day", "utilization": 0.9});
        assert_eq!(five_hour_left_pct(&week), None);
        let five = json!({"unifiedWindows": {"five_hour": {"utilization": 0.25}, "seven_day": {"utilization": 0.9}}});
        assert_eq!(five_hour_left_pct(&five), Some(75));
        assert_eq!(five_hour_left_pct(&json!({"utilization": 40})), Some(60));
    }
}
