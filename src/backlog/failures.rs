//! Store failures met in a spawned task, which has no event log: the task keeps them here and its project's loop
//! writes them as `store_error` events on its next pass (agent-telemetry.md section 3.3).
use std::sync::{Arc, Mutex};

use super::BacklogError;

/// One failed store operation: its name, the draft or ask it was for, the store's refusal code and the message.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub op: String,
    pub item: String,
    pub code: Option<String>,
    pub error: String,
}

/// A shared list of [`Failure`]s; clones share it.
#[derive(Debug, Clone, Default)]
pub struct Failures(Arc<Mutex<Vec<Failure>>>, bool);

impl Failures {
    /// This list as a task should hold it: with `on` false (the `watchdog-anomaly-rules` flag Off) it keeps nothing.
    pub fn when(&self, on: bool) -> Self {
        Self(self.0.clone(), on)
    }

    pub fn add(&self, op: &str, item: &str, e: &BacklogError) {
        if !self.1 {
            return;
        }
        let failure = Failure {
            op: op.into(),
            item: item.into(),
            code: e.code().map(str::to_string),
            error: e.to_string(),
        };
        self.0.lock().unwrap_or_else(|p| p.into_inner()).push(failure);
    }

    /// Everything added since the last call.
    pub fn take(&self) -> Vec<Failure> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused() -> BacklogError {
        BacklogError::BadAnswer {
            status: 400,
            code: Some("bad_request".into()),
            message: "nope".into(),
        }
    }

    #[test]
    fn a_default_list_keeps_nothing() {
        let failures = Failures::default();
        failures.add("draft", "d1", &refused());
        assert!(failures.take().is_empty());
    }

    #[test]
    fn when_off_keeps_nothing_even_if_the_list_was_on() {
        let on = Failures::default().when(true);
        let off = on.when(false);
        off.add("draft", "d1", &refused());
        assert!(on.take().is_empty());
    }

    #[test]
    fn records_the_op_item_code_and_message() {
        let failures = Failures::default().when(true);
        failures.add("draft", "d1", &refused());
        failures.add("ask", "a1", &BacklogError::Unreachable("timeout".into()));
        assert_eq!(
            failures.take(),
            vec![
                Failure {
                    op: "draft".into(),
                    item: "d1".into(),
                    code: Some("bad_request".into()),
                    error: "the backlog store answered 400: nope".into(),
                },
                Failure {
                    op: "ask".into(),
                    item: "a1".into(),
                    code: None,
                    error: "couldn't reach the backlog store: timeout".into(),
                },
            ]
        );
    }

    #[test]
    fn take_empties_the_list_and_clones_share_it() {
        let base = Failures::default();
        let task = base.when(true);
        task.add("ask", "a1", &refused());
        assert_eq!(base.clone().take().len(), 1);
        assert!(task.take().is_empty());
    }
}
