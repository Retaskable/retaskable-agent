//! The head a `pr_opened` end carries (`result.head`), which the store keeps so a passed `ci_finish` for it settles the piece
//! (store-push-now.md section 4).
use super::*;

/// The commit `branch` is at on origin.
fn origin_head(rig: &Rig, branch: &str) -> String {
    crate::git_bin::test_git_stdout(&rig.root.join("origin.git"), &["rev-parse", branch])
}

#[tokio::test]
async fn a_pr_opened_end_carries_the_pushed_head_so_a_passed_ci_finish_can_settle_the_piece() {
    let r = rig(&[COMMIT], &answer("pr_opened", ""));
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened);
    let head = end.result["head"].as_str().expect("the end names the head");
    assert_eq!(head.len(), 40);
    assert_eq!(head, origin_head(&r, BRANCH));
}

#[tokio::test]
async fn the_head_is_the_one_after_a_review_round_changed_the_branch() {
    let r = rig(&[COMMIT, COMMIT_B], &answer("pr_opened", ""));
    script_review(&r, 1, "request_changes");
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened);
    assert_eq!(end.result["head"], origin_head(&r, BRANCH));
}
