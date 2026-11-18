//! The main sync (`main_sync.rs`) against a fake store, real git repos and a fake `gh`.
use super::store_script::{Store_, store};
use super::*;
use crate::test_support::{git, repo_with_origin, run as run_git, write_executable};
use std::path::{Path, PathBuf};

struct Fixture {
    rig: Rig,
    script: Store_,
    repo: PathBuf,
    root: tempfile::TempDir,
}

pub(super) fn commit(repo: &Path, name: &str, text: &str) {
    std::fs::write(repo.join(name), text).unwrap();
    run_git(git(repo).args(["add", name]));
    run_git(git(repo).args(["commit", "--quiet", "-m", name]));
}

pub(super) fn push(repo: &Path, branch: &str) {
    run_git(git(repo).args(["push", "--quiet", "origin", branch]));
}

/// A fake `gh`: logs its arguments, lists no PR and prints a URL for `pr create`.
pub(super) fn fake_gh(root: &Path) -> PathBuf {
    let path = root.join("gh");
    let log = root.join("gh.log");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> \"{}\"\ncase \"$*\" in \"pr list\"*) echo '[]';; \"pr create\"*) cat >/dev/null; echo https://github.com/o/r/pull/9;; esac\n",
        log.display()
    );
    write_executable(&path, &script);
    path
}

/// origin has `main` one commit ahead of `team` (which has its own commit); the project's base is `team`.
async fn fixture(conflict: bool) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    run_git(git(&repo).args(["checkout", "--quiet", "-b", "team"]));
    commit(&repo, "c.txt", "team side");
    push(&repo, "team");
    run_git(git(&repo).args(["checkout", "--quiet", "main"]));
    commit(&repo, if conflict { "c.txt" } else { "m.txt" }, "main side");
    push(&repo, "main");
    let (base, script) = store().await;
    let mut rig = rig(1, &[&base]);
    let lp = &mut rig.machine.projects[0];
    lp.mode = OrchestratorMode::Runs;
    lp.project.repo_path = repo.display().to_string();
    lp.file_project.repo_path = repo.display().to_string();
    lp.project.worktrees_dir = root.path().join("wt").display().to_string();
    lp.file_project.worktrees_dir = root.path().join("wt").display().to_string();
    lp.project.base_branch = "team".into();
    lp.file_project.base_branch = "team".into();
    lp.seen.default_branch = Some("main".into());
    lp.run_loop.main_sync = crate::orchestrator::driver::main_sync::State::with_gh(fake_gh(root.path()));
    Fixture {
        rig,
        script,
        repo,
        root,
    }
}

fn remote_branches(f: &Fixture) -> String {
    let out = git(&f.repo)
        .args(["ls-remote", "origin", "refs/heads/retaskable/*"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

/// What the minute's fetch shows (`report`), as the loop runs it after each good fetch.
async fn fetched(f: &mut Fixture) {
    let m = &mut f.rig.machine;
    m.projects[0].report(&m.links).await;
}

#[tokio::test]
async fn a_fetch_keeps_how_far_behind_main_the_base_is_and_sends_the_store_nothing_about_it() {
    let mut f = fixture(false).await;
    assert_eq!(
        f.rig.machine.projects[0].main_look.facts(false),
        None,
        "nothing before the first fetch"
    );
    fetched(&mut f).await;
    let facts = f.rig.machine.projects[0].main_look.facts(false).unwrap();
    assert_eq!(
        (facts.branch.as_str(), facts.sha.as_str()),
        ("team", origin_sha(&f, "team").as_str())
    );
    let behind = facts.behind.unwrap();
    assert_eq!(
        (behind.branch.as_str(), behind.head.as_str(), behind.count),
        ("main", origin_sha(&f, "main").as_str(), 1)
    );
    let s = f.script.lock().unwrap();
    assert!(
        s.args("chore_trigger").is_empty() && s.args("lock_take").is_empty() && s.args("add").is_empty(),
        "the store decides what follows: {:?}",
        s.log
    );
    drop(s);
    // The merge is the task's (`chores.rs`): nothing was pushed or opened by the fetch.
    assert_eq!(remote_branches(&f), "");
}

#[tokio::test]
async fn a_base_that_is_main_or_has_all_of_it_reports_no_behind() {
    for case in ["behind nothing", "base is main"] {
        let mut f = fixture(false).await;
        let lp = &mut f.rig.machine.projects[0];
        match case {
            "behind nothing" => lp.seen.default_branch = Some("team".into()),
            _ => lp.project.base_branch = "main".into(),
        }
        fetched(&mut f).await;
        let facts = f.rig.machine.projects[0].main_look.facts(false).unwrap();
        let behind = facts.behind.map(|b| b.count);
        assert!(behind.is_none() || behind == Some(0), "{case}: {behind:?}");
    }
}

/// A fake `gh` that says pull request 7 from team into main merged with the given head, and lists nothing else.
fn gh_with_merged(root: &Path, head: &str) -> PathBuf {
    let path = root.join("gh");
    let log = root.join("gh.log");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> \"{}\"\ncase \"$*\" in \"pr list\"*\"--state merged\"*) echo '[{{\"number\":7,\"mergedAt\":\"2026-02-01T00:00:00Z\",\"headRefOid\":\"{head}\"}}]';; \"pr list\"*) echo '[]';; esac\n",
        log.display()
    );
    write_executable(&path, &script);
    path
}

fn origin_sha(f: &Fixture, branch: &str) -> String {
    let out = git(&f.repo)
        .args(["ls-remote", "origin", &format!("refs/heads/{branch}")])
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn a_fetch_finishes_the_restart_of_a_squashed_dev_branch_once_under_the_lock() {
    let mut f = fixture(false).await;
    let (team, main) = (origin_sha(&f, "team"), origin_sha(&f, "main"));
    let lp = &mut f.rig.machine.projects[0];
    lp.project.github_repo = "o/r".into();
    lp.run_loop.main_sync =
        crate::orchestrator::driver::main_sync::State::with_gh(gh_with_merged(f.root.path(), &team));
    fetched(&mut f).await;
    assert_eq!(origin_sha(&f, "team"), main, "the dev branch starts again from main");
    assert_eq!(origin_sha(&f, "main"), main, "main is never written");
    {
        let s = f.script.lock().unwrap();
        assert_eq!(s.args("lock_take")[0]["name"], "sync_main-team");
        assert_eq!(s.args("lock_drop").len(), 1);
        assert!(s.args("add").is_empty(), "that pass syncs nothing");
    }
    // Further fetches find it restarted: no more lock, no more push.
    let takes = f.script.lock().unwrap().args("lock_take").len();
    for _ in 0..3 {
        fetched(&mut f).await;
    }
    assert_eq!(f.script.lock().unwrap().args("lock_take").len(), takes);
    assert_eq!(origin_sha(&f, "team"), main);
}
