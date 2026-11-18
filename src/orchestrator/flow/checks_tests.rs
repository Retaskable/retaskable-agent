use super::test_rig::{PR, check, rig};
use super::{advance, checks};
use crate::item_lifecycle::ItemState;

const BROKEN_BY_BAD: &str = "test ! -f bad.txt";

#[tokio::test]
async fn passing_checks_go_on_to_review_with_their_evidence_and_the_change_found() {
    let r = rig(vec![check("ok", "true", true), check("lint", "false", false)]);
    let done = checks::step(r.ctx.clone(), r.request(ItemState::Checking)).await;
    assert_eq!(done.run.state, ItemState::Reviewing);
    assert_eq!(done.send_back, None);
    let flow = done.flow.unwrap();
    assert_eq!(flow.checks_summary.as_deref(), Some("✓ ok\n✗ lint (non-blocking)"));
    assert!(flow.diff_stat.unwrap().contains("small.rs"));
    assert_eq!((flow.pr_url.as_deref(), flow.pr_number), (Some(PR), Some(7)));
    assert_eq!(done.notes[0], "Checks: ok ✓ lint ✗");
    assert_eq!(done.detail.checks.len(), 2);
}

#[tokio::test]
async fn a_blocking_failure_goes_back_to_the_implementer_with_the_output() {
    let c = check("needs-fix", &format!("echo still-broken; {BROKEN_BY_BAD}"), true);
    let r = rig(vec![c]);
    r.commit("bad.txt", "x");
    let done = checks::step(r.ctx.clone(), r.request(ItemState::Checking)).await;
    assert_eq!(done.run.state, ItemState::Implementing);
    assert_eq!(done.run.check_rounds, 1);
    let message = done.send_back.unwrap();
    assert!(
        message.starts_with("The project's checks failed on your branch:") && message.contains("still-broken"),
        "{message}"
    );
    assert!(message.ends_with("Fix them, commit, and return the result again."));
}

#[tokio::test]
async fn a_failure_main_has_too_is_not_the_changes() {
    let r = rig(vec![check("red-on-main", "false", true)]);
    let done = checks::step(r.ctx.clone(), r.request(ItemState::Checking)).await;
    assert_eq!(done.run.state, ItemState::Reviewing);
    assert!(done.flow.unwrap().checks_summary.unwrap().contains("(fails on main)"));
}

#[tokio::test]
async fn failing_past_max_check_rounds_needs_a_person_naming_the_checks() {
    let r = rig(vec![check("unit", BROKEN_BY_BAD, true)]);
    r.commit("bad.txt", "x");
    let mut request = r.request(ItemState::Checking);
    request.run.check_rounds = r.ctx.project.max_check_rounds;
    let done = checks::step(r.ctx.clone(), request).await;
    assert_eq!(done.run.state, ItemState::NeedsHuman);
    assert_eq!(done.send_back, None);
    assert!(
        done.notes
            .iter()
            .any(|n| n.contains("still failing") && n.contains("unit")),
        "{:?}",
        done.notes
    );
}

#[tokio::test]
async fn an_item_with_no_lane_needs_a_person_and_other_states_have_no_step_yet() {
    let r = rig(vec![]);
    let mut request = r.request(ItemState::Checking);
    request.lane = None;
    assert_eq!(
        checks::step(r.ctx.clone(), request).await.run.state,
        ItemState::NeedsHuman
    );
    assert!(advance(r.ctx.clone(), r.request(ItemState::NeedsHuman)).is_none());
    assert!(advance(r.ctx.clone(), r.request(ItemState::Checking)).is_some());
}

#[tokio::test]
async fn a_lane_that_holds_something_else_is_put_on_the_items_pushed_branch_first() {
    let r = rig(vec![check("ok", "true", true)]);
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&r.lane)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["push", "-q", "origin", super::test_rig::BRANCH]);
    // Another item's work was in the lane last.
    git(&["checkout", "-q", "-B", "other", "origin/main"]);
    assert!(!r.lane.join("small.rs").exists());
    let done = checks::step(r.ctx.clone(), r.request(ItemState::Checking)).await;
    assert_eq!(done.run.state, ItemState::Reviewing, "{:?}", done.notes);
    assert!(r.lane.join("small.rs").exists());
}

