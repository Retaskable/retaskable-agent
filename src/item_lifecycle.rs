//! The states a single item's active slice moves through, and the rule that decides the next one
//! from each step's outcome (docs/specs/agent/05-item-lifecycle.md). The TypeScript agent folds this
//! decision into each step itself (`orchestrator.ts`'s `steps.checkStep`, `reviewStep`, ...); this
//! module keeps it separate and pure, so the orchestrator loop that will run the real steps and call
//! it (section 7, not ported yet) can be built, and this can be tested, on its own.
//!
//! What's deliberately left out: the cross-item merge queue, batch merges and GitHub-specific landing
//! mechanics inside `merging` (github-workflow.md, team-branch.md — neither ported here yet). This
//! module only tracks what the *item's own* state becomes once that step reports an outcome.
use crate::config::{Playtests, ProjectConfig};

/// Section 5's diagram, one variant per box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    /// The store handed the item out; the agent decides none of the ordering.
    Queued,
    /// Claude codes the next slice in the worktree.
    Implementing,
    /// The session exited with questions, waiting for a person to answer them.
    NeedsClarification,
    /// Waiting for the CI run beside the checks and review (only reached when `requireCi` is on).
    AwaitingCi,
    /// Running the repo's own deterministic checks in the lane.
    Checking,
    /// Scouts, then the Opus reviewer, look at the diff.
    Reviewing,
    /// The approved slice waits for a person to play it.
    AwaitingPlaytest,
    /// The PR is open and approved; GitHub or a person hasn't merged it yet.
    AwaitingMerge,
    /// Landing the PR.
    Merging,
    /// This slice merged; the item's backlog status decides whether the whole item is done.
    Merged,
    /// Nothing in the item's backlog is left.
    Done,
    /// Needs a human: a budget hit, too many rounds, a reviewer escalation, or an error.
    NeedsHuman,
}

/// One item's progress through its current slice: its state plus the counters the round caps
/// read. Check rounds and review rounds are counted separately ("check rounds don't use up
/// review rounds"); conflict rounds have their own cap and count both kinds of base update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemRun {
    pub state: ItemState,
    pub check_rounds: u32,
    pub review_rounds: u32,
    pub conflict_rounds: u32,
    /// Whether the review approval still matches the branch's head (`approvedSha`). A conflict
    /// update or a failed playtest drops it, so the slice is checked or reviewed again rather
    /// than merged unchanged.
    pub approved_at_head: bool,
}

impl Default for ItemRun {
    fn default() -> Self {
        Self::new()
    }
}

impl ItemRun {
    pub fn new() -> Self {
        Self {
            state: ItemState::Queued,
            check_rounds: 0,
            review_rounds: 0,
            conflict_rounds: 0,
            approved_at_head: false,
        }
    }

    /// `queued` -> `implementing`: the store handed the item out.
    pub fn claimed(&mut self) {
        self.state = ItemState::Implementing;
    }

    /// The implementer session exited.
    pub fn implementer_finished(&mut self, outcome: ImplementerOutcome, cfg: &ProjectConfig) {
        self.state = match outcome {
            ImplementerOutcome::Questions => ItemState::NeedsClarification,
            ImplementerOutcome::MainSync => awaiting_ci_entry(cfg),
            ImplementerOutcome::Finished => {
                if cfg.require_ci && cfg.ci_when_ready {
                    ItemState::Checking
                } else {
                    awaiting_ci_entry(cfg)
                }
            }
        };
    }

    /// A person answered the session's questions; the same session resumes.
    pub fn clarification_answered(&mut self) {
        self.state = ItemState::Implementing;
    }

    /// The CI run `awaiting_ci` was watching finished (or never started within the grace, or
    /// doesn't cover the base at all).
    pub fn ci_finished(&mut self, outcome: CiOutcome, cfg: &ProjectConfig) {
        self.state = match outcome {
            CiOutcome::Passed => {
                if self.approved_at_head {
                    ItemState::Merging
                } else if cfg.ci_covers_checks {
                    ItemState::Reviewing
                } else {
                    ItemState::Checking
                }
            }
            CiOutcome::NoChecksRan => ItemState::Checking,
            CiOutcome::Failed => self.fail_check(cfg),
            CiOutcome::TimedOut => ItemState::NeedsHuman,
        };
    }

