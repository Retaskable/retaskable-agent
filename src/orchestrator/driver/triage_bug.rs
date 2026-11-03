//! Reporting the Retaskable bugs a triager names (06.9 bullet 19, `bugReport.ts`): whatever the triager decided, a
//! `retaskableBug` is scrubbed and, while the hub's consent is on and the item hasn't sent that signature, sent
//! through the project's bug link without holding the loop. The signature is kept in the item's record before the
//! send, answered or not. An answer adds a timeline line; anything else adds nothing and the log says why.
use tokio::sync::mpsc;

use super::{Links, ProjectLoop};
use crate::connector::bug_link::BugOutcome;
use crate::triage::bug_report::{decide_bug_report, scrub_words};
use crate::triage::{KnownBug, RetaskableBug};

/// The sent bugs' answers on their way back to the loop.
pub(super) struct Answers {
    tx: mpsc::UnboundedSender<(String, BugOutcome)>,
    rx: mpsc::UnboundedReceiver<(String, BugOutcome)>,
}

impl Default for Answers {
    fn default() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self { tx, rx }
    }
}

impl ProjectLoop {
    /// The bugs on Retaskable's board the hub listed, for the triager to name one as the same bug.
    pub(super) fn known_bugs(&self) -> Vec<KnownBug> {
        self.bugs.as_ref().map(|(link, _)| link.known()).unwrap_or_default()
    }

    /// Sends `bug` if it's worth sending and this item hasn't sent it yet.
    pub(super) fn report_bug(&mut self, item_id: &str, bug: Option<&RetaskableBug>, links: &Links) {
        let Some(bug) = bug else { return };
        let Some((link, version)) = self.bugs.clone() else {
            tracing::info!(
                "{}: bug report not sent: the project isn't connected to retaskable.com",
                self.id
            );
            return;
        };
        let Some(record) = self.records.get(item_id) else {
            return;
        };
        let words = scrub_words(&self.id, &self.project, item_id, record.branch.as_deref(), &links.env);
        let Some(message) = decide_bug_report(
            Some(bug),
            record.needs_human_reason.as_deref(),
            &words,
            link.consent(),
            record.reported_bugs.last().map(String::as_str),
            &link.known(),
        ) else {
            tracing::info!(
                "{}: bug report for {item_id} not sent: off, empty or already reported",
                self.id
            );
            return;
        };
        if let Some(record) = self.records.get_mut(item_id) {
            record.reported_bugs.push(message.signature.clone());
        }
        self.save_leases();
        let (tx, item) = (self.bug_answers.tx.clone(), item_id.to_string());
        tokio::spawn(async move {
            let outcome = link.send(&message, &version).await;
            let _ = tx.send((item, outcome));
        });
    }

    /// Adds the timeline line for each answer that came back since the last pass.
    pub(super) fn bug_answers(&mut self) {
        while let Ok((item_id, outcome)) = self.bug_answers.rx.try_recv() {
            let BugOutcome::Answered(a) = outcome else { continue };
            if !a.ok || !self.records.get(&item_id).is_some_and(|r| r.state == "needs_human") {
                continue;
            }
            let line = if a.duplicate {
                "Retaskable already has this bug on its board"
            } else {
                "Reported to Retaskable as a fix"
            };
            self.look_note(&item_id, line.into());
        }
    }
}
