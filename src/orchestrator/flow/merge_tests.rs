use serde_json::json;

use super::test_rig::{BRANCH, Rig, rig};
use super::{host::now_ms, merge};
use crate::item_lifecycle::ItemState;
use crate::orchestrator::steps::{MergeEnd, StepDone, StepRequest};

const HEAD: &str = "aaaa111";

fn merge_rig() -> Rig {
    let mut r = rig(vec![]);
    r.ci_gh(HEAD);
    r.ctx.project.max_conflict_rounds = 3;
    r
}

fn request(r: &Rig, state: ItemState) -> StepRequest {
    let mut request = r.request(state);
    request.flow.pr_number = Some(7);
    request.flow.approved_sha = Some(HEAD.into());
    request.run.approved_at_head = true;
    request
}

async fn merging(r: &Rig) -> StepDone {
    merge::step(r.ctx.clone(), request(r, ItemState::Merging)).await
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// The item's branch is pushed, and main moved on (`file` written with `text`) since it was cut.
fn base_moved(r: &Rig, file: &str, text: &str) {
    git(&r.lane, &["push", "-q", "origin", BRANCH]);
    let repo = std::path::PathBuf::from(&r.ctx.project.repo_path);
    std::fs::write(repo.join(file), text).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "main moves"]);
    git(&repo, &["push", "-q", "origin", "main"]);
}

fn set(r: &Rig, name: &str, text: &str) {
    std::fs::write(r.root.join(name), text).unwrap();
}

#[tokio::test]
async fn a_mergeable_change_is_squash_merged_and_the_item_is_merged_with_its_commit() {
    let r = merge_rig();
    let done = merging(&r).await;
    assert_eq!(done.run.state, ItemState::Merged);
    assert_eq!(
        done.merge,
        Some(MergeEnd::Merged {
            commit: Some("mc1".into())
        })
    );
    assert!(
        r.gh_calls()
            .iter()
            .any(|c| c.starts_with("pr merge 7 --squash --delete-branch")),
        "{:?}",
        r.gh_calls()
    );
}

#[tokio::test]
async fn a_change_already_merged_is_recorded_without_merging_it_again_and_a_closed_one_needs_a_person() {
    let r = merge_rig();
    r.view("MERGED", HEAD, "UNKNOWN");
    let done = merging(&r).await;
    assert_eq!(
        (done.run.state, done.merge),
        (
            ItemState::Merged,
            Some(MergeEnd::Merged {
                commit: Some("mc1".into())
            })
        )
    );
    assert!(!r.gh_calls().iter().any(|c| c.starts_with("pr merge")));
    r.view("CLOSED", HEAD, "UNKNOWN");
    let done = merging(&r).await;
    assert_eq!(
        (done.run.state, done.merge),
        (ItemState::NeedsHuman, Some(MergeEnd::Closed))
    );
}

#[tokio::test]
async fn a_head_pushed_since_the_approval_goes_through_ci_again() {
    let r = merge_rig();
    r.view("OPEN", "bbbb222", "MERGEABLE");
    let done = merging(&r).await;
    assert_eq!(done.run.state, ItemState::AwaitingCi);
    assert_eq!(done.merge, Some(MergeEnd::Updated { head: "bbbb222".into() }));
    assert!(!r.gh_calls().iter().any(|c| c.starts_with("pr merge")));
}

#[tokio::test]
async fn a_conflicting_change_has_the_base_merged_in_and_goes_to_ci_a_real_conflict_goes_to_the_implementer() {
    let r = merge_rig();
    base_moved(&r, "other.rs", "fn other() {}");
    r.view("OPEN", HEAD, "CONFLICTING");
    let done = merging(&r).await;
    assert_eq!(
        (done.run.state, done.run.conflict_rounds),
        (ItemState::AwaitingCi, 1),
        "{:?}",
        done.notes
    );
    assert!(matches!(done.merge, Some(MergeEnd::Updated { .. })), "{:?}", done.notes);
    assert!(!done.flow.unwrap().ci_wait_since.is_none());
    // The branch and main both changed small.rs.
    let r = merge_rig();
    base_moved(&r, "small.rs", "fn main_wrote_this() {}");
    r.view("OPEN", HEAD, "CONFLICTING");
    let done = merging(&r).await;
    assert_eq!((done.run.state, done.run.conflict_rounds), (ItemState::Implementing, 1));
    assert!(
        done.send_back.as_deref().is_some_and(|s| s.contains("small.rs")),
        "{:?}",
        done.send_back
    );
    // Past maxConflictRounds it needs a person.
    let mut req = request(&r, ItemState::Merging);
    req.run.conflict_rounds = r.ctx.project.max_conflict_rounds;
    let done = merge::step(r.ctx.clone(), req).await;
    assert_eq!(done.run.state, ItemState::NeedsHuman);
}

