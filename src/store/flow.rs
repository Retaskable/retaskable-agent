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
    /// The questions the session asked a person while the item waits in `needs_clarification` (`questions`), what an
    /// `answer` command's answers are paired with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<String>,
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
    /// after the merge (`pendingPlaytest`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playtest: Option<TryOut>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_playtest: Option<TryOut>,
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
    /// A read or call of the code host failed and was noted: the streak's first failure is noted and warned once, the
    /// next ones stay quiet (a long outage would add a note per poll), and the first success clears it with a note. Kept
    /// so a restart doesn't note the same streak again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_failing: bool,
}

/// What a person is asked to try out: what to look at, and the steps to get there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TryOut {
    pub what: String,
    #[serde(default)]
    pub steps: Vec<String>,
}

/// What a computer keeps of a try-out it sent for a merged slice, so a problem found with it goes back to what built it
/// (`tryoutsBuilt`: `{ key, sessionId, lane }`). The most recent 200.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltTryout {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<u32>,
}

impl TryOut {
    /// The try-out as the store takes it (`shared/itemDetails.ts`: `what` up to 2000 characters, 50 steps of up to 2000).
    pub fn fitted(&self) -> TryOut {
        TryOut {
            what: crate::text::truncate(&self.what, 2000),
            steps: (self.steps.iter())
                .take(50)
                .map(|s| crate::text::truncate(s, 2000))
                .collect(),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_record_keeps_nothing_in_state_json() {
        assert_eq!(serde_json::to_string(&FlowRecord::default()).unwrap(), "{}");
    }

    #[test]
    fn a_record_without_any_fields_reads_as_the_default() {
        assert_eq!(serde_json::from_str::<FlowRecord>("{}").unwrap(), FlowRecord::default());
    }

    #[test]
    fn fields_are_kept_under_camel_case_names_and_only_when_set() {
        let record = FlowRecord {
            pr_number: Some(7),
            check_rounds: 2,
            approved_at_head: true,
            retry_after_ms: Some(1_000),
            ..FlowRecord::default()
        };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "prNumber": 7, "checkRounds": 2, "approvedAtHead": true, "retryAfterMs": 1000 })
        );
        assert_eq!(serde_json::from_value::<FlowRecord>(json).unwrap(), record);
    }

    #[test]
    fn the_counters_survive_a_restart_but_nothing_else_of_the_run_moves() {
        let before = ItemRun {
            check_rounds: 3,
            review_rounds: 2,
            conflict_rounds: 1,
            approved_at_head: true,
            slice: 4,
            ..ItemRun::new()
        };
        let mut record = FlowRecord::default();
        record.keep_counters(&before);
        let record: FlowRecord = serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();

        let mut after = ItemRun::new();
        record.restore_counters(&mut after);
        assert_eq!(
            (
                after.check_rounds,
                after.review_rounds,
                after.conflict_rounds,
                after.approved_at_head
            ),
            (3, 2, 1, true)
        );
        assert_eq!(after.slice, 0);
    }

    #[test]
    fn keeping_a_fresh_run_clears_earlier_counters() {
        let mut record = FlowRecord {
            check_rounds: 5,
            approved_at_head: true,
            ..FlowRecord::default()
        };
        record.keep_counters(&ItemRun::new());
        assert_eq!((record.check_rounds, record.approved_at_head), (0, false));
    }

    #[test]
    fn a_tryout_is_fitted_to_the_stores_limits() {
        let tryout = TryOut {
            what: "w".repeat(2001),
            steps: (0..51).map(|_| "é".repeat(2001)).collect(),
        };
        let fitted = tryout.fitted();
        assert_eq!(fitted.what.chars().count(), 2000);
        assert_eq!(fitted.steps.len(), 50);
        assert!(fitted.steps.iter().all(|s| s.chars().count() == 2000));
    }

    #[test]
    fn a_tryout_within_the_limits_is_unchanged() {
        let tryout = TryOut {
            what: "Open the page".into(),
            steps: vec!["Launch".into(), "Click".into()],
        };
        assert_eq!(tryout.fitted(), tryout);
    }

    #[test]
    fn a_built_tryout_without_a_session_or_lane_keeps_only_its_key() {
        let built = BuiltTryout {
            key: "k".into(),
            session_id: None,
            lane: None,
        };
        let json = serde_json::to_string(&built).unwrap();
        assert_eq!(json, r#"{"key":"k"}"#);
        assert_eq!(serde_json::from_str::<BuiltTryout>(&json).unwrap(), built);
    }
}
