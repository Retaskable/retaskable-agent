//! A change a run opened and ended before the store heard of it: the store's piece has no branch, so the next build
//! finds the change on the host by the branch name a build gives it and carries on from it.
use super::*;

/// Pushes `BRANCH` to the rig's origin with `pushed.rs` on it, as an earlier run left it.
fn push_earlier_run(r: &Rig) {
    let repo = r.root.join("repo");
    let git = |args: &[&str]| crate::git_bin::run_test_git(&repo, args);
    git(&["checkout", "-q", "-b", BRANCH]);
    std::fs::write(repo.join("pushed.rs"), "fn p() {}").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "pushed"]);
    git(&["push", "-q", "origin", BRANCH]);
    git(&["checkout", "-q", "main"]);
}

fn pushed_tree(r: &Rig) -> String {
    let out = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(r.root.join("origin.git"))
        .args(["ls-tree", "--name-only", BRANCH])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test]
async fn a_build_with_no_recorded_branch_carries_on_from_the_open_change_and_reports_it() {
    let r = rig(
        &["echo 'fn b() {}' > more.rs; git add -A; git -c user.name=t -c user.email=t@t commit -qm more"],
        &answer("pr_opened", ""),
    );
    push_earlier_run(&r);
    std::fs::write(
        r.root.join("gh_list"),
        r#"[{"number":7,"url":"https://github.com/o/p/pull/7"}]"#,
    )
    .unwrap();
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened, "{:?}", end.result);
    // The end says the branch and the change, which is all the store needs to hold the piece's change again.
    assert_eq!(end.result["branch"], BRANCH);
    assert_eq!(end.result["prNumber"], 7);
    assert_eq!(end.result["prUrl"], "https://github.com/o/p/pull/7");
    let tree = pushed_tree(&r);
    assert!(tree.contains("pushed.rs") && tree.contains("more.rs"), "{tree}");
    let gh = std::fs::read_to_string(r.root.join("gh.log")).unwrap();
    assert!(gh.contains(&format!("pr list --head {BRANCH} --state open")), "{gh}");
    assert!(!gh.contains("pr create"), "{gh}");
}

#[tokio::test]
async fn a_build_with_no_open_change_starts_fresh_from_the_base() {
    let r = rig(&[COMMIT], &answer("pr_opened", ""));
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened);
    assert!(!pushed_tree(&r).contains("pushed.rs"));
    let gh = std::fs::read_to_string(r.root.join("gh.log")).unwrap();
    assert!(gh.contains("pr create"), "{gh}");
}

#[test]
fn a_piece_records_its_change_as_change_or_as_its_pr_url_and_number() {
    let of = |v: Value| serde_json::from_value::<RunPiece>(v).unwrap().recorded_change();
    assert_eq!(
        of(json!({ "title": "t", "change": { "number": 4 } })),
        Some(json!({ "number": 4 }))
    );
    assert_eq!(
        of(json!({ "title": "t", "prUrl": "https://github.com/o/p/pull/9", "prNumber": 9 })),
        Some(json!("https://github.com/o/p/pull/9"))
    );
    assert_eq!(of(json!({ "title": "t", "prNumber": 9 })), Some(json!(9)));
    assert_eq!(
        of(json!({ "title": "t", "change": null, "prUrl": null, "prNumber": null })),
        None
    );
    assert_eq!(of(json!({ "title": "t", "change": " ", "prUrl": "" })), None);
}
