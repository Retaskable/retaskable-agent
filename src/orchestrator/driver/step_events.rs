//! The `step_start` key and `again`, and the `step_end` each step writes (docs/specs/agent-telemetry.md
//! section 3.3). A step is open from its `step_start` until its task ends, panics or is cut.
use std::time::Instant;

use super::ProjectLoop;
use crate::item_lifecycle::{ItemRun, ItemState};
use crate::orchestrator::steps::{StepDetail, state_name};
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
    pub(super) fn step_ended(&mut self, item: &str, ok: bool, outcome: &str, detail: Option<&StepDetail>) {
        let Some(open) = self.open_steps.remove(item) else {
            return;
        };
        let review = detail.filter(|_| open.step == "reviewing");
        let end = event::StepEnd {
            item: Some(item.into()),
            step: Some(open.step),
            key: Some(open.key),
            ms: Some(open.began.elapsed().as_millis() as f64),
            ok: Some(ok),
            outcome: Some(outcome.into()),
            checks: detail.filter(|d| !d.checks.is_empty()).map(|d| {
                (d.checks.iter())
                    .map(|c| event::Check {
                        name: Some(c.name.clone()),
                        ok: Some(c.ok),
                        sec: Some(c.sec),
                        ..Default::default()
                    })
                    .collect()
            }),
            // A review's diff lines and scout count; never on a step that panicked or was cut.
            lines: review.and_then(|d| d.lines),
            scouts: review.and_then(|d| d.scouts),
            ..Default::default()
        };
        self.events.add_in(&self.id, Event::StepEnd(end));
    }

    /// A restart or shutdown cuts every step still running: each writes its `step_end` with `ok` false.
    pub(super) fn cut_steps(&mut self) {
        let items: Vec<String> = self.open_steps.keys().cloned().collect();
        for item in items {
            self.step_ended(&item, false, "cut", None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(state: ItemState, slice: u32, check_rounds: u32, review_rounds: u32) -> ItemRun {
        ItemRun {
            state,
            slice,
            check_rounds,
            review_rounds,
            ..ItemRun::new()
        }
    }

    fn start(item: &str, step: &str, key: &str) -> Entry {
        let s = event::StepStart {
            item: Some(item.into()),
            step: Some(step.into()),
            key: Some(key.into()),
            ..Default::default()
        };
        Entry::new("t".into(), None, Event::StepStart(s))
    }

    fn end(item: &str) -> Entry {
        let e = event::StepEnd {
            item: Some(item.into()),
            ..Default::default()
        };
        Entry::new("t".into(), None, Event::StepEnd(e))
    }

    #[test]
    fn a_step_key_is_its_one_based_slice_and_for_checks_and_reviews_the_round() {
        assert_eq!(key_of(&run(ItemState::Queued, 0, 0, 0)), "slice 1");
        assert_eq!(key_of(&run(ItemState::Checking, 1, 0, 3)), "slice 2 round 1");
        assert_eq!(key_of(&run(ItemState::Checking, 0, 2, 0)), "slice 1 round 3");
        assert_eq!(key_of(&run(ItemState::Reviewing, 2, 5, 1)), "slice 3 round 2");
    }

    #[test]
    fn a_step_is_open_in_the_log_until_its_step_end() {
        let log = [start("a", "checking", "slice 1 round 1")];
        assert!(open_in_log(&log, "a", "checking", "slice 1 round 1"));
        assert!(!open_in_log(&[], "a", "checking", "slice 1 round 1"));
        let closed = [log[0].clone(), end("a")];
        assert!(!open_in_log(&closed, "a", "checking", "slice 1 round 1"));
    }

    #[test]
    fn only_the_same_step_and_key_count_as_open() {
        let log = [start("a", "checking", "slice 1 round 1")];
        assert!(!open_in_log(&log, "a", "reviewing", "slice 1 round 1"));
        assert!(!open_in_log(&log, "a", "checking", "slice 1 round 2"));
    }

    #[test]
    fn another_items_events_neither_open_nor_close_a_step() {
        let log = [
            start("a", "checking", "slice 1 round 1"),
            end("b"),
            start("b", "reviewing", "slice 1 round 1"),
        ];
        assert!(open_in_log(&log, "a", "checking", "slice 1 round 1"));
        let log = [
            start("a", "checking", "slice 1 round 1"),
            start("a", "reviewing", "slice 1 round 1"),
        ];
        assert!(!open_in_log(&log, "a", "checking", "slice 1 round 1"));
        assert!(open_in_log(&log, "a", "reviewing", "slice 1 round 1"));
    }
}
