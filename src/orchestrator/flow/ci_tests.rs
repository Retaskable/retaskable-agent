use super::ci;
use super::test_rig::{Rig, rig};
use crate::item_lifecycle::ItemState;
use crate::orchestrator::steps::{StepDone, StepRequest};

const HEAD: &str = "aaaa111";

/// A rig with CI required and run for changes into main, the fake `gh` showing the change open at [`HEAD`].
fn ci_rig() -> Rig {
    let mut r = rig(vec![]);
    r.with_ci();
    r.ci_gh(HEAD);
    r
}

fn request(r: &Rig) -> StepRequest {
    let mut request = r.request(ItemState::AwaitingCi);
    request.flow.pr_number = Some(7);
    request
}

async fn step(r: &Rig, request: StepRequest) -> StepDone {
    ci::step(r.ctx.clone(), request).await
}

fn names(done: &StepDone) -> String {
    done.notes.join(" | ")
}

#[tokio::test]
async fn a_pass_goes_on_to_the_checks_or_to_review_where_ci_covers_them() {
    let mut r = ci_rig();
    r.runs(&[("build", "completed", Some("success"))], "2026-11-01T10:05:00Z");
    let done = step(&r, request(&r)).await;
    assert_eq!(done.run.state, ItemState::Checking);
    r.ctx.project.ci_covers_checks = true;
    assert_eq!(step(&r, request(&r)).await.run.state, ItemState::Reviewing);
}

#[tokio::test]
async fn a_pass_on_the_approved_head_merges_and_a_recheck_runs_the_checks_first() {
    let r = ci_rig();
    r.runs(&[("build", "completed", Some("success"))], "2026-11-01T10:05:00Z");
    let mut req = request(&r);
    req.flow.approved_sha = Some(HEAD.into());
    let done = step(&r, req.clone()).await;
    assert_eq!(done.run.state, ItemState::Merging);
    assert_eq!(done.flow.unwrap().reviewed_sha.as_deref(), Some(HEAD));
    req.flow.recheck = true;
    let done = step(&r, req).await;
    assert_eq!(done.run.state, ItemState::Checking);
    assert!(!done.flow.unwrap().recheck);
    // A head the approval doesn't cover goes the long way round.
    let r = ci_rig();
    r.runs(&[("build", "completed", Some("success"))], "2026-11-01T10:05:00Z");
    let mut req = request(&r);
    req.flow.approved_sha = Some("older".into());
    assert_eq!(step(&r, req).await.run.state, ItemState::Checking);
}

#[tokio::test]
async fn a_red_goes_back_to_the_implementer_with_the_failing_log_and_counts_a_check_round() {
    let r = ci_rig();
    r.runs(&[("build", "completed", Some("failure"))], "2026-11-01T10:05:00Z");
    let done = step(&r, request(&r)).await;
    assert_eq!(done.run.state, ItemState::Implementing);
    assert_eq!(done.run.check_rounds, 1);
    let message = done.send_back.unwrap();
    assert!(
        message.contains("build") && message.contains("boom in the build"),
        "{message}"
    );
    // Past maxCheckRounds a person is needed, and nothing is sent back.
    let mut req = request(&r);
    req.run.check_rounds = r.ctx.project.max_check_rounds;
    let done = step(&r, req).await;
    assert_eq!(
        (done.run.state, done.send_back.as_deref()),
        (ItemState::NeedsHuman, None)
    );
    assert!(names(&done).contains("CI still failing"), "{}", names(&done));
}

#[tokio::test]
async fn a_cancelled_run_is_run_again_up_to_max_ci_reruns_and_then_reads_as_red() {
    let r = ci_rig();
    r.runs(&[("build", "completed", Some("cancelled"))], "2026-11-01T10:05:00Z");
    r.checks_json(&[("build", "cancel")]);
    let done = step(&r, request(&r)).await;
    assert_eq!(done.run.state, ItemState::AwaitingCi);
    assert_eq!(done.flow.unwrap().ci_reruns, 1);
    assert!(r.gh_calls().iter().any(|c| c.starts_with("run rerun 42 --failed")));
    let mut req = request(&r);
    req.flow.ci_reruns = r.ctx.project.tuning.store.unwrap().max_ci_reruns;
    assert_eq!(step(&r, req).await.run.state, ItemState::Implementing);
    // A host that finds no run to run again reads the cancelled one as a failure.
    std::fs::write(r.root.join("checks_json"), "[]").unwrap();
    assert_eq!(step(&r, request(&r)).await.run.state, ItemState::Implementing);
}

