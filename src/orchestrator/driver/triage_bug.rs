//! Reporting the Retaskable bug a triager named (docs/specs/agent/06.9-triage-claude-looks-at-needs-you-first.md
//! bullet 19): decided with `bug_report::decide_bug_report` from the hub's consent and known bugs, its signature kept
//! in the item's record before it goes (so it's never sent twice, answered or not, even after a restart), and sent
//! from a task of its own so the loop isn't held. The answer comes back on a channel and adds the timeline line on
//! the next pass; off, unconnected, refused or unanswered adds nothing (the link logs why).
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::ProjectLoop;
use crate::connector::bug_link::{BugHub, BugOutcome};
use crate::triage::RetaskableBug;
use crate::triage::bug_report::{decide_bug_report, scrub_words};

/// Each project's bug-report link, by project id, and the build's version a report carries.
#[derive(Clone, Default)]
pub struct BugHubs {
    pub hubs: HashMap<String, Arc<dyn BugHub>>,
    pub version: String,
}

impl BugHubs {
    pub(super) fn get(&self, project: &str) -> Option<&Arc<dyn BugHub>> {
        self.hubs.get(project)
    }
}

/// The hub's answers to bugs sent from tasks, waiting for the loop to note them.
pub struct BugAnswers {
    tx: UnboundedSender<(String, BugOutcome)>,
    rx: UnboundedReceiver<(String, BugOutcome)>,
}

impl Default for BugAnswers {
    fn default() -> Self {
        let (tx, rx) = unbounded_channel();
        Self { tx, rx }
    }
}

impl ProjectLoop {
    /// Sends `bug` (a finding's `retaskableBug`) unless the hub's consent is off, nothing is left of it once
    /// scrubbed, or the item's record already holds its signature.
    pub(super) fn report_bug(&mut self, item_id: &str, bug: Option<&RetaskableBug>, shared: &super::Shared<'_>) {
        let Some(bug) = bug else { return };
        let Some(hub) = shared.links.bugs.get(&self.id).cloned() else {
            return;
        };
        let Some(record) = self.records.get_mut(item_id).filter(|r| r.state == "needs_human") else {
            return;
        };
        let branch = self
            .items
            .get(item_id)
            .and_then(|i| i.branch.clone())
            .or(record.branch.clone());
        let words = scrub_words(&self.id, &self.project, item_id, branch.as_deref(), &shared.links.env);
        let Some(msg) = decide_bug_report(
            Some(bug),
            record.needs_human_reason.as_deref(),
            &words,
            hub.consent(),
            None,
            &hub.known(),
        ) else {
            tracing::info!("{}: the bug the triager named for {item_id} isn't reported", self.id);
            return;
        };
        if record.reported_bugs.contains(&msg.signature) {
            return;
        }
        record.reported_bugs.push(msg.signature.clone());
        self.save_leases();
        let (item, version, tx) = (
            item_id.to_string(),
            shared.links.bugs.version.clone(),
            self.bug_answers.tx.clone(),
        );
        tokio::spawn(async move {
            let outcome = hub.send(&msg, &version).await;
            let _ = tx.send((item, outcome));
        });
    }

    /// Notes the hub's answers that came in since the last pass on their items' timelines.
    pub(super) fn bugs_answered(&mut self) {
        while let Ok((item_id, outcome)) = self.bug_answers.rx.try_recv() {
            let line = match outcome {
                BugOutcome::Answered(a) if a.ok && a.duplicate => "Retaskable already has this bug on its board",
                BugOutcome::Answered(a) if a.ok => "Reported to Retaskable as a fix",
                _ => continue,
            };
            self.look_note(&item_id, line.into());
        }
    }
}
