//! How a project's loop reports a pass that couldn't reach the store.
use serde_json::json;

use super::project::ProjectLoop;
use crate::backlog::BacklogError;
use crate::orchestrator::live::now_iso8601;
use crate::telemetry::{Event, event};

impl ProjectLoop {
    /// A pass that couldn't even ask the store (no repo id, no link): the page hears of it too.
    pub(super) fn cant_start(&self, message: String) {
        self.live.failed(&self.id, now_iso8601(), message);
    }

    pub(super) fn store_error(&mut self, op: &str, e: &BacklogError) {
        tracing::warn!("{}: {op} failed: {e}", self.id);
        self.live.failed(&self.id, now_iso8601(), e.to_string());
        self.events.add_in(
            &self.id,
            Event::StoreError(event::StoreError {
                op: Some(op.into()),
                code: e.code().map(|c| json!(c)),
                error: Some(e.to_string()),
                ..Default::default()
            }),
        );
    }
}
