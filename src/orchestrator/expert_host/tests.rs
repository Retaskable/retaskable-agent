//! `ClaudeExperts::ship` against real git: a lane's uncommitted work is committed and pushed as the item's own branch,
//! the change is opened (a fake `gh`), and the session's own copy on the remote goes.
use super::*;
use crate::test_support::{git, item_lifecycle_cfg, repo_with_origin, run, write_executable};
use std::path::Path;
use std::process::Command;

/// A `gh` that lists no open change and prints the address of the one `pr create` makes.
fn fake_gh(dir: &Path) -> PathBuf {
    let path = dir.join(if cfg!(windows) { "gh.cmd" } else { "gh.sh" });
    if cfg!(windows) {
        let script = "@echo off\r\necho %* | findstr /C:\"pr list\" >nul && (echo [] & exit /b 0)\r\necho https://github.com/o/r/pull/9\r\n";
        std::fs::write(&path, script).unwrap();
    } else {
        let script = "#!/bin/sh\ncase \"$*\" in \"pr list\"*) echo '[]';; *) cat >/dev/null; echo https://github.com/o/r/pull/9;; esac\n";
        write_executable(&path, script);
    }
    path
}

#[tokio::test]
async fn shipping_commits_the_lanes_work_pushes_it_as_the_items_branch_and_opens_the_change() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let trees = root.path().join("lanes");
    let (session, target) = ("retaskable/expert-x-x", "retaskable/expert-x-x-s0");
    let dir = lanes::prepare(&repo, &trees, 1, session, "main").unwrap();
    // A parked session's branch is on the remote already.
    std::fs::write(dir.join("a.txt"), "first").unwrap();
    run(git(&dir).args(["add", "-A"]));
    run(git(&dir).args(["commit", "--quiet", "-m", "first"]));
    run(git(&dir).args(["push", "--quiet", "origin", &format!("HEAD:refs/heads/{session}")]));
    // Work left uncommitted.
    std::fs::write(dir.join("b.txt"), "second").unwrap();
    let mut project = item_lifecycle_cfg();
    project.repo_path = repo.to_string_lossy().into_owned();
    project.worktrees_dir = trees.to_string_lossy().into_owned();
    project.base_branch = "main".into();
    let gh = fake_gh(root.path());
    let env = Env::from_pairs([("RETASKABLE_GH", gh.to_string_lossy().as_ref())]);
    let host = ClaudeExperts::new(root.path().join("data"), env, Default::default());
    let published = host
        .ship(ShipJob {
            project,
            lane: 1,
            branch: session.into(),
            target: target.into(),
            prepare: false,
            title: "expert-x: X".into(),
            body: "Request\n\nDriven by ada in a Retaskable expert session.".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        (published.change.url.as_str(), published.change.number),
        ("https://github.com/o/r/pull/9", 9)
    );
    // The commit the change is at, for the store's check.
    assert!(
        published.head.as_deref().is_some_and(|h| h.len() == 40),
        "{published:?}"
    );
    let origin = root.path().join("origin.git");
    let shown = |what: &str| {
        let out = Command::new("git")
            .arg("--git-dir")
            .arg(&origin)
            .args(["show", what])
            .output()
            .unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned())
    };
    assert_eq!(
        shown(&format!("{target}:b.txt")),
        (true, "second".into()),
        "the uncommitted work is in the change"
    );
    assert!(shown(&format!("{target}:a.txt")).0);
    assert!(
        !shown(&format!("{session}:a.txt")).0,
        "the session's own branch is gone from the remote"
    );
    let on = git(&dir).args(["rev-parse", "--abbrev-ref", "HEAD"]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&on.stdout).trim(),
        target,
        "the lane carries the item's own branch"
    );
}

#[tokio::test]
async fn a_change_gh_cant_open_is_an_error_naming_why() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let trees = root.path().join("lanes");
    lanes::prepare(&repo, &trees, 1, "retaskable/expert-x-x", "main").unwrap();
    let mut project = item_lifecycle_cfg();
    project.repo_path = repo.to_string_lossy().into_owned();
    project.worktrees_dir = trees.to_string_lossy().into_owned();
    project.base_branch = "main".into();
    let host = ClaudeExperts::new(
        root.path().join("data"),
        Env::from_pairs([(
            "RETASKABLE_GH",
            root.path().join("no-such-gh").to_string_lossy().as_ref(),
        )]),
        Default::default(),
    );
    let why = host
        .ship(ShipJob {
            project,
            lane: 1,
            branch: "retaskable/expert-x-x".into(),
            target: "retaskable/expert-x-x-s0".into(),
            prepare: false,
            title: "t".into(),
            body: "b".into(),
        })
        .await
        .unwrap_err();
    assert!(why.contains("gh pr list"), "{why}");
}
