//! The `step_start` key and `again`, and the `step_end` each step writes (docs/specs/agent-telemetry.md
//! section 3.3). A step is open from its `step_start` until its task ends, panics or is cut.
use std::time::Instant;

use super::ProjectLoop;
use crate::item_lifecycle::{ItemRun, ItemState};
use crate::orchestrator::steps::state_name;
use crate::telemetry::event::{Body, Entry};
use crate::telemetry::{Event, event};

/// A step running now: what its `step_end` repeats, and when it began.
pub(super) struct OpenStep {
    step: String,
    key: String,
    began: Instant,
}

/// The step's key: its slice and, for checks and reviews, the round it is in (`slice 2 round 1`).
fn key_of(run: &ItemRun) -> String {
    let round = match run.state {
        ItemState::Checking => Some(run.check_rounds + 1),
        ItemState::Reviewing => Some(run.review_rounds + 1),
        _ => None,
    };
    match round {
        Some(r) => format!("slice {} round {r}", run.slice + 1),
        None => format!("slice {}", run.slice + 1),
    }
}

/// Whether the log has a `step_start` for this item, step and key with no `step_end` after it.
fn open_in_log(recent: &[Entry], item: &str, step: &str, key: &str) -> bool {
    let mut open = false;
    for e in recent {
        match &e.body {
            Body::Known(Event::StepStart(s)) if s.item.as_deref() == Some(item) => {
                open = s.step.as_deref() == Some(step) && s.key.as_deref() == Some(key)
            }
            Body::Known(Event::StepEnd(s)) if s.item.as_deref() == Some(item) => open = false,
            _ => {}
        }
    }
    open
}

impl ProjectLoop {
    /// Writes the `step_start` for `run`'s step and remembers it as open.
    pub(super) fn step_started(&mut self, item: &str, run: &ItemRun) {
        let step = state_name(run.state).to_string();
        // Behind `watchdog-anomaly-rules`; Off, the event is as it always was and no `step_end` follows.
        if !self.flags.last("watchdog-anomaly-rules") {
            let start = event::StepStart {
                item: Some(item.into()),
                step: Some(step),
                ..Default::default()
            };
            self.events.add_in(&self.id, Event::StepStart(start));
            return;
        }
        let key = key_of(run);
        let again = open_in_log(self.events.recent(), item, &step, &key);
        let start = event::StepStart {
            item: Some(item.into()),
            step: Some(step.clone()),
            key: Some(key.clone()),
            again: again.then_some(true),
            ..Default::default()
        };
        self.events.add_in(&self.id, Event::StepStart(start));
        self.open_steps.insert(
            item.into(),
            OpenStep {
                step,
                key,
                began: Instant::now(),
            },
        );
    }

    /// Writes the `step_end` of `item`'s open step, if it has one.
    pub(super) fn step_ended(&mut self, item: &str, ok: bool, outcome: &str) {
        let Some(open) = self.open_steps.remove(item) else {
            return;
        };
        let end = event::StepEnd {
            item: Some(item.into()),
            step: Some(open.step),
            key: Some(open.key),
            ms: Some(open.began.elapsed().as_millis() as f64),
            ok: Some(ok),
            outcome: Some(outcome.into()),
            ..Default::default()
        };
        self.events.add_in(&self.id, Event::StepEnd(end));
    }

    /// A restart or shutdown cuts every step still running: each writes its `step_end` with `ok` false.
    pub(super) fn cut_steps(&mut self) {
        let items: Vec<String> = self.open_steps.keys().cloned().collect();
        for item in items {
            self.step_ended(&item, false, "cut");
        }
    }
}
