use super::*;
use crate::test_support::item_lifecycle_cfg as cfg;

#[test]
fn claim_moves_queued_to_implementing() {
    let mut run = ItemRun::new();
    assert_eq!(run.state, ItemState::Queued);
    run.claimed(&cfg());
    assert_eq!(run.state, ItemState::Implementing);
}

#[test]
fn questions_go_to_needs_clarification_and_an_answer_resumes_implementing() {
    let mut run = ItemRun::new();
    run.implementer_finished(
        ImplementerOutcome::Questions {
            all_already_answered: false,
        },
        &cfg(),
    );
    assert_eq!(run.state, ItemState::NeedsClarification);
    run.clarification_answered();
    assert_eq!(run.state, ItemState::Implementing);
}

#[test]
fn a_reask_of_an_already_answered_question_resumes_implementing_and_counts_itself() {
    let mut run = ItemRun::new();
    run.implementer_finished(
        ImplementerOutcome::Questions {
            all_already_answered: true,
        },
        &cfg(),
    );
    assert_eq!(run.state, ItemState::Implementing);
    assert_eq!(run.reasks, 1);
}

#[test]
fn reasks_past_max_reasks_needs_a_human() {
    let cfg = cfg();
    let mut run = ItemRun::new();
    for _ in 0..crate::decisions::MAX_REASKS {
        run.implementer_finished(
            ImplementerOutcome::Questions {
                all_already_answered: true,
            },
            &cfg,
        );
        assert_eq!(run.state, ItemState::Implementing);
    }
    run.implementer_finished(
        ImplementerOutcome::Questions {
            all_already_answered: true,
        },
        &cfg,
    );
    assert_eq!(run.state, ItemState::NeedsHuman);
    assert_eq!(run.reasks, crate::decisions::MAX_REASKS + 1);
}

#[test]
fn a_person_answering_directly_resets_the_reask_count() {
    let mut run = ItemRun::new();
    run.implementer_finished(
        ImplementerOutcome::Questions {
            all_already_answered: true,
        },
        &cfg(),
    );
    assert_eq!(run.reasks, 1);
    run.clarification_answered();
    assert_eq!(run.reasks, 0);
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
    assert_eq!(run.slice, 1);

    let mut run = ItemRun::new();
    run.state = ItemState::Merged;
    run.merged_resolved(false);
    assert_eq!(run.state, ItemState::Merged);
    assert_eq!(run.slice, 1);

    run.claimed(&cfg());
    assert_eq!(run.state, ItemState::Implementing);
}

#[test]
fn claiming_past_the_slice_limit_needs_a_human_instead_of_implementing() {
    let cfg = ProjectConfig {
        max_slices_per_item: 2,
        ..cfg()
    };
    let mut run = ItemRun::new();
    run.state = ItemState::Merged;
    run.slice = 2;
    run.claimed(&cfg);
    assert_eq!(run.state, ItemState::NeedsHuman);
}

#[test]
fn carrying_on_past_the_slice_limit_raises_it_and_resumes_implementing() {
    let cfg = ProjectConfig {
        max_slices_per_item: 2,
        ..cfg()
    };
    let mut run = ItemRun::new();
    run.state = ItemState::NeedsHuman;
    run.slice = 2;
    run.resume_past_slice_limit(&cfg);
    assert_eq!(run.state, ItemState::Implementing);
    assert_eq!(run.extra_slices, 2);

    // It takes another maxSlicesPerItem slices to hit the raised limit, not just one more.
    run.slice = 3;
    run.claimed(&cfg);
    assert_eq!(run.state, ItemState::Implementing);
    run.slice = 4;
    run.claimed(&cfg);
    assert_eq!(run.state, ItemState::NeedsHuman);
}

#[test]
fn ran_out_of_turns_is_false_the_first_time_and_true_the_second() {
    let mut run = ItemRun::new();
    assert!(!run.ran_out_of_turns());
    assert!(run.max_turns_hit);
    assert!(run.ran_out_of_turns());
}

#[test]
fn a_normal_finish_clears_a_pending_max_turns_hit() {
    let mut run = ItemRun::new();
    run.ran_out_of_turns();
    run.implementer_finished(ImplementerOutcome::Finished, &cfg());
    assert!(!run.max_turns_hit);
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
