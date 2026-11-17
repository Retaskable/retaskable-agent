//! Whether Claude is looking into a stopped item (`claudeOnIt`, docs/specs/agent/06.9-triage-claude-looks-at-needs-you-first.md).
//! The agent starts no look of its own: the store sends a `triage` job as a task (`jobs::triage`, `job_tasks.rs`) and
//! decides what the item becomes. What stays here is the fact the agent reports: a triage job for the item is queued
//! or running here, or a decision of Claude's waits to be carried out.
use super::ProjectLoop;
use crate::config::OnOff;
use crate::triage::record::needs_human_facts;
use crate::triage::schedule::claude_on_it;

impl ProjectLoop {
    /// Whether Claude is looking into `item_id` or about to (`claudeOnIt`).
    pub(super) fn claude_on_it(&self, item_id: &str) -> bool {
        self.records.get(item_id).is_some_and(|r| {
            let facts = needs_human_facts(
                r,
                self.decisions_of(item_id),
                self.claude_pending.contains(item_id),
                self.triaging.contains(item_id),
            );
            claude_on_it(&facts, self.project.triage == OnOff::On)
        })
    }
}
