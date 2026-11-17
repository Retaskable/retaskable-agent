//! What a leased item's chain after coding tells the store and keeps (the TypeScript agent's `HostedWork.changed`
//! and the end of each step in `steps.ts`, `review.ts`, `merge.ts`): a step's end is applied to the item's run and
//! record, its notes join the item's history, a send-back is queued for the implementer's session, and the store
//! hears the item's status, state, branch and change whenever one of them moved.
use std::collections::{BTreeMap, BTreeSet};

use super::Links;
use super::project::ProjectLoop;
use crate::backlog::progress::Progress;
use crate::item_lifecycle::ItemState;
use crate::orchestrator::steps::{StepDone, state_name};
use crate::telemetry::{Event, event};

/// What the store was last told of each leased item, and the reports it has not taken (sent again on later passes,
/// as the TS agent's `sent` and `unsent` are).
#[derive(Debug, Default)]
pub(super) struct Told {
    sent: BTreeMap<String, Progress>,
    unsent: BTreeSet<String>,
}

/// `reportStatus`: what an item in `state` is, to the store. `None` for the states `complete` and `release` say.
pub(super) fn report_status(state: ItemState) -> Option<&'static str> {
    use ItemState::*;
    match state {
        Queued => Some("claimed"),
        Implementing => Some("building"),
        AwaitingCi | Checking | Reviewing => Some("in_review"),
        Merging | AwaitingMerge => Some("merging"),
        NeedsClarification | AwaitingPlaytest | NeedsHuman => Some("needs_person"),
        Merged | Done => None,
    }
}

impl ProjectLoop {
    /// Applies a step's end to the item: its flow record, the notes for its history, the send-back for its
    /// session, and the state it moved to.
    pub(super) fn apply_step(&mut self, item: &str, before: ItemState, done: StepDone) {
        let StepDone {
            run,
            notes,
            flow,
            send_back,
            ..
        } = done;
        if let Some(record) = self.records.get_mut(item) {
            if let Some(flow) = flow {
                record.flow = flow;
            }
            record.flow.keep_counters(&run);
            if let Some(text) = &send_back {
                record.flow.follow_up = Some(text.clone());
            }
        }
        self.sync_tryouts(item);
        for note in notes {
            self.events.add_in(
                &self.id,
                Event::ItemState(event::ItemState {
                    item: Some(item.to_string()),
                    from: Some(state_name(before).into()),
                    to: Some(state_name(run.state).into()),
                    note: Some(note),
                    ..Default::default()
                }),
            );
        }
        if send_back.is_some() && run.state == ItemState::Implementing {
            self.to_resume.push(item.to_string());
        }
    }

    /// Keeps the item's try-out details (`playtest`, `pendingPlaytest`) as its record says, to go to the store with the next
    /// `details_report`. An item whose details were not read (no lease: a synthetic chore) has none and nothing is written.
    pub(super) fn sync_tryouts(&mut self, item: &str) {
        let (Some(record), Some(held)) = (self.records.get(item), self.decisions.get_mut(item)) else {
            return;
        };
        held.now
            .set_tryouts(record.flow.playtest.as_ref(), record.flow.pending_playtest.as_ref());
    }

    /// Whether a step asked not to be run again yet (`flow.retry_after_ms`: a review at a usage limit, a merge the host
    /// isn't ready for).
    pub(super) fn waiting_to_retry(&self, item: &str) -> bool {
        let until = self.records.get(item).and_then(|r| r.flow.retry_after_ms);
        until.is_some_and(|t| t > self.now_secs() * 1000)
    }

    /// What the store should now hear of `item`, if it is leased here and in a state the store is told of.
    fn progress_of(&self, item: &str) -> Option<Progress> {
        let (run, record) = (self.runs.get(item)?, self.records.get(item));
        let status = report_status(run.state)?;
        let needs_person = status == "needs_person";
        let why = record
            .and_then(|r| r.needs_human_reason.clone())
            .filter(|_| needs_person && run.state == ItemState::NeedsHuman);
        let pr_url = record.and_then(|r| r.flow.pr_url.clone());
        Some(Progress {
            status: status.into(),
            detail: state_name(run.state).into(),
            branch: record.and_then(|r| r.branch.clone()).unwrap_or_default(),
            pr_url: pr_url
                .or_else(|| self.items.get(item).and_then(|i| i.pr_url.clone()))
                .unwrap_or_default(),
            claude_on_it: why.as_ref().map(|_| self.claude_on_it(item)),
            note: why,
        })
    }

    /// Tells the store `item`'s progress if it moved since the last report (a state, the branch or the change).
    /// A refusal keeps it to send again ([`Self::resend_progress`]); `stale_lease` means another computer has it.
    pub(super) async fn report_progress(&mut self, links: &Links, item: &str) {
        let (Some(repo), Some(lease), Some(progress)) =
            (self.repo, self.leases.get(item).cloned(), self.progress_of(item))
        else {
            return;
        };
        if self.told.sent.get(item) == Some(&progress) {
            return;
        }
        let Ok(Some(token)) = links.store_token().await else {
            self.told.unsent.insert(item.to_string());
            return;
        };
        match self
            .client
            .report_progress(repo, Some(&token), item, &lease, &progress)
            .await
        {
            Ok(()) => {
                self.told.unsent.remove(item);
                self.told.sent.insert(item.to_string(), progress);
            }
            Err(e) if e.code() == Some("stale_lease") => {
                self.told.unsent.remove(item);
            }
            Err(e) => {
                self.store_error("report", &e);
                self.told.unsent.insert(item.to_string());
            }
        }
    }

    /// Sends again the progress reports the store did not take.
    pub(super) async fn resend_progress(&mut self, links: &Links) {
        for item in std::mem::take(&mut self.told.unsent) {
            self.report_progress(links, &item).await;
        }
    }

    /// Forgets what was told of `item` (it was given back, parked or finished here).
    pub(super) fn forget_progress(&mut self, item: &str) {
        self.told.sent.remove(item);
        self.told.unsent.remove(item);
    }
}
