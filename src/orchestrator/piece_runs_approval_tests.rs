//! Approval reuse in a `build` or `fix` run (docs/specs/pieces/03-behavior.md "Approval reuse and tier escalation").
use super::*;

const MERGE_MAIN: &str = "git fetch -q origin; git -c user.name=t -c user.email=t@t merge -q --no-edit origin/main";

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Pushes `pushed-branch` with a 30-line `feat.rs` and answers its head.
fn pushed_branch(r: &Rig) -> String {
    let repo = r.root.join("repo");
    git_in(&repo, &["checkout", "-q", "-b", "pushed-branch"]);
    let body: String = (1..=30).map(|n| format!("line {n}\n")).collect();
    std::fs::write(repo.join("feat.rs"), body).unwrap();
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-qm", "feat"]);
    git_in(&repo, &["push", "-q", "origin", "pushed-branch"]);
    git_in(&repo, &["checkout", "-q", "main"]);
    git_in(&repo, &["rev-parse", "pushed-branch"])
}

/// The base moves on with a change of its own.
fn base_moves(r: &Rig) {
    let repo = r.root.join("repo");
    std::fs::write(repo.join("other.rs"), "fn other() {}").unwrap();
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-qm", "other"]);
    git_in(&repo, &["push", "-q", "origin", "main"]);
}

/// Changes the first `n` lines of `feat.rs`, committed, after merging the base in.
fn merge_then_edit(n: u32) -> String {
    format!(
        "{MERGE_MAIN}; sed -i '1,{n}s/$/ edited/' feat.rs; git add -A; git -c user.name=t -c user.email=t@t commit -qm edit"
    )
}

fn origin_head(r: &Rig) -> String {
    git_in(&r.root.join("origin.git"), &["rev-parse", "pushed-branch"])
}

fn reviewed_by_a_reviewer(r: &Rig) -> bool {
    r.root.join("reviewer1.txt").exists()
}

/// A fix on the pushed branch (the base has moved on) whose session runs `step`; the piece carries the approved
/// head, and `conflictOf` it when `conflict`.
async fn fix(step: &str, conflict: bool) -> (Rig, RunEnd) {
    let r = rig(&[step], &answer("pr_opened", ""));
    let sha = pushed_branch(&r);
    base_moves(&r);
    let mut p = piece();
    p["branch"] = json!("pushed-branch");
    p["next"] = json!({ "what": "Merge the base in." });
    p["approvedSha"] = json!(sha);
    if conflict {
        p["conflictOf"] = json!(sha);
    }
    let (req, mut s) = request(&r, RunKind::Fix, p);
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened, "{:?}", end.result);
    (r, end)
}

#[tokio::test]
async fn the_approved_head_is_not_reviewed_again_and_is_sent_again() {
    let (r, end) = fix("true", false).await;
    assert!(!reviewed_by_a_reviewer(&r));
    assert_eq!(end.result["approved"], origin_head(&r));
}

#[tokio::test]
async fn the_same_slice_on_a_moved_base_is_not_reviewed_again() {
    let (r, end) = fix(MERGE_MAIN, false).await;
    assert!(!reviewed_by_a_reviewer(&r));
    let head = origin_head(&r);
    assert_eq!(end.result["approved"], head);
    assert!(!end.result["approved"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn a_small_resolution_keeps_the_approval_only_when_the_fix_was_a_conflict_fix() {
    // Two lines edited: 4 lines of drift, 12 or fewer.
    let (r, end) = fix(&merge_then_edit(2), true).await;
    assert!(!reviewed_by_a_reviewer(&r));
    assert_eq!(end.result["approved"], origin_head(&r));
    // The same edit on a fix for something else is a changed slice: reviewed.
    let (r, end) = fix(&merge_then_edit(2), false).await;
    assert!(reviewed_by_a_reviewer(&r));
    assert_eq!(end.result["approved"], origin_head(&r));
}

#[tokio::test]
async fn a_resolution_past_twelve_lines_is_reviewed_even_for_a_conflict_fix() {
    // Seven lines edited: 14 lines of drift.
    let (r, end) = fix(&merge_then_edit(7), true).await;
    assert!(reviewed_by_a_reviewer(&r));
    assert_eq!(end.result["approved"], origin_head(&r));
}

#[tokio::test]
async fn an_approving_review_sends_the_head_it_approved_and_a_failed_one_sends_nothing() {
    let r = rig(&[COMMIT], &answer("pr_opened", ""));
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(
        end.result["approved"],
        git_in(&r.root.join("origin.git"), &["rev-parse", BRANCH])
    );
    // A review that asked for changes, then approved the head after them: that head.
    let r = rig(&[COMMIT, COMMIT_B], &answer("pr_opened", ""));
    script_review(&r, 1, "request_changes");
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(
        end.result["approved"],
        git_in(&r.root.join("origin.git"), &["rev-parse", BRANCH])
    );
    // A review that needs a person ends failed with no `approved`.
    let r = rig(&[COMMIT], &answer("pr_opened", ""));
    script_review(&r, 1, "needs_human");
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert!(end.result.get("approved").is_none(), "{:?}", end.result);
}
