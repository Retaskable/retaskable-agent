//! Reporting the triager's Retaskable bugs to the hub (docs/specs/agent/06.9-triage-claude-looks-at-needs-you-first.md
//! bullet 19): a finding that carries `retaskableBug` is decided by [`decide_bug_report`] against the hub's consent and
//! known bugs, marked on the item's record before it's sent (so a restart or a later look never sends it twice,
//! answered or not), and sent from its own task so the loop isn't held. The hub's answer adds a timeline line on a
//! later pass; anything else adds nothing and is logged.
use futures_util::FutureExt;
use tokio::task::JoinHandle;

use super::{ProjectLoop, Shared};
use crate::connector::bug_link::{BugOutcome, BugReported};
use crate::triage::RetaskableBug;
use crate::triage::bug_report::{decide_bug_report, scrub_words};

/// A bug on its way to the hub, for the item whose look found it.
pub(super) type BugSend = (String, JoinHandle<BugOutcome>);

impl ProjectLoop {
    /// Decides, marks and sends the bug `item_id`'s look named, if any.
    pub(super) fn report_bug(&mut self, item_id: &str, bug: Option<RetaskableBug>, shared: &Shared<'_>) {
        let Some(bug) = bug else { return };
        let Some(link) = shared.links.bugs.get(&self.id).cloned() else {
            tracing::info!("{}: no bug report for {item_id}: the project has no hub link", self.id);
            return;
        };
        let Some(record) = self.records.get_mut(item_id) else {
            return;
        };
        let words = scrub_words(
            &self.id,
            &self.project,
            item_id,
            record.branch.as_deref(),
            &shared.links.env,
        );
        let message = decide_bug_report(
            Some(&bug),
            record.needs_human_reason.as_deref(),
            &words,
            link.consent(),
            None,
            &link.known(),
        )
        .filter(|m| !record.reported_bugs.contains(&m.signature));
        let Some(message) = message else {
            tracing::info!(
                "{}: no bug report sent for {item_id}: not agreed, empty or already reported",
                self.id
            );
            return;
        };
        record.reported_bugs.push(message.signature.clone());
        self.save_leases();
        let version = shared.links.bug_version.clone();
        let sent = tokio::spawn(async move { link.send(&message, &version).await });
        self.bug_sends.push((item_id.to_string(), sent));
    }

    /// Adds the timeline line for each bug the hub has answered since the last pass.
    pub(super) fn bug_answers(&mut self) {
        let (done, waiting): (Vec<BugSend>, Vec<BugSend>) = std::mem::take(&mut self.bug_sends)
            .into_iter()
            .partition(|(_, h)| h.is_finished());
        self.bug_sends = waiting;
        for (item_id, handle) in done {
            let Some(Ok(BugOutcome::Answered(BugReported {
                ok: true, duplicate, ..
            }))) = handle.now_or_never()
            else {
                continue;
            };
            let note = if duplicate {
                "Retaskable already has this bug on its board"
            } else {
                "Reported to Retaskable as a fix"
            };
            self.look_note(&item_id, note.into());
        }
    }
}