fn as_chore(r: &super::test_rig::Rig, kind: &str) -> crate::orchestrator::steps::StepRequest {
    let mut request = r.request(ItemState::Checking);
    let mut item = request.item.take().unwrap_or_default();
    item.milestone = Some(crate::chore_items::MILESTONE.into());
    item.kind = Some(kind.into());
    request.item = Some(item);
    request
}

#[tokio::test]
async fn a_fix_main_is_not_forgiven_what_main_fails_too_but_any_other_chore_is() {
    let r = rig(vec![check("red-on-main", "false", true)]);
    let done = checks::step(r.ctx.clone(), as_chore(&r, "fix_main")).await;
    // Making main's own failures pass is the chore: sent back to its implementer.
    assert_eq!(done.run.state, ItemState::Implementing, "{:?}", done.notes);
    assert!(done.send_back.unwrap().contains("red-on-main"));
    let done = checks::step(r.ctx.clone(), as_chore(&r, "tidy")).await;
    assert_eq!(done.run.state, ItemState::Reviewing);
}

#[tokio::test]
async fn a_main_sync_that_passes_its_checks_goes_to_the_merge_with_no_review() {
    let r = rig(vec![check("ok", "true", true)]);
    let done = checks::step(r.ctx.clone(), as_chore(&r, "sync_main")).await;
    assert_eq!(done.run.state, ItemState::Merging);
    assert!(
        done.notes.iter().any(|n| n.contains("without a review")),
        "{:?}",
        done.notes
    );
    assert_eq!(r.reviews(), 0);
}

#[tokio::test]
async fn checks_set_on_the_site_but_not_approved_here_keep_the_item_checking_with_a_note() {
    let mut r = rig(vec![]);
    r.ctx.project.checks_unapproved = true;
    let done = checks::step(r.ctx.clone(), r.request(ItemState::Checking)).await;
    assert_eq!(done.run.state, ItemState::Checking);
    assert!(done.flow.is_none() && done.send_back.is_none());
    assert!(
        done.notes.iter().any(|n| n.contains("approve the project's commands")),
        "{:?}",
        done.notes
    );
    assert!(!done.notes.iter().any(|n| n.contains("none configured")));
    assert_eq!(r.reviews(), 0);
    // Approved, the checks run.
    r.ctx.project.checks_unapproved = false;
    r.ctx.project.checks = vec![check("ok", "true", true)];
    let done = checks::step(r.ctx.clone(), r.request(ItemState::Checking)).await;
    assert_eq!(done.run.state, ItemState::Reviewing);
    assert_eq!(done.notes[0], "Checks: ok ✓");
}

#[tokio::test]
async fn truly_no_checks_say_none_configured() {
    let r = rig(vec![]);
    let done = checks::step(r.ctx.clone(), r.request(ItemState::Checking)).await;
    assert_eq!(done.run.state, ItemState::Reviewing);
    assert_eq!(done.notes[0], "Checks: none configured");
}

#[tokio::test]
async fn a_check_whose_program_is_missing_here_holds_the_item_and_does_not_blame_it() {
    let r = rig(vec![check("test", "retaskable-no-such-program-xyz --version", true)]);
    let done = checks::step(r.ctx.clone(), r.request(ItemState::Checking)).await;
    assert_eq!(done.run.state, ItemState::Checking, "held, not reviewed or sent back");
    assert_eq!((done.send_back, done.run.check_rounds), (None, 0));
    let note = done.notes.join("\n");
    assert!(
        note.contains("can't run check `test`") && note.contains("`retaskable-no-such-program-xyz` isn't installed"),
        "{note}"
    );
}
