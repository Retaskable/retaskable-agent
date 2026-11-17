//! A draft's last word, `merged` or `discarded`, kept in a file of the project's own until the store answers (the
//! owner's outage rule, 2026-11-16: "it should be able to finish what it's doing but it can't pick up the next thing
//! till the store is back."). The change is already made, so the store must hear of it, also after a restart in the
//! middle of an outage: the report is listed in `draft_report.pending.json` in the project's folder before it is first
//! sent and removed when the store answers. Draft tasks run on their own and cannot write the state file (the loop is
//! its one writer), so the file is the neutral [`OwedFile`], which the tasks and the loop share.
//!
//! The store takes the report from the holder with the draft's lease and from the holder a draft moved from
//! (`lapsedDraft`, backlog/06.3) while nobody has taken it since, both under the lease token the claim handed out and
//! this computer's own link, so the file holds the token and the report and nothing else.
//!
//! What each answer means: taken is done; a refusal that names a code, below 500 (`stale_lease`: the draft moved on), is
//! dropped; anything else (unreachable, a 5xx, an answer this agent can't read) is kept and sent again every `wait`, for
//! 24 hours counted from when it was first listed (so a restart doesn't give it a fresh day). An improvement over the TS
//! agent (`draftMove.ts`), which sends once and ignores a failure.
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::Shared;
use super::project::ProjectLoop;
use crate::backlog::drafts::DraftReport;
use crate::backlog::failures::Failures;
use crate::backlog::{BacklogError, Client};
use crate::owed_file::{KEPT_SECS, OwedFile, now_secs};

/// The file in the project's folder.
pub(super) const FILE: &str = "draft_report.pending.json";
/// The wait between sends of a report the store didn't take.
pub(super) const RETRY: Duration = Duration::from_secs(5);

/// A final report owed to the store, by draft id: the lease it is sent under, what it says, and when it was first listed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct Owed {
    pub lease: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Epoch seconds.
    pub since: i64,
    /// A merged plan's revision of its item (`revise.rs`), carried here so that it survives every crash point: the
    /// entry stays on disk after the store takes the report until the loop has listed the revision in the state file
    /// (`ProjectLoop::list_kept_revisions`), which is the state file's one writer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<crate::backlog::revise::Owed>,
    /// The store took the report; only the revision is still to hand over.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reported: bool,
}

impl Owed {
    fn report(&self) -> DraftReport {
        DraftReport {
            status: Some(self.status.clone()),
            detail: self.detail.clone(),
            summary: self.summary.clone(),
            ..Default::default()
        }
    }
}

pub(super) type Kept = OwedFile<Owed>;

pub(super) fn open(dir: &Path) -> Kept {
    OwedFile::open(dir.join(FILE))
}

/// What sending a final report needs: the store, the link and where the report is kept.
pub(super) struct Sender<'a> {
    pub client: &'a Client,
    pub repo: u64,
    pub token: &'a str,
    pub failures: &'a Failures,
    pub kept: &'a Kept,
    pub wait: Duration,
}

impl Sender<'_> {
    /// Lists the report (keeping the time it was first listed) and sends it until the store answers. `Err` is a refusal
    /// that names a code, or the 24 hours up: the report is dropped.
    pub(super) async fn send(
        &self,
        id: &str,
        lease: &str,
        report: &DraftReport,
        revision: Option<crate::backlog::revise::Owed>,
    ) -> Result<(), BacklogError> {
        let since = self.kept.get(id).map_or_else(now_secs, |o| o.since);
        self.kept.put(
            id,
            Owed {
                lease: lease.into(),
                status: report.status.clone().unwrap_or_default(),
                detail: report.detail.clone(),
                summary: report.summary.clone(),
                since,
                revision,
                reported: false,
            },
        );
        self.resend(id).await
    }

    /// Sends the report listed for `id` until the store answers (nothing when it was answered already).
    async fn resend(&self, id: &str) -> Result<(), BacklogError> {
        let Some(owed) = self.kept.get(id) else {
            return Ok(());
        };
        if owed.reported {
            return Ok(());
        }
        let report = owed.report();
        let mut waited = Duration::from_secs(now_secs().saturating_sub(owed.since).max(0) as u64);
        let result = loop {
            let e = match (self.client)
                .draft_report(self.repo, Some(self.token), id, &owed.lease, &report)
                .await
            {
                Ok(()) => break Ok(()),
                Err(e) => e,
            };
            self.failures.add("draft_report", id, &e);
            if e.is_refusal() || waited.as_secs() as i64 >= KEPT_SECS {
                tracing::warn!("a draft's final report was dropped: {e}");
                break Err(e);
            }
            tracing::warn!("a draft's final report didn't reach the store, keeping it: {e}");
            tokio::time::sleep(self.wait).await;
            waited += self.wait.max(Duration::from_millis(1));
        };
        // Taken: a revision still to hand over keeps the entry (marked), the loop removes it once listed. Dropped (the draft
        // moved on, or a day passed): the plan's end was another computer's, and so is its revision.
        match (&result, owed.revision.is_some()) {
            (Ok(()), true) => self.kept.put(id, Owed { reported: true, ..owed }),
            _ => self.kept.remove(id),
        }
        result
    }
}

impl ProjectLoop {
    /// After a start, sends again each final report kept from before, on tasks of their own; a new draft isn't claimed
    /// until each is answered ([`Self::final_reports_owed`]). Does nothing while stopping or without the store's link.
    pub(super) async fn resume_final_reports(&mut self, shared: &Shared<'_>) {
        if self.owed_resume.is_empty() || shared.stopping {
            return;
        }
        let Some(repo) = self.repo else { return };
        let token = match shared.links.store_token().await {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        let wait = self.owed_wait;
        for id in std::mem::take(&mut self.owed_resume) {
            let (client, failures, kept) = (self.client.clone(), self.failures.clone(), self.drafts_owed.clone());
            let token = token.clone();
            self.owed_sends.push(tokio::spawn(async move {
                let sender = Sender {
                    client: &client,
                    repo,
                    token: &token,
                    failures: &failures,
                    kept: &kept,
                    wait,
                };
                let _ = sender.resend(&id).await;
            }));
        }
    }

    /// Whether a final report kept from before a start is still to be sent: nothing new is taken meanwhile.
    pub(super) fn final_reports_owed(&mut self) -> bool {
        self.owed_sends.retain(|t| !t.is_finished());
        !self.owed_resume.is_empty() || !self.owed_sends.is_empty()
    }
}