    /// The repo's own checks, run in the lane, finished.
    pub fn checks_finished(&mut self, outcome: CheckOutcome, cfg: &ProjectConfig) {
        self.state = match outcome {
            CheckOutcome::Passed => ItemState::Reviewing,
            CheckOutcome::Failed => self.fail_check(cfg),
        };
    }

    fn fail_check(&mut self, cfg: &ProjectConfig) -> ItemState {
        self.check_rounds += 1;
        if self.check_rounds > cfg.max_check_rounds {
            ItemState::NeedsHuman
        } else {
            ItemState::Implementing
        }
    }

    /// Scouts and the reviewer finished looking at the diff.
    pub fn review_finished(&mut self, outcome: ReviewOutcome, cfg: &ProjectConfig) {
        self.state = match outcome {
            ReviewOutcome::Approved => {
                self.approved_at_head = true;
                if cfg.playtests == Playtests::On {
                    ItemState::AwaitingPlaytest
                } else {
                    ItemState::Merging
                }
            }
            ReviewOutcome::ChangesRequested => {
                self.approved_at_head = false;
                self.review_rounds += 1;
                if self.review_rounds > cfg.max_review_rounds {
                    ItemState::NeedsHuman
                } else {
                    ItemState::Implementing
                }
            }
            ReviewOutcome::Escalated => ItemState::NeedsHuman,
        };
    }

    /// A person played the approved build.
    pub fn playtest_finished(&mut self, outcome: PlaytestOutcome, cfg: &ProjectConfig) {
        self.state = match outcome {
            PlaytestOutcome::Passed => ItemState::Merging,
            PlaytestOutcome::Failed => {
                self.approved_at_head = false;
                self.review_rounds += 1;
                if self.review_rounds > cfg.max_review_rounds {
                    ItemState::NeedsHuman
                } else {
                    ItemState::Implementing
                }
            }
        };
    }

    /// Landing the PR finished, one way or another.
    pub fn merge_finished(&mut self, outcome: MergeOutcome, cfg: &ProjectConfig) {
        self.state = match outcome {
            MergeOutcome::Merged => ItemState::Merged,
            MergeOutcome::HandedToGithub => ItemState::AwaitingMerge,
            MergeOutcome::Conflict => {
                self.approved_at_head = false;
                self.conflict_rounds += 1;
                if self.conflict_rounds > cfg.max_conflict_rounds {
                    ItemState::NeedsHuman
                } else {
                    ItemState::Implementing
                }
            }
            MergeOutcome::Failed => ItemState::NeedsHuman,
        };
    }

    /// GitHub or a person finally landed (or closed) the PR that `merging` handed off.
    pub fn awaiting_merge_finished(&mut self, outcome: AwaitingMergeOutcome) {
        self.state = match outcome {
            AwaitingMergeOutcome::Merged => ItemState::Merged,
            AwaitingMergeOutcome::ClosedWithoutMerging => ItemState::NeedsHuman,
        };
    }

    /// The backlog store said whether anything is left in the item once this slice merged.
    pub fn merged_resolved(&mut self, item_done: bool) {
        self.state = if item_done { ItemState::Done } else { ItemState::Queued };
    }

    /// Any state can go to `needs_human`: a budget hit, max rounds, max slices, a reviewer
    /// escalation, or an error.
    pub fn escalate(&mut self) {
        self.state = ItemState::NeedsHuman;
    }

    /// A human chose **Carry on** or **Start this slice over**: either way it resumes in
    /// `implementing` with its review, check and conflict rounds starting again.
    pub fn resume_from_needs_human(&mut self) {
        self.check_rounds = 0;
        self.review_rounds = 0;
        self.conflict_rounds = 0;
        self.approved_at_head = false;
        self.state = ItemState::Implementing;
    }
}

/// `awaiting_ci`'s own first rule: skip straight to `checking` when there's no CI to wait for.
fn awaiting_ci_entry(cfg: &ProjectConfig) -> ItemState {
    if cfg.require_ci {
        ItemState::AwaitingCi
    } else {
        ItemState::Checking
    }
}

/// How the implementer session exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImplementerOutcome {
    /// The slice is coded and committed.
    Finished,
    /// It brought the branch up to date with the base (a main sync), which always waits for CI.
    MainSync,
    /// It exited with questions for a person.
    Questions,
}

/// How the CI run `awaiting_ci` was watching finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiOutcome {
    Passed,
    Failed,
    /// Nothing showed up within the grace, or CI doesn't cover the base branch at all.
    NoChecksRan,
    /// Still pending past the project's limit.
    TimedOut,
}

