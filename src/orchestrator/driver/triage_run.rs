//! Whether Claude is looking into a stopped item (`claudeOnIt`, docs/specs/agent/06.9-triage-claude-looks-at-needs-you-first.md).
//! The agent starts no look of its own: the store sends a `triage` job as a task (`jobs::triage`, `job_tasks.rs`) and
//! decides what the item becomes. What stays here is the fact the agent reports: a triage job for the item is queued
//! or running here, or a decision of Claude's waits to be carried out. No number and no setting: the store decides
//! whether a look happens.
use super::ProjectLoop;

impl ProjectLoop {
    /// Whether Claude is looking into `item_id` or about to (`claudeOnIt`).
    pub(super) fn claude_on_it(&self, item_id: &str) -> bool {
        self.records.get(item_id).is_some_and(|r| {
            r.state == "needs_human"
                && self.decisions_of(item_id).is_some()
                && (self.triaging.contains(item_id) || self.claude_pending.contains(item_id))
        })
    }
}