#[tokio::test]
async fn a_merge_github_isnt_ready_for_is_tried_again_then_needs_a_person() {
    let r = merge_rig();
    set(&r, "merge-code", "1");
    set(
        &r,
        "merge-err",
        "Base branch was modified. Review and try the merge again.",
    );
    let done = merging(&r).await;
    assert_eq!(done.run.state, ItemState::Merging);
    let flow = done.flow.unwrap();
    assert_eq!(flow.merge_retries, 1);
    assert!(flow.retry_after_ms.unwrap() > now_ms() + 20_000);
    let mut req = request(&r, ItemState::Merging);
    req.flow.merge_retries = r.ctx.project.tuning.store.unwrap().max_merge_retries;
    let done = merge::step(r.ctx.clone(), req).await;
    assert_eq!(done.run.state, ItemState::NeedsHuman);
    assert!(matches!(done.merge, Some(MergeEnd::NeedsPerson { .. })));
}

#[tokio::test]
async fn where_a_person_or_githubs_auto_merge_lands_it_the_item_waits_in_awaiting_merge() {
    let mut r = merge_rig();
    r.ctx.project.github = Some(json!({ "land": "person" }));
    let done = merging(&r).await;
    assert_eq!(done.run.state, ItemState::AwaitingMerge);
    assert_eq!(done.flow.as_ref().unwrap().land.as_deref(), Some("person"));
    assert!(!r.gh_calls().iter().any(|c| c.starts_with("pr merge")));
    let mut r = merge_rig();
    r.ctx.project.github = Some(json!({ "land": "auto_merge" }));
    let done = merging(&r).await;
    assert_eq!(
        (done.run.state, done.merge),
        (ItemState::AwaitingMerge, Some(MergeEnd::HandedOff))
    );
    assert!(done.flow.unwrap().auto_merge_on);
    assert!(r.gh_calls().iter().any(|c| c.contains("--auto")));
}

#[tokio::test]
async fn an_awaiting_merge_item_follows_its_change_merged_closed_conflicting_or_still_open() {
    let mut r = merge_rig();
    r.ctx.project.github = Some(json!({ "land": "person" }));
    let waiting = || {
        let mut req = request(&r, ItemState::AwaitingMerge);
        req.flow.land = Some("person".into());
        req
    };
    let done = merge::awaiting(r.ctx.clone(), waiting()).await;
    assert_eq!(done.run.state, ItemState::AwaitingMerge);
    assert!(done.flow.unwrap().retry_after_ms.is_some());
    r.view("MERGED", HEAD, "UNKNOWN");
    let done = merge::awaiting(r.ctx.clone(), waiting()).await;
    assert_eq!(
        (done.run.state, done.merge),
        (
            ItemState::Merged,
            Some(MergeEnd::Merged {
                commit: Some("mc1".into())
            })
        )
    );
    r.view("CLOSED", HEAD, "UNKNOWN");
    assert_eq!(
        merge::awaiting(r.ctx.clone(), waiting()).await.run.state,
        ItemState::NeedsHuman
    );
    r.view("OPEN", HEAD, "CONFLICTING");
    assert_eq!(
        merge::awaiting(r.ctx.clone(), waiting()).await.run.state,
        ItemState::Merging
    );
    // Auto-merge that was turned off is turned on again.
    r.ctx.project.github = Some(json!({ "land": "auto_merge" }));
    r.view("OPEN", HEAD, "MERGEABLE");
    let mut req = request(&r, ItemState::AwaitingMerge);
    req.flow.land = Some("auto_merge".into());
    let done = merge::awaiting(r.ctx.clone(), req).await;
    assert!(done.flow.unwrap().auto_merge_on);
}
