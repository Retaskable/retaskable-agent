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