/// How the repo's own checks, run in the lane, finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    Passed,
    Failed,
}

/// How review finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewOutcome {
    Approved,
    ChangesRequested,
    /// The reviewer sent it straight to a human instead of asking for changes.
    Escalated,
}

/// How a person's playtest of the approved build went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaytestOutcome {
    Passed,
    Failed,
}

/// How landing the PR finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeOutcome {
    /// Squash-merged into the base branch.
    Merged,
    /// Handed to GitHub's auto-merge or a person; waits in `awaiting_merge`.
    HandedToGithub,
    /// Couldn't bring the branch up to date with the base, or the merge failed mechanically.
    Conflict,
    /// Some other failure (e.g. the code host didn't answer after repeated retries).
    Failed,
}

/// How the PR `merging` handed off was finally resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AwaitingMergeOutcome {
    Merged,
    ClosedWithoutMerging,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A baseline config with real round caps (the derived `Default` leaves those at 0, which
    /// would escalate to `needs_human` on the very first failure).
    fn cfg() -> ProjectConfig {
        ProjectConfig {
            max_check_rounds: 3,
            max_review_rounds: 3,
            max_conflict_rounds: 5,
            ..Default::default()
        }
    }

    #[test]
    fn claim_moves_queued_to_implementing() {
        let mut run = ItemRun::new();
        assert_eq!(run.state, ItemState::Queued);
        run.claimed();
        assert_eq!(run.state, ItemState::Implementing);
    }

    #[test]
    fn questions_go_to_needs_clarification_and_an_answer_resumes_implementing() {
        let mut run = ItemRun::new();
        run.implementer_finished(ImplementerOutcome::Questions, &cfg());
        assert_eq!(run.state, ItemState::NeedsClarification);
        run.clarification_answered();
        assert_eq!(run.state, ItemState::Implementing);
    }

    #[test]
    fn finishing_with_require_ci_off_skips_straight_to_checking() {
        let mut run = ItemRun::new();
        let cfg = ProjectConfig {
            require_ci: false,
            ..cfg()
        };
        run.implementer_finished(ImplementerOutcome::Finished, &cfg);
        assert_eq!(run.state, ItemState::Checking);
    }

    #[test]
    fn finishing_with_require_ci_on_and_ci_when_ready_off_waits_in_awaiting_ci() {
        let mut run = ItemRun::new();
        let cfg = ProjectConfig {
            require_ci: true,
            ci_when_ready: false,
            ..cfg()
        };
        run.implementer_finished(ImplementerOutcome::Finished, &cfg);
        assert_eq!(run.state, ItemState::AwaitingCi);
    }

    #[test]
    fn finishing_with_ci_when_ready_on_goes_straight_to_checking_but_a_main_sync_still_waits() {
        let cfg = ProjectConfig {
            require_ci: true,
            ci_when_ready: true,
            ..cfg()
        };
        let mut run = ItemRun::new();
        run.implementer_finished(ImplementerOutcome::Finished, &cfg);
        assert_eq!(run.state, ItemState::Checking);

        let mut main_sync = ItemRun::new();
        main_sync.implementer_finished(ImplementerOutcome::MainSync, &cfg);
        assert_eq!(main_sync.state, ItemState::AwaitingCi);
    }

    #[test]
    fn ci_passing_already_approved_at_head_goes_straight_to_merging() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingCi;
        run.approved_at_head = true;
        run.ci_finished(CiOutcome::Passed, &cfg());
        assert_eq!(run.state, ItemState::Merging);
    }

    #[test]
    fn ci_passing_with_ci_covers_checks_skips_the_lane_and_goes_to_reviewing() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingCi;
        let cfg = ProjectConfig {
            ci_covers_checks: true,
            ..cfg()
        };
        run.ci_finished(CiOutcome::Passed, &cfg);
        assert_eq!(run.state, ItemState::Reviewing);
    }

    #[test]
    fn ci_passing_otherwise_carries_on_to_checking() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingCi;
        run.ci_finished(CiOutcome::Passed, &cfg());
        assert_eq!(run.state, ItemState::Checking);
    }

    #[test]
    fn no_checks_ran_carries_on_to_checking_even_with_ci_covers_checks_on() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingCi;
        let cfg = ProjectConfig {
            ci_covers_checks: true,
            ..cfg()
        };
        run.ci_finished(CiOutcome::NoChecksRan, &cfg);
        assert_eq!(run.state, ItemState::Checking);
    }

    #[test]
    fn ci_timing_out_goes_straight_to_needs_human() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingCi;
        run.ci_finished(CiOutcome::TimedOut, &cfg());
        assert_eq!(run.state, ItemState::NeedsHuman);
    }

    #[test]
    fn a_ci_or_check_failure_sends_it_back_to_implementing_and_counts_a_check_round() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingCi;
        run.ci_finished(CiOutcome::Failed, &cfg());
        assert_eq!(run.state, ItemState::Implementing);
        assert_eq!(run.check_rounds, 1);

        run.state = ItemState::Checking;
        run.checks_finished(CheckOutcome::Failed, &cfg());
        assert_eq!(run.state, ItemState::Implementing);
        assert_eq!(run.check_rounds, 2);
    }

    #[test]
    fn check_rounds_cap_sends_it_to_needs_human_instead_of_looping_forever() {
        let mut run = ItemRun::new();
        let cfg = ProjectConfig {
            max_check_rounds: 2,
            ..cfg()
        };
        run.state = ItemState::Checking;
        run.checks_finished(CheckOutcome::Failed, &cfg);
        assert_eq!(run.state, ItemState::Implementing);
        run.state = ItemState::Checking;
        run.checks_finished(CheckOutcome::Failed, &cfg);
        assert_eq!(run.state, ItemState::Implementing);
        run.state = ItemState::Checking;
        run.checks_finished(CheckOutcome::Failed, &cfg);
        assert_eq!(run.state, ItemState::NeedsHuman);
        assert_eq!(run.check_rounds, 3);
    }

    #[test]
    fn checking_passing_goes_to_reviewing() {
        let mut run = ItemRun::new();
        run.state = ItemState::Checking;
        run.checks_finished(CheckOutcome::Passed, &cfg());
        assert_eq!(run.state, ItemState::Reviewing);
    }

    #[test]
    fn check_rounds_and_review_rounds_are_counted_separately() {
        let mut run = ItemRun::new();
        run.state = ItemState::Checking;
        run.checks_finished(CheckOutcome::Failed, &cfg());
        assert_eq!(run.check_rounds, 1);
        assert_eq!(run.review_rounds, 0);

        run.state = ItemState::Reviewing;
        run.review_finished(ReviewOutcome::ChangesRequested, &cfg());
        assert_eq!(run.check_rounds, 1);
        assert_eq!(run.review_rounds, 1);
    }

    #[test]
    fn approval_without_playtests_on_goes_straight_to_merging() {
        let mut run = ItemRun::new();
        run.state = ItemState::Reviewing;
        run.review_finished(ReviewOutcome::Approved, &cfg());
        assert_eq!(run.state, ItemState::Merging);
        assert!(run.approved_at_head);
    }

    #[test]
    fn approval_with_playtests_on_waits_for_a_playtest_first() {
        let mut run = ItemRun::new();
        run.state = ItemState::Reviewing;
        let cfg = ProjectConfig {
            playtests: Playtests::On,
            ..cfg()
        };
        run.review_finished(ReviewOutcome::Approved, &cfg);
        assert_eq!(run.state, ItemState::AwaitingPlaytest);
    }

    #[test]
    fn changes_requested_drops_the_approval_and_goes_back_to_implementing() {
        let mut run = ItemRun::new();
        run.state = ItemState::Reviewing;
        run.approved_at_head = true;
        run.review_finished(ReviewOutcome::ChangesRequested, &cfg());
        assert_eq!(run.state, ItemState::Implementing);
        assert!(!run.approved_at_head);
        assert_eq!(run.review_rounds, 1);
    }

    #[test]
    fn review_rounds_cap_escalates_to_needs_human() {
        let cfg = ProjectConfig {
            max_review_rounds: 1,
            ..cfg()
        };
        let mut run = ItemRun::new();
        run.state = ItemState::Reviewing;
        run.review_finished(ReviewOutcome::ChangesRequested, &cfg);
        assert_eq!(run.state, ItemState::Implementing);
        run.state = ItemState::Reviewing;
        run.review_finished(ReviewOutcome::ChangesRequested, &cfg);
        assert_eq!(run.state, ItemState::NeedsHuman);
    }

    #[test]
    fn an_escalated_review_goes_straight_to_needs_human() {
        let mut run = ItemRun::new();
        run.state = ItemState::Reviewing;
        run.review_finished(ReviewOutcome::Escalated, &cfg());
        assert_eq!(run.state, ItemState::NeedsHuman);
    }

    #[test]
    fn a_passed_playtest_goes_to_merging() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingPlaytest;
        run.playtest_finished(PlaytestOutcome::Passed, &cfg());
        assert_eq!(run.state, ItemState::Merging);
    }

    #[test]
    fn a_failed_playtest_drops_the_approval_and_resumes_implementing_counting_a_review_round() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingPlaytest;
        run.approved_at_head = true;
        run.playtest_finished(PlaytestOutcome::Failed, &cfg());
        assert_eq!(run.state, ItemState::Implementing);
        assert!(!run.approved_at_head);
        assert_eq!(run.review_rounds, 1);
    }

    #[test]
    fn a_merge_lands_the_item_in_merged() {
        let mut run = ItemRun::new();
        run.state = ItemState::Merging;
        run.merge_finished(MergeOutcome::Merged, &cfg());
        assert_eq!(run.state, ItemState::Merged);
    }

    #[test]
    fn handing_off_to_github_waits_in_awaiting_merge_and_then_resolves() {
        let mut run = ItemRun::new();
        run.state = ItemState::Merging;
        run.merge_finished(MergeOutcome::HandedToGithub, &cfg());
        assert_eq!(run.state, ItemState::AwaitingMerge);
        run.awaiting_merge_finished(AwaitingMergeOutcome::Merged);
        assert_eq!(run.state, ItemState::Merged);
    }

    #[test]
    fn a_pr_closed_without_merging_needs_a_human() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingMerge;
        run.awaiting_merge_finished(AwaitingMergeOutcome::ClosedWithoutMerging);
        assert_eq!(run.state, ItemState::NeedsHuman);
    }

    #[test]
    fn a_conflict_drops_the_approval_and_resumes_implementing_counting_its_own_round() {
        let mut run = ItemRun::new();
        run.state = ItemState::Merging;
        run.approved_at_head = true;
        run.merge_finished(MergeOutcome::Conflict, &cfg());
        assert_eq!(run.state, ItemState::Implementing);
        assert!(!run.approved_at_head);
        assert_eq!(run.conflict_rounds, 1);
    }

    #[test]
    fn conflict_rounds_cap_escalates_to_needs_human() {
        let cfg = ProjectConfig {
            max_conflict_rounds: 1,
            ..cfg()
        };
        let mut run = ItemRun::new();
        run.state = ItemState::Merging;
        run.merge_finished(MergeOutcome::Conflict, &cfg);
        assert_eq!(run.state, ItemState::Implementing);
        run.state = ItemState::Merging;
        run.merge_finished(MergeOutcome::Conflict, &cfg);
        assert_eq!(run.state, ItemState::NeedsHuman);
    }

    #[test]
    fn a_merge_failure_other_than_a_conflict_needs_a_human() {
        let mut run = ItemRun::new();
        run.state = ItemState::Merging;
        run.merge_finished(MergeOutcome::Failed, &cfg());
        assert_eq!(run.state, ItemState::NeedsHuman);
    }

    #[test]
    fn merged_resolves_to_done_or_back_to_queued_from_the_backlog_itemdone_flag() {
        let mut run = ItemRun::new();
        run.state = ItemState::Merged;
        run.merged_resolved(true);
        assert_eq!(run.state, ItemState::Done);

        let mut run = ItemRun::new();
        run.state = ItemState::Merged;
        run.merged_resolved(false);
        assert_eq!(run.state, ItemState::Queued);
    }

    #[test]
    fn any_state_can_escalate_to_needs_human() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingPlaytest;
        run.escalate();
        assert_eq!(run.state, ItemState::NeedsHuman);
    }

    #[test]
    fn resuming_from_needs_human_goes_to_implementing_and_clears_every_round_counter() {
        let mut run = ItemRun::new();
        run.state = ItemState::NeedsHuman;
        run.check_rounds = 3;
        run.review_rounds = 2;
        run.conflict_rounds = 1;
        run.approved_at_head = true;
        run.resume_from_needs_human();
        assert_eq!(run.state, ItemState::Implementing);
        assert_eq!(run.check_rounds, 0);
        assert_eq!(run.review_rounds, 0);
        assert_eq!(run.conflict_rounds, 0);
        assert!(!run.approved_at_head);
    }
}
