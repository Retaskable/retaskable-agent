//! What the triager is given about a stopped item, gathered from the loop's own state (06.9 bullet 1).
use std::path::Path;

use super::ProjectLoop;

/// How many of an item's recent notes the triager is given.
#[allow(dead_code)] // run by the triage session, a later piece
const TRIAGE_NOTES: usize = 20;

impl ProjectLoop {
    /// The item's recent `item_state` notes from this project's event log, newest first, at most `max`.
    pub(super) fn item_notes(&self, item_id: &str, max: usize) -> Vec<String> {
        use crate::telemetry::{Body, Event};
        self.events
            .recent()
            .iter()
            .rev()
            .filter_map(|e| match &e.body {
                Body::Known(Event::ItemState(s)) if s.item.as_deref() == Some(item_id) => s.note.clone(),
                _ => None,
            })
            .take(max)
            .collect()
    }

    /// What the triager is given for an item at `needs_human` (06.9 bullet 1), from its record, its backlog item
    /// and its recent notes. `None` without a record.
    #[allow(dead_code)] // run by the triage session, a later piece
    pub(super) fn triage_context(&self, item_id: &str, data_dir: &Path) -> Option<crate::triage::TriageContext> {
        let record = self.records.get(item_id)?;
        Some(crate::triage::record::triage_context(
            record,
            self.items.get(item_id),
            self.item_notes(item_id, TRIAGE_NOTES),
            data_dir,
            &self.id,
            item_id,
        ))
    }
}