#[tokio::test]
async fn the_reruns_and_waits_are_the_stores_numbers_and_ci_is_not_read_without_them() {
    let mut r = ci_rig();
    r.runs(&[("build", "completed", Some("cancelled"))], "2026-11-01T10:05:00Z");
    r.checks_json(&[("build", "cancel")]);
    // A store that allows no reruns: the cancelled run reads as red at once.
    r.ctx.project.tuning.store.as_mut().unwrap().max_ci_reruns = 0;
    assert_eq!(step(&r, request(&r)).await.run.state, ItemState::Implementing);
    // A store whose numbers aren't known: nothing is read, nothing is decided, and the poll goes on.
    r.ctx.project.tuning.store = None;
    let calls_before = r.gh_calls().len();
    let done = step(&r, request(&r)).await;
    assert_eq!(done.run.state, ItemState::AwaitingCi);
    assert!(done.flow.unwrap().retry_after_ms.is_some());
    assert_eq!(r.gh_calls().len(), calls_before, "no gh call");
}

#[tokio::test]
async fn a_cancelled_run_with_the_ready_label_takes_the_label_off_and_on_again() {
    let mut r = ci_rig();
    r.ctx.project.ci_when_ready = Some(true);
    r.runs(&[("build", "completed", Some("cancelled"))], "2026-11-01T10:05:00Z");
    let mut req = request(&r);
    req.flow.ci_ready_at = Some(1);
    req.flow.approved_sha = Some(HEAD.into());
    let done = step(&r, req).await;
    assert_eq!(done.run.state, ItemState::AwaitingCi);
    let flow = done.flow.unwrap();
    assert_eq!(flow.ci_reruns, 1);
    assert!(flow.ci_ready_at.unwrap() > 1 && flow.retry_after_ms.is_some());
    assert!(
        names(&StepDone {
            notes: done.notes,
            ..Default::default()
        })
        .contains("running it again (1/3)")
    );
    let calls = r.gh_calls().join("\n");
    assert!(calls.contains("-X DELETE") && calls.contains("-X POST"), "{calls}");
}

#[tokio::test]
async fn a_pending_check_waits_and_past_the_limit_is_cancelled_to_run_again_then_needs_a_person() {
    let r = ci_rig();
    r.runs(&[("build", "in_progress", None)], "");
    r.checks_json(&[("build", "pending")]);
    let done = step(&r, request(&r)).await;
    assert_eq!(done.run.state, ItemState::AwaitingCi);
    assert!(done.flow.unwrap().retry_after_ms.is_some());
    let mut req = request(&r);
    req.flow.ci_wait_since = Some(1);
    let done = step(&r, req.clone()).await;
    assert_eq!(done.run.state, ItemState::AwaitingCi);
    assert_eq!(done.flow.as_ref().unwrap().ci_reruns, 1);
    assert!(done.notes[0].contains("cancelled it, to run it again"));
    assert!(r.gh_calls().iter().any(|c| c.starts_with("run cancel 42")));
    req.flow.ci_reruns = r.ctx.project.tuning.store.unwrap().max_ci_reruns;
    let done = step(&r, req).await;
    assert_eq!(done.run.state, ItemState::NeedsHuman);
    assert!(
        done.notes[0].starts_with("CI has been pending for over"),
        "{:?}",
        done.notes
    );
}

