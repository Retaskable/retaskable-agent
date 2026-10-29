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

    /// `queued` -> `implementing`: the store handed the item out. Also takes a `merged` item
    /// (one not yet `done`) into its next slice, once the store hands that out too.
    pub fn claimed(&mut self) {
        self.state = ItemState::Implementing;
    }

    /// The implementer session exited.
    pub fn implementer_finished(&mut self, outcome: ImplementerOutcome, cfg: &ProjectConfig) {
        self.state = match outcome {
            ImplementerOutcome::Questions => ItemState::NeedsClarification,
            ImplementerOutcome::MainSync => awaiting_ci_entry(cfg),
            ImplementerOutcome::Finished => {
                if ci_when_ready_active(cfg) {
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
        let ci_when_ready = ci_when_ready_active(cfg);
        self.state = match outcome {
            CiOutcome::Passed => {
                if self.approved_at_head {
                    ItemState::Merging
                } else if ci_when_ready {
                    // "With ciWhenReady on, ciCoversChecks doesn't apply": the lane's checks
                    // still run even though CI just passed.
                    ItemState::Checking
                } else if cfg.ci_covers_checks {
                    ItemState::Reviewing
                } else {
                    ItemState::Checking
                }
            }
            CiOutcome::NoChecksRan => {
                // "An item approved at the PR's head goes to merging rather than back to
                // review" — but only in ciWhenReady mode; otherwise it just carries on to
                // checking (checks still need to run; nothing covered them).
                if self.approved_at_head && ci_when_ready {
                    ItemState::Merging
                } else {
                    ItemState::Checking
                }
            }
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
            ReviewOutcome::Approved { playtest } => {
                self.approved_at_head = true;
                // 06.3-review.md: "approve moves the item to awaiting_playtest if the
                // reviewer's result has a playtest, and to merging if it doesn't" — gated on
                // the project's playtests setting (06.4-playtests.md): "on" asks before
                // merging; "after_merge" and "off" never block the merge on one.
                if playtest && cfg.playtests == Playtests::On {
                    ItemState::AwaitingPlaytest
                } else {
                    merge_entry(cfg)
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
            PlaytestOutcome::Passed => merge_entry(cfg),
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
            MergeOutcome::UpdatedFromBase { approval_kept } => {
                // The base moved, so it's brought up to date and checked again before it
                // merges — unlike a real Conflict below, this merged in cleanly, so it goes
                // on to awaiting_ci rather than back to implementing. It still counts against
                // maxConflictRounds; the merge step checks `may_update_from_base` *before*
                // doing this, and lands the PR as it is instead once the cap is reached, so
                // this transition itself never needs to decide that.
                self.approved_at_head = approval_kept;
                self.conflict_rounds += 1;
                ItemState::AwaitingCi
            }
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
    /// Row 27: done moves it to `done`; otherwise "it stays `merged` here and is back in the
    /// backlog's queue for its next slice" — `claimed()` takes it from `merged` to
    /// `implementing` once the store hands that slice out.
    pub fn merged_resolved(&mut self, item_done: bool) {
        self.state = if item_done { ItemState::Done } else { ItemState::Merged };
    }

    /// Any state can go to `needs_human`: a budget hit, max rounds, max slices, a reviewer
    /// escalation, or an error.
    pub fn escalate(&mut self) {
        self.state = ItemState::NeedsHuman;
    }

    /// Whether the merge step may still bring the PR up to date with its base before landing
    /// it. Once `maxConflictRounds` is reached, the merge step checks this first and lands the
    /// PR as it is instead (reporting `Merged` or `HandedToGithub`, not `UpdatedFromBase`) —
    /// 05-item-lifecycle.md's `merging` row: "once the count is reached, a PR that is only
    /// behind merges as it is".
    pub fn may_update_from_base(&self, cfg: &ProjectConfig) -> bool {
        self.conflict_rounds < cfg.max_conflict_rounds
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

/// Whether CI only runs once a change is ready (`ciWhenReady`, which only means anything with
/// `requireCi` on too).
fn ci_when_ready_active(cfg: &ProjectConfig) -> bool {
    cfg.require_ci && cfg.ci_when_ready
}

/// Where an approved item (the review approved it, or its playtest passed) goes next. 06.3's
/// `ciWhenReady` rule: "wherever an approved item would go to `merging` ... it goes to
/// `awaiting_ci` instead, and CI passing on the approved commit merges it."
fn merge_entry(cfg: &ProjectConfig) -> ItemState {
    if ci_when_ready_active(cfg) {
        ItemState::AwaitingCi
    } else {
        ItemState::Merging
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
    /// `playtest`: whether the reviewer's result asks for one (06.3-review.md), which only
    /// sends it to `awaiting_playtest` when the project's `playtests` setting is `on`.
    Approved {
        playtest: bool,
    },
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
    /// The base moved since its checks ran, so it was merged in (cleanly, no conflict) and
    /// goes on to `awaiting_ci` to be checked again before it merges. `approval_kept`: whether
    /// the slice's own changes still read the same as at `approvedSha`.
    UpdatedFromBase { approval_kept: bool },
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
    fn ci_when_ready_ignores_ci_covers_checks_and_still_goes_to_checking() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingCi;
        let cfg = ProjectConfig {
            require_ci: true,
            ci_when_ready: true,
            ci_covers_checks: true,
            ..cfg()
        };
        run.ci_finished(CiOutcome::Passed, &cfg);
        assert_eq!(run.state, ItemState::Checking);
    }

    #[test]
    fn ci_when_ready_with_no_checks_ran_and_approved_at_head_goes_to_merging() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingCi;
        run.approved_at_head = true;
        let cfg = ProjectConfig {
            require_ci: true,
            ci_when_ready: true,
            ..cfg()
        };
        run.ci_finished(CiOutcome::NoChecksRan, &cfg);
        assert_eq!(run.state, ItemState::Merging);
    }

    #[test]
    fn ci_when_ready_with_no_checks_ran_and_not_yet_approved_goes_to_checking() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingCi;
        let cfg = ProjectConfig {
            require_ci: true,
            ci_when_ready: true,
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
        run.review_finished(ReviewOutcome::Approved { playtest: false }, &cfg());
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
        run.review_finished(ReviewOutcome::Approved { playtest: true }, &cfg);
        assert_eq!(run.state, ItemState::AwaitingPlaytest);
    }

    #[test]
    fn playtests_on_does_not_wait_when_the_reviewer_did_not_ask_for_one() {
        let mut run = ItemRun::new();
        run.state = ItemState::Reviewing;
        let cfg = ProjectConfig {
            playtests: Playtests::On,
            ..cfg()
        };
        run.review_finished(ReviewOutcome::Approved { playtest: false }, &cfg);
        assert_eq!(run.state, ItemState::Merging);
    }

    #[test]
    fn ci_when_ready_sends_an_approval_to_awaiting_ci_instead_of_straight_to_merging() {
        let mut run = ItemRun::new();
        run.state = ItemState::Reviewing;
        let cfg = ProjectConfig {
            require_ci: true,
            ci_when_ready: true,
            ..cfg()
        };
        run.review_finished(ReviewOutcome::Approved { playtest: false }, &cfg);
        assert_eq!(run.state, ItemState::AwaitingCi);
        assert!(run.approved_at_head);

        run.ci_finished(CiOutcome::Passed, &cfg);
        assert_eq!(run.state, ItemState::Merging);
    }

    #[test]
    fn ci_when_ready_sends_a_passed_playtest_to_awaiting_ci_instead_of_straight_to_merging() {
        let mut run = ItemRun::new();
        run.state = ItemState::AwaitingPlaytest;
        run.approved_at_head = true;
        let cfg = ProjectConfig {
            require_ci: true,
            ci_when_ready: true,
            playtests: Playtests::On,
            ..cfg()
        };
        run.playtest_finished(PlaytestOutcome::Passed, &cfg);
        assert_eq!(run.state, ItemState::AwaitingCi);

        run.ci_finished(CiOutcome::Passed, &cfg);
        assert_eq!(run.state, ItemState::Merging);
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
    fn being_brought_up_to_date_with_approval_kept_waits_in_awaiting_ci_again() {
        let mut run = ItemRun::new();
        run.state = ItemState::Merging;
        run.approved_at_head = true;
        run.merge_finished(MergeOutcome::UpdatedFromBase { approval_kept: true }, &cfg());
        assert_eq!(run.state, ItemState::AwaitingCi);
        assert!(run.approved_at_head);
        assert_eq!(run.conflict_rounds, 1);
    }

    #[test]
    fn being_brought_up_to_date_without_keeping_approval_drops_it_but_still_waits_for_ci() {
        let mut run = ItemRun::new();
        run.state = ItemState::Merging;
        run.approved_at_head = true;
        run.merge_finished(MergeOutcome::UpdatedFromBase { approval_kept: false }, &cfg());
        assert_eq!(run.state, ItemState::AwaitingCi);
        assert!(!run.approved_at_head);
        assert_eq!(run.conflict_rounds, 1);
    }

    #[test]
    fn conflict_rounds_cap_stops_further_base_updates_so_the_merge_step_lands_it_as_is() {
        let cfg = ProjectConfig {
            max_conflict_rounds: 1,
            ..cfg()
        };
        let mut run = ItemRun::new();
        assert!(run.may_update_from_base(&cfg));

        run.state = ItemState::Merging;
        run.merge_finished(MergeOutcome::UpdatedFromBase { approval_kept: true }, &cfg);
        assert_eq!(run.state, ItemState::AwaitingCi);
        assert!(!run.may_update_from_base(&cfg));
    }

    #[test]
    fn merged_resolves_to_done_or_stays_merged_from_the_backlog_itemdone_flag() {
        let mut run = ItemRun::new();
        run.state = ItemState::Merged;
        run.merged_resolved(true);
        assert_eq!(run.state, ItemState::Done);

        let mut run = ItemRun::new();
        run.state = ItemState::Merged;
        run.merged_resolved(false);
        assert_eq!(run.state, ItemState::Merged);

        run.claimed();
        assert_eq!(run.state, ItemState::Implementing);
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
