//! How a project's loop reports a pass that couldn't reach the store.
use serde_json::json;

use super::project::ProjectLoop;
use crate::backlog::BacklogError;
use crate::orchestrator::live::now_iso8601;
use crate::telemetry::{Event, event};

/// The release flag: Off, a failed draft or ask write is only logged to the console.
pub(super) const WATCH: &str = "watchdog-anomaly-rules";

impl ProjectLoop {
    /// A pass that couldn't even ask the store (no repo id, no link): the page hears of it too.
    pub(super) fn cant_start(&self, message: String) {
        self.live.failed(&self.id, now_iso8601(), message);
    }

    /// A failed write for one draft or ask: the event log hears of it, nothing else changes.
    pub(super) async fn store_failure(&mut self, op: &str, item: &str, e: &BacklogError) {
        self.failures.when(self.flags.on(WATCH).await).add(op, item, e);
        self.drain_failures();
    }

    /// Writes the failures spawned tasks kept as `store_error` events.
    pub(super) fn drain_failures(&mut self) {
        for f in self.failures.take() {
            self.events.add_in(
                &self.id,
                Event::StoreError(event::StoreError {
                    item: Some(f.item),
                    op: Some(f.op),
                    code: f.code.map(|c| json!(c)),
                    error: Some(f.error),
                    ..Default::default()
                }),
            );
        }
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