#[tokio::test]
async fn nothing_reported_waits_out_the_grace_then_carries_on() {
    let r = ci_rig();
    let done = step(&r, request(&r)).await;
    assert_eq!(done.run.state, ItemState::AwaitingCi);
    assert!(done.flow.unwrap().ci_wait_since.is_some());
    let mut req = request(&r);
    req.flow.ci_wait_since = Some(1);
    let done = step(&r, req).await;
    assert_eq!(done.run.state, ItemState::Checking);
    assert!(done.notes[0].starts_with("No CI checks reported after 10 minutes"));
    assert!(done.flow.unwrap().ci_note.is_some());
}

#[tokio::test]
async fn no_ci_for_the_base_a_host_without_ci_or_no_requirement_carries_on_at_once() {
    let mut r = rig(vec![]);
    r.ci_gh(HEAD);
    r.ctx.project.require_ci = true;
    // No workflow on origin/main: nothing runs for the change.
    let done = step(&r, request(&r)).await;
    assert_eq!(done.run.state, ItemState::Checking);
    assert!(done.notes[0].starts_with("No CI runs for pull requests into main"));
    r.ctx.project.require_ci = false;
    assert_eq!(step(&r, request(&r)).await.run.state, ItemState::Checking);
}

#[tokio::test]
async fn with_ci_only_when_ready_the_label_goes_on_for_an_approved_change_and_not_before() {
    let mut r = ci_rig();
    r.ctx.project.ci_when_ready = Some(true);
    let done = step(&r, request(&r)).await;
    assert_eq!(done.run.state, ItemState::Checking);
    assert!(names(&done).contains("running the checks here, with CI beside them"));
    let mut req = request(&r);
    req.flow.approved_sha = Some(HEAD.into());
    let done = step(&r, req).await;
    assert_eq!(done.run.state, ItemState::AwaitingCi);
    let flow = done.flow.unwrap();
    assert!(flow.ci_ready_at.is_some() && flow.ci_wait_since.is_some());
    assert!(
        r.gh_calls()
            .iter()
            .any(|c| c.contains("labels") && c.contains("ci-ready"))
    );
}

#[tokio::test]
async fn with_ci_only_when_ready_a_failure_from_before_the_label_is_not_this_runs() {
    let mut r = ci_rig();
    r.ctx.project.ci_when_ready = Some(true);
    r.runs(&[("build", "completed", Some("failure"))], "2026-11-01T10:05:00Z");
    let mut req = request(&r);
    req.flow.approved_sha = Some(HEAD.into());
    req.flow.ci_ready_at = Some(i64::MAX / 2);
    req.flow.ci_wait_since = Some(super::host::now_ms());
    let done = step(&r, req).await;
    assert_eq!(
        (done.run.state, done.send_back.as_deref()),
        (ItemState::AwaitingCi, None)
    );
}

#[tokio::test]
async fn a_change_that_cannot_be_found_needs_a_person() {
    let r = ci_rig();
    let mut req = r.request(ItemState::AwaitingCi);
    req.flow.pr_number = None;
    std::fs::write(r.root.join("gh_list"), "[]").unwrap();
    let done = step(&r, req).await;
    assert_eq!(done.run.state, ItemState::NeedsHuman);
    assert!(done.notes[0].starts_with("Lost track of this pull request"));
}

#[tokio::test]
async fn the_label_goes_on_beside_the_checks_when_the_change_is_there_and_the_lane_is_pushed() {
    let mut r = ci_rig();
    r.ctx.project.ci_when_ready = Some(true);
    let head = r.head();
    r.ci_gh(&head);
    let mut flow = crate::store::FlowRecord {
        pr_number: Some(7),
        ..Default::default()
    };
    let mut notes = Vec::new();
    ci::start(&r.ctx, &mut flow, &r.lane, &mut notes).await;
    assert!(flow.ci_ready_at.is_some(), "{notes:?}");
    assert!(notes[0].contains("CI runs beside the checks and review"));
    // Once on, it is not put on again.
    let calls = r.gh_calls().len();
    ci::start(&r.ctx, &mut flow, &r.lane, &mut notes).await;
    assert_eq!(r.gh_calls().len(), calls);
}
