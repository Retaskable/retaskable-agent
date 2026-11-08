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

fn commit(repo: &Path, name: &str, text: &str) {
    std::fs::write(repo.join(name), text).unwrap();
    run_git(git(repo).args(["add", name]));
    run_git(git(repo).args(["commit", "--quiet", "-m", name]));
}

fn push(repo: &Path, branch: &str) {
    run_git(git(repo).args(["push", "--quiet", "origin", branch]));
}

/// A fake `gh`: logs its arguments, lists no PR and prints a URL for `pr create`.
fn fake_gh(root: &Path) -> PathBuf {
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
    lp.flags = Arc::new(crate::flags::Flags::fixed(&["chore-rotation", "chore-main-sync"]));
    lp.project.repo_path = repo.display().to_string();
    lp.project.worktrees_dir = root.path().join("wt").display().to_string();
    lp.project.base_branch = "team".into();
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
        let shared = Shared {
            links: &m.links,
            local: &m.local,
            claim_paused_until: &mut None,
            claude_signed_out_until: &mut None,
            engine_limits: &mut Default::default(),
            stopping: false,
        };
        m.projects[0]
            .run_pass(&mut m.pool, &Idle, &shared, Instant::now())
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        if !adds(f).is_empty() && !f.script.lock().unwrap().args("lock_drop").is_empty() {
            break;
        }
    }
}

fn adds(f: &Fixture) -> Vec<Value> {
    f.script.lock().unwrap().args("add")
}

fn remote_branches(f: &Fixture) -> String {
    let out = git(&f.repo)
        .args(["ls-remote", "origin", "refs/heads/retaskable/*"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[tokio::test]
async fn a_clean_main_pushes_the_branch_opens_one_pr_and_adds_one_item() {
    let mut f = fixture(false).await;
    settle(&mut f).await;
    let sha = crate::lanes::head_sha(&f.repo).unwrap();
    let branch = format!("retaskable/sync-main-{}", &sha[..7]);
    assert!(remote_branches(&f).contains(&branch));
    let adds = adds(&f);
    assert_eq!(adds.len(), 1, "{adds:?}");
    assert_eq!(adds[0]["via"], "chore");
    let item = &adds[0]["items"][0];
    assert_eq!(item["kind"], "sync_main");
    assert_eq!(item["id"], format!("chore-sync-main-{}", &sha[..7]));
    assert_eq!(item["title"], "Sync main into team");
    assert_eq!(item["syncSha"], sha);
    assert_eq!(item["branch"], branch);
    assert_eq!(item["prUrl"], "https://github.com/o/r/pull/9");
    assert!(item.get("remaining").is_none());
    let log = std::fs::read_to_string(f.root.path().join("gh.log")).unwrap();
    assert_eq!(log.matches("pr create").count(), 1, "{log}");
    assert!(log.contains("--title Sync: main into team (1 commits)"), "{log}");
    assert!(log.contains("--base team"), "{log}");
    let s = f.script.lock().unwrap();
    assert_eq!(s.args("lock_take")[0]["name"], "sync_main-team");
    assert_eq!(s.args("lock_drop")[0]["token"], "L1");
    drop(s);
    assert_eq!(f.rig.machine.projects[0].run_loop.runs().locks().count(), 0);
}

#[tokio::test]
async fn a_conflicting_main_adds_the_files_and_pushes_nothing() {
    let mut f = fixture(true).await;
    settle(&mut f).await;
    assert_eq!(remote_branches(&f), "");
    let adds = adds(&f);
    assert_eq!(adds.len(), 1, "{adds:?}");
    let item = &adds[0]["items"][0];
    assert_eq!(item["kind"], "sync_main");
    assert!(item["remaining"].as_str().unwrap().contains("c.txt"));
    assert!(item.get("branch").is_none() && item.get("prUrl").is_none());
    let log = std::fs::read_to_string(f.root.path().join("gh.log")).unwrap_or_default();
    assert!(!log.contains("pr "), "{log}");
    assert_eq!(f.script.lock().unwrap().args("lock_drop").len(), 1);
}

async fn adds_nothing(mut f: Fixture) {
    settle(&mut f).await;
    assert!(adds(&f).is_empty());
    assert_eq!(remote_branches(&f), "");
}

#[tokio::test]
async fn an_open_sync_main_item_adds_nothing() {
    let f = fixture(false).await;
    f.script.lock().unwrap().items =
        vec![json!({ "id": "chore-sync-main-1", "title": "t", "status": "todo", "rank": 1, "kind": "sync_main" })];
    adds_nothing(f).await;
}

#[tokio::test]
async fn the_base_being_the_default_branch_adds_nothing() {
    let mut f = fixture(false).await;
    f.rig.machine.projects[0].project.base_branch = "main".into();
    adds_nothing(f).await;
}

#[tokio::test]
async fn sync_main_off_adds_nothing() {
    let mut f = fixture(false).await;
    f.rig.machine.projects[0].project.sync_main = crate::config::SyncMain::Off;
    adds_nothing(f).await;
}

#[tokio::test]
async fn nothing_behind_adds_nothing() {
    let f = fixture(false).await;
    run_git(git(&f.repo).args(["push", "--quiet", "--force", "origin", "main:team"]));
    adds_nothing(f).await;
}

#[tokio::test]
async fn another_lock_holder_adds_nothing() {
    let f = fixture(false).await;
    f.script.lock().unwrap().lock = Some(json!({ "held": false, "by": "other-pc" }));
    let mut f = f;
    settle(&mut f).await;
    assert!(adds(&f).is_empty());
    assert!(!f.script.lock().unwrap().args("lock_take").is_empty());
    assert_eq!(remote_branches(&f), "");
}

#[tokio::test]
async fn a_store_error_adds_nothing() {
    let f = fixture(false).await;
    f.script.lock().unwrap().list_fails = true;
    let mut f = f;
    settle(&mut f).await;
    assert!(adds(&f).is_empty());
    assert!(f.script.lock().unwrap().args("lock_take").is_empty());
}

#[tokio::test]
async fn a_lock_take_error_adds_nothing_and_records_no_lock() {
    let mut f = fixture(false).await;
    f.script.lock().unwrap().lock_fails = true;
    settle(&mut f).await;
    assert!(adds(&f).is_empty());
    assert!(!f.script.lock().unwrap().args("lock_take").is_empty());
    assert_eq!(remote_branches(&f), "");
    assert_eq!(f.rig.machine.projects[0].run_loop.runs().locks().count(), 0);
}

#[tokio::test]
async fn a_skipped_sync_main_item_for_mains_head_adds_nothing() {
    let f = fixture(false).await;
    let sha = crate::lanes::head_sha(&f.repo).unwrap();
    f.script.lock().unwrap().items = vec![json!({
        "id": format!("chore-sync-main-{}", &sha[..7]), "title": "t", "status": "skipped", "rank": 1, "kind": "sync_main"
    })];
    let mut f = f;
    settle(&mut f).await;
    assert!(f.script.lock().unwrap().args("lock_take").is_empty());
    assert!(adds(&f).is_empty());
    assert_eq!(remote_branches(&f), "");
}

#[tokio::test]
async fn with_the_flag_off_nothing_happens() {
    let mut f = fixture(false).await;
    f.rig.machine.projects[0].flags = Arc::new(crate::flags::Flags::fixed(&["chore-rotation"]));
    settle(&mut f).await;
    assert!(f.script.lock().unwrap().args("list").is_empty());
    assert!(adds(&f).is_empty());
}
