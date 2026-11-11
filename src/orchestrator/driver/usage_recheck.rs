//! The usage recheck on the tick (docs/specs/small-agent-jobs.md): while Claude's limit holds the computer, one tiny
//! question is asked every ten minutes ([`crate::usage_check`]) and its rate-limit event recorded as a session's
//! would be, so a hold that no longer applies ends on the next tick.
use std::time::Instant;

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

    fn apply_reading(&mut self, reading: Reading) {
        for lp in &mut self.projects {
            lp.events.add_in(
                &lp.id,
                Event::UsageRechecked(event::UsageRechecked {
                    reading: Some(reading.word().into()),
                    ..Default::default()
                }),
            );
        }
        let Reading::Event(usage) = &reading else {
            if reading == Reading::Answered {
                self.engine_limits.clear_claude();
            }
            return;
        };
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
