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
pub struct Failures(Arc<Mutex<Vec<Failure>>>);

impl Failures {
    pub fn add(&self, op: &str, item: &str, e: &BacklogError) {
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
            status: 409,
            code: Some("conflict".into()),
            message: "nope".into(),
        }
    }

    #[test]
    fn add_records_op_item_code_and_message() {
        let failures = Failures::default();
        let e = refused();
        failures.add("claim", "draft-1", &e);
        assert_eq!(
            failures.take(),
            vec![Failure {
                op: "claim".into(),
                item: "draft-1".into(),
                code: Some("conflict".into()),
                error: e.to_string(),
            }]
        );
    }

    #[test]
    fn an_unreachable_store_has_no_code() {
        let failures = Failures::default();
        failures.add("post", "ask-2", &BacklogError::Unreachable("timeout".into()));
        let taken = failures.take();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].code, None);
    }

    #[test]
    fn take_drains_in_order_and_leaves_it_empty() {
        let failures = Failures::default();
        failures.add("a", "1", &refused());
        failures.add("b", "2", &refused());
        let ops: Vec<_> = failures.take().into_iter().map(|f| f.op).collect();
        assert_eq!(ops, ["a", "b"]);
        assert!(failures.take().is_empty());
    }

    #[test]
    fn clones_share_the_list() {
        let failures = Failures::default();
        let clone = failures.clone();
        clone.add("a", "1", &refused());
        assert_eq!(failures.take().len(), 1);
        assert!(clone.take().is_empty());
    }
}
