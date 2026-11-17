//! The main sync (`main_sync.rs`) against a fake store, real git repos and a fake `gh`.
use super::fix_main::{Idle, Store_, store};
use super::*;
use crate::test_support::{git, repo_with_origin, run as run_git, write_executable};
use std::path::{Path, PathBuf};
use std::time::Instant;

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
    lp.takes_tasks = true;
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

async fn settle(f: &mut Fixture) {
    for _ in 0..100 {
        let m = &mut f.rig.machine;
        // Production looks again a minute after a look; here every pass may, so one look that failed under load
        // (another test holding the shared git lock, a slow fake store) doesn't leave the rest of the test unlooked.
        m.projects[0].run_loop.main_sync.look_again();
        let shared = Shared {
            links: &m.links,
            local: &m.local,
            own_failure: &mut None,
            claude_signed_out_until: &mut None,
            engine_limits: &mut Default::default(),
            stopping: false,
        };
        m.projects[0]
            .run_pass(&mut m.pool, &Idle, &shared, Instant::now())
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        if !triggers(f).is_empty() {
            break;
        }
    }
}

fn triggers(f: &Fixture) -> Vec<Value> {
    f.script.lock().unwrap().args("chore_trigger")
}

fn remote_branches(f: &Fixture) -> String {
    let out = git(&f.repo)
        .args(["ls-remote", "origin", "refs/heads/retaskable/*"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

/// A few more looks, each as if the minute had passed.
async fn look_more(f: &mut Fixture, n: usize) {
    for _ in 0..n {
        let m = &mut f.rig.machine;
        m.projects[0].run_loop.main_sync.look_again();
        let shared = Shared {
            links: &m.links,
            local: &m.local,
            own_failure: &mut None,
            claude_signed_out_until: &mut None,
            engine_limits: &mut Default::default(),
            stopping: false,
        };
        m.projects[0]
            .run_pass(&mut m.pool, &Idle, &shared, Instant::now())
            .await;
    }
}

#[tokio::test]
async fn a_base_behind_main_asks_the_store_for_the_sync_once_per_main_commit_and_merges_nothing_itself() {
    let mut f = fixture(false).await;
    settle(&mut f).await;
    look_more(&mut f, 3).await;
    let sent = triggers(&f);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(
        sent[0],
        json!({ "trigger": "behind_main", "branch": "team", "main": "main", "behind": 1, "project": f.rig.machine.projects[0].id })
    );
    let s = f.script.lock().unwrap();
    assert!(
        s.args("lock_take").is_empty() && s.args("add").is_empty(),
        "{:?}",
        s.log
    );
    drop(s);
    // The merge is the task's (`chores.rs`): nothing was pushed or opened by the look.
    assert_eq!(remote_branches(&f), "");
}

#[tokio::test]
async fn a_look_that_finds_nothing_due_or_cannot_run_the_task_asks_for_nothing() {
    for case in [
        "behind nothing",
        "syncMain off",
        "base is main",
        "takes no tasks",
        "sync open here",
    ] {
        let mut f = fixture(false).await;
        let lp = &mut f.rig.machine.projects[0];
        match case {
            "behind nothing" => {
                lp.seen.default_branch = Some("team".into());
            }
            "syncMain off" => lp.project.sync_main = crate::config::SyncMain::Off,
            "base is main" => lp.project.base_branch = "main".into(),
            "takes no tasks" => lp.takes_tasks = false,
            _ => {
                let task: crate::backlog::pool::TaskInput =
                    serde_json::from_value(json!({ "id": "t", "subject": "chore:sync_main:team", "prompt": "" }))
                        .unwrap();
                lp.chores.insert(
                    "t".into(),
                    crate::chore_items::sync_work(&task, "abc", "main", "team", &[]),
                );
            }
        }
        look_more(&mut f, 4).await;
        assert!(triggers(&f).is_empty(), "{case}: {:?}", f.script.lock().unwrap().log);
    }
}

#[tokio::test]
async fn a_store_that_breaks_is_told_again_and_one_that_skips_is_not() {
    let mut f = fixture(false).await;
    f.script.lock().unwrap().trigger_fails = true;
    look_more(&mut f, 3).await;
    assert_eq!(
        triggers(&f).len(),
        3,
        "asked again on each look until the store answers"
    );
    assert!(
        f.rig.live.snapshot().projects[&f.rig.machine.projects[0].id]
            .sync_main
            .as_deref()
            .is_some_and(|n| n.contains("Couldn't ask the store"))
    );
    f.script.lock().unwrap().trigger_fails = false;
    f.script.lock().unwrap().trigger = Some(json!({ "skipped": "open" }));
    look_more(&mut f, 3).await;
    assert_eq!(
        triggers(&f).len(),
        4,
        "told once for the commit, and a skip is its answer"
    );
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
async fn a_look_finishes_the_restart_of_a_squashed_dev_branch_once_under_the_lock() {
    let mut f = fixture(false).await;
    let (team, main) = (origin_sha(&f, "team"), origin_sha(&f, "main"));
    let lp = &mut f.rig.machine.projects[0];
    lp.project.github_repo = "o/r".into();
    lp.run_loop.main_sync =
        crate::orchestrator::driver::main_sync::State::with_gh(gh_with_merged(f.root.path(), &team));
    for _ in 0..100 {
        let m = &mut f.rig.machine;
        m.projects[0].run_loop.main_sync.look_again();
        let shared = Shared {
            links: &m.links,
            local: &m.local,
            own_failure: &mut None,
            claude_signed_out_until: &mut None,
            engine_limits: &mut Default::default(),
            stopping: false,
        };
        m.projects[0]
            .run_pass(&mut m.pool, &Idle, &shared, Instant::now())
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        if origin_sha(&f, "team") == main && !f.script.lock().unwrap().args("lock_drop").is_empty() {
            break;
        }
    }
    assert_eq!(origin_sha(&f, "team"), main, "the dev branch starts again from main");
    assert_eq!(origin_sha(&f, "main"), main, "main is never written");
    {
        let s = f.script.lock().unwrap();
        assert_eq!(s.args("lock_take")[0]["name"], "sync_main-team");
        assert_eq!(s.args("lock_drop").len(), 1);
        assert!(s.args("add").is_empty(), "that pass syncs nothing");
    }
    // Further looks find it restarted: no more lock, no more push.
    let takes = f.script.lock().unwrap().args("lock_take").len();
    for _ in 0..3 {
        let m = &mut f.rig.machine;
        m.projects[0].run_loop.main_sync.look_again();
        let shared = Shared {
            links: &m.links,
            local: &m.local,
            own_failure: &mut None,
            claude_signed_out_until: &mut None,
            engine_limits: &mut Default::default(),
            stopping: false,
        };
        m.projects[0]
            .run_pass(&mut m.pool, &Idle, &shared, Instant::now())
            .await;
    }
    assert_eq!(f.script.lock().unwrap().args("lock_take").len(), takes);
    assert_eq!(origin_sha(&f, "team"), main);
}
