//! The states a single item's active slice moves through, and the rule that decides the next one
//! from each step's outcome (docs/specs/agent/05-item-lifecycle.md). The TypeScript agent folds this
//! decision into each step itself (`orchestrator.ts`'s `steps.checkStep`, `reviewStep`, ...); this
//! module keeps it separate and pure, so the orchestrator loop that will run the real steps and call
//! it (section 7, not ported yet) can be built, and this can be tested, on its own.
//!
//! What's deliberately left out: the cross-item merge queue, batch merges and GitHub-specific landing
//! mechanics inside `merging` (github-workflow.md, team-branch.md — neither ported here yet). This
//! module only tracks what the *item's own* state becomes once that step reports an outcome.
//!
//! `crate::decisions` is the other half of the `needs_clarification` row: it matches a freshly
//! asked question against what a person has already answered for the item, so a caller can tell
//! `implementer_finished` whether this is a real question or a reask.
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
    /// Needs a human: a budget hit, too many rounds, too many slices, a reviewer escalation, or
    /// an error.
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
    /// How many slices have merged so far (row 27: "increments slice" — on every merge, done or
    /// not). Checked against the cap when the *next* one is claimed, not here, so a slice already
    /// in flight always finishes.
    pub slice: u32,
    /// Slices a human has allowed past `maxSlicesPerItem` (row 29: "Carry on" at the limit
    /// "raises the item's own sliceLimit by another maxSlicesPerItem").
    pub extra_slices: u32,
    /// Times in a row a session has asked again what a person already answered (`decisions.rs`),
    /// and been sent the answer back instead of a person; capped by `maxReasks` (`needs_clarification`
    /// row: "at most twice before the item needs a human"). A person answering directly
    /// (`clarification_answered`) resets it, since that always comes from a person, not a reask.
    pub reasks: u32,
    /// Whether the implementer session just before this one ran out of turns
    /// (06.2-implementer.md "at most maxTurns turns"). Set by [`ran_out_of_turns`](Self::ran_out_of_turns);
    /// cleared once a session finishes normally.
    pub max_turns_hit: bool,
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
            slice: 0,
            extra_slices: 0,
            reasks: 0,
            max_turns_hit: false,
        }
    }

    /// `queued` -> `implementing`: the store handed the item out. Also takes a `merged` item
    /// (one not yet `done`) into its next slice, once the store hands that out too — unless it's
    /// already merged `maxSlicesPerItem` of them (plus anything a human has since allowed past
    /// that), in which case there's nothing in flight to resume and it goes to `needs_human`
    /// instead (row 29); `resume_past_slice_limit` is the **Carry on** for that.
    pub fn claimed(&mut self, cfg: &ProjectConfig) {
        self.state = if self.slice >= cfg.max_slices_per_item + self.extra_slices {
            ItemState::NeedsHuman
        } else {
            ItemState::Implementing
        };
    }

    /// The implementer session exited.
    pub fn implementer_finished(&mut self, outcome: ImplementerOutcome, cfg: &ProjectConfig) {
        self.max_turns_hit = false;
        self.state = match outcome {
            // "Only questions nobody has answered are put to the person": every one just asked
            // already has a decision on file, so the answers go back to the session instead,
            // up to maxReasks times before it needs a human after all.
            ImplementerOutcome::Questions {
                all_already_answered: true,
            } => {
                self.reasks += 1;
                if self.reasks > crate::decisions::MAX_REASKS {
                    ItemState::NeedsHuman
                } else {
                    ItemState::Implementing
                }
            }
            ImplementerOutcome::Questions {
                all_already_answered: false,
            } => ItemState::NeedsClarification,
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

    /// A person answered the session's questions; the same session resumes. Always from a
    /// person, never a reask, so it resets that count.
    pub fn clarification_answered(&mut self) {
        self.reasks = 0;
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
    /// Row 27: "increments slice" happens here regardless of the answer; done moves it to
    /// `done`, otherwise "it stays `merged` here and is back in the backlog's queue for its next
    /// slice" — `claimed()` takes it from `merged` to `implementing`, or to `needs_human` if
    /// that next slice would be past the limit, once the store hands that slice out.
    pub fn merged_resolved(&mut self, item_done: bool) {
        self.slice += 1;
        self.state = if item_done { ItemState::Done } else { ItemState::Merged };
    }

    /// Any state can go to `needs_human`: a budget hit, max rounds, max slices, a reviewer
    /// escalation, or an error.
    pub fn escalate(&mut self) {
        self.state = ItemState::NeedsHuman;
    }

    /// The implementer session hit `error_max_turns` (06.2-implementer.md "at most maxTurns
    /// turns"). Returns whether this is the second one in a row: the caller then escalates to
    /// `needs_human` instead of starting a third fresh session in the same lane.
    pub fn ran_out_of_turns(&mut self) -> bool {
        let twice = self.max_turns_hit;
        self.max_turns_hit = true;
        twice
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

    /// Row 29's other **Carry on**, for when `needs_human` was reached with nothing in flight
    /// because the item hit its slice limit: raises that limit by another `maxSlicesPerItem` and
    /// queues the next slice.
    pub fn resume_past_slice_limit(&mut self, cfg: &ProjectConfig) {
        self.extra_slices += cfg.max_slices_per_item;
        self.claimed(cfg);
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
    /// It exited with questions. `all_already_answered`: at least one question was asked and
    /// every one of them already has a decision on file (`decisions::split_asked` found none
    /// open — an empty question list doesn't count), so it's a reask rather than a real
    /// `needs_clarification` — the answers go back to the session instead of a person.
    Questions { all_already_answered: bool },
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
mod tests;
