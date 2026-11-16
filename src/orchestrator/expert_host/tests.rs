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

fn project_in(root: &Path, repo: &Path) -> ProjectConfig {
    let mut project = item_lifecycle_cfg();
    project.repo_path = repo.to_string_lossy().into_owned();
    project.worktrees_dir = root.join("lanes").to_string_lossy().into_owned();
    project.base_branch = "main".into();
    project
}

#[test]
fn a_lane_helpers_lane_is_on_its_own_branch_from_the_sessions_last_commit_unpushed() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let project = project_in(root.path(), &repo);
    let trees = worktrees(&project);
    let session = "retaskable/expert-x-x";
    let dir = lanes::prepare(&repo, &trees, 1, session, "main").unwrap();
    std::fs::write(dir.join("a.txt"), "committed, never pushed").unwrap();
    run(git(&dir).args(["add", "-A"]));
    run(git(&dir).args(["commit", "--quiet", "-m", "first"]));
    let helper_dir = prepare_helper_lane(&project, 2, &helpers::branch(session, 1), session).unwrap();
    assert_eq!(helper_dir, lanes::lane_path(&trees, 2));
    assert_eq!(
        std::fs::read_to_string(helper_dir.join("a.txt")).unwrap(),
        "committed, never pushed"
    );
    let on = git(&helper_dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&on.stdout).trim(), "retaskable/expert-x-x-h1");
    // The session's own lane is as it was.
    let on = git(&dir).args(["rev-parse", "--abbrev-ref", "HEAD"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&on.stdout).trim(), session);
}

#[cfg(unix)]
async fn run_stub_helper(own: bool, structured: &str) -> (Ended, String) {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let project = project_in(root.path(), &repo);
    let session = "retaskable/expert-x-x";
    lanes::prepare(&repo, &worktrees(&project), 1, session, "main").unwrap();
    let out = format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"structured_output":{structured},"session_id":"s"}}"#
    );
    let env = crate::test_support::stub_engines(root.path(), &out, "", &[]);
    let host = ClaudeExperts::new(root.path().join("data"), env, Default::default());
    let ended = host
        .helper(HelperJob {
            project,
            item: "expert-x".into(),
            n: 1,
            task: "look at it".into(),
            from: session.into(),
            lane: if own { 2 } else { 1 },
            own,
            stop: Arc::new(tokio::sync::Notify::new()),
            sessions: Default::default(),
        })
        .await;
    let calls = std::fs::read_to_string(root.path().join("calls.txt")).unwrap_or_default();
    (ended, calls)
}

#[cfg(unix)]
#[tokio::test]
async fn a_read_helper_is_a_read_only_haiku_run_whose_result_is_its_answer() {
    let (ended, calls) = run_stub_helper(false, r#"{"result":"found it in a.rs:3"}"#).await;
    assert_eq!(ended, Ended::Finished("found it in a.rs:3".into()));
    assert!(calls.contains("--model haiku"), "{calls}");
    assert!(
        calls.contains("--disallowedTools"),
        "a read helper can't write: {calls}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_lane_helper_may_write_and_a_helper_with_no_result_didnt_finish() {
    let (ended, calls) = run_stub_helper(true, r#"{"result":"done"}"#).await;
    assert_eq!(ended, Ended::Finished("done".into()));
    assert!(
        !calls.contains("--disallowedTools Edit"),
        "a lane helper changes files: {calls}"
    );
    let (ended, _) = run_stub_helper(false, r#"{"result":"  "}"#).await;
    assert!(matches!(ended, Ended::Failed(_)), "{ended:?}");
}

#[cfg(unix)]
async fn run_stub_turn(out: &str) -> TurnEnd {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let project = project_in(root.path(), &repo);
    let env = crate::test_support::stub_engines(root.path(), out, "", &[]);
    let host = ClaudeExperts::new(root.path().join("data"), env, Default::default());
    host.turn(ExpertJob {
        project,
        item: "expert-x".into(),
        branch: "retaskable/expert-x-x".into(),
        lane: 1,
        session_id: None,
        message: "hi".into(),
        pictures: Default::default(),
        prepare: true,
        from_branch: false,
        stop: Arc::new(tokio::sync::Notify::new()),
        sessions: Default::default(),
    })
    .await
}

#[cfg(unix)]
#[tokio::test]
async fn a_turn_that_ends_at_the_usage_limit_is_told_apart_from_one_that_failed() {
    let limited = r#"{"type":"result","subtype":"success","is_error":true,"result":"You've hit your limit","api_error_status":429,"session_id":"s"}"#;
    assert_eq!(
        run_stub_turn(limited).await,
        TurnEnd::Limited("You've hit your limit".into())
    );
    let failed =
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"it broke","session_id":"s"}"#;
    assert_eq!(run_stub_turn(failed).await, TurnEnd::Failed("it broke".into()));
}
