//! What an item's chain after coding keeps beside its run (`RunRecord::flow`): the change it opened, the rounds
//! counted so far, what the steps learned (the checks' evidence, the head a review approved) and the message the
//! implementer's session is sent back with. The TypeScript agent keeps these on its item record (`prUrl`,
//! `prNumber`, `checkRounds`, `reviewRound`, `conflictRounds`, `approvedSha`, `followUp`); the store hears the
//! change's address and the status through `report`, and the rest lives here so a restart carries on mid-chain.
use serde::{Deserialize, Serialize};

use crate::item_lifecycle::ItemRun;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowRecord {
    /// The message the implementer's session is resumed with: a failed check, a review's requests, CI's red, a
    /// conflict. Taken when the session starts again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<String>,
    /// The change opened for the slice, once a step found it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    /// `ItemRun`'s counters and approval, kept so a restart doesn't give a stuck item its rounds back.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub check_rounds: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub review_rounds: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub conflict_rounds: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approved_at_head: bool,
    /// The commit the last review read and the one it approved (`reviewedSha`, `approvedSha`), what that review asked
    /// for, and the commit a conflict was sent back at (`conflictOf`): a later head the approval still covers needs no
    /// new review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_review: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_of: Option<String>,
    /// The try-out a person is asked to do (`playtest`), once the review approved with one; and the one deferred until
    /// after the merge (`pendingPlaytest`). Why a diff held the change for a person's check (`held`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playtest: Option<TryOut>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_playtest: Option<TryOut>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<String>,
    /// A step that must wait (a review at a usage limit, a merge GitHub isn't ready for) is not run again before this
    /// time, in epoch milliseconds (`resumeAfter`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<i64>,
    /// The checks' evidence for the review: each check's mark, and the diff's stat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_stat: Option<String>,
    /// CI's side (`ciReadyAt`, `ciWaitSince`, `ciReruns`, `recheck`, `ciNote` in the TS record): when the `ci-ready` label
    /// went on and when the wait for CI's checks started (epoch ms), the runs of a cancelled CI started again, whether the
    /// checks run again once CI passes on a change brought up to date, and why CI was not waited for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_ready_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_wait_since: Option<i64>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ci_reruns: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recheck: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_note: Option<String>,
    /// The merge's side: who lands the change (`land`), whether GitHub's auto-merge is on (`autoMergeOn`), the merges GitHub
    /// wasn't ready for (`mergeRetries`) and since when it hasn't worked out whether the change merges (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub land: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_merge_on: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub merge_retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_since: Option<i64>,
}

/// What a person is asked to try out: what to look at, and the steps to get there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TryOut {
    pub what: String,
    #[serde(default)]
    pub steps: Vec<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl FlowRecord {
    /// Keeps `run`'s counters and approval.
    pub fn keep_counters(&mut self, run: &ItemRun) {
        self.check_rounds = run.check_rounds;
        self.review_rounds = run.review_rounds;
        self.conflict_rounds = run.conflict_rounds;
        self.approved_at_head = run.approved_at_head;
    }

    /// Puts the kept counters back on a run taken up again after a restart.
    pub fn restore_counters(&self, run: &mut ItemRun) {
        run.check_rounds = self.check_rounds;
        run.review_rounds = self.review_rounds;
        run.conflict_rounds = self.conflict_rounds;
        run.approved_at_head = self.approved_at_head;
    }
}
