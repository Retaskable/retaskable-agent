//! Reporting the Retaskable bug a triager named (06.9 bullet 19, `agent/src/bugReport.ts`): whatever the triager
//! decided, the bug is scrubbed and decided with [`decide_bug_report`] on the hub's current consent and known bugs,
//! its signature is kept in the item's record (answered or not, so it is never sent twice for the item, even after a
//! restart), and it is sent without holding the loop. The timeline line for an answer comes back on the next pass.
use super::{ProjectLoop, Shared};
use crate::connector::bug_link::BugOutcome;
use crate::triage::RetaskableBug;
use crate::triage::bug_report::{decide_bug_report, scrub_words};

impl ProjectLoop {
    /// Decides, records and sends `bug` for `item_id`'s stop. Nothing when there's no link, no bug, no consent, or
    /// the signature was already sent.
    pub(super) fn report_bug(&mut self, item_id: &str, bug: Option<&RetaskableBug>, shared: &Shared<'_>) {
        let (Some((link, version)), Some(record)) = (self.bugs.clone(), self.records.get(item_id)) else {
            return;
        };
        if record.state != "needs_human" {
            return;
        }
        let branch = self.items.get(item_id).and_then(|i| i.branch.clone());
        let words = scrub_words(
            &self.id,
            &self.project,
            item_id,
            branch.as_deref().or(record.branch.as_deref()),
            &shared.links.env,
        );
        let Some(message) = decide_bug_report(
            bug,
            record.needs_human_reason.as_deref(),
            &words,
            link.consent(),
            record.reported_bugs.last().map(String::as_str),
            &link.known(),
        ) else {
            return;
        };
        if let Some(record) = self.records.get_mut(item_id) {
            record.reported_bugs.push(message.signature.clone());
        }
        self.save_leases();
        let notes = self.bug_notes.clone();
        let item = item_id.to_string();
        tokio::spawn(async move {
            let line = match link.send(&message, &version).await {
                BugOutcome::Answered(a) if a.ok && a.duplicate => "Retaskable already has this bug on its board",
                BugOutcome::Answered(a) if a.ok => "Reported to Retaskable as a fix",
                _ => return,
            };
            notes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((item, line.into()));
        });
    }

    /// Adds the timeline lines of bug reports answered since the last pass.
    pub(super) fn bug_notes_in(&mut self) {
        let lines = std::mem::take(&mut *self.bug_notes.lock().unwrap_or_else(|e| e.into_inner()));
        for (item, line) in lines {
            self.look_note(&item, line);
        }
    }
}
