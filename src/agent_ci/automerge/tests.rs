use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

use super::*;

/// A store whose lock is held by another computer for the first `busy` asks.
pub(crate) struct FakeLocks {
    pub busy: Mutex<u32>,
    pub log: Arc<Mutex<Vec<String>>>,
}

impl MergeLocks for FakeLocks {
    fn take<'a>(&'a self, _: &'a str, name: &'a str, note: &'a str) -> BoxFuture<'a, Result<LockTake, String>> {
        Box::pin(async move {
            self.log.lock().unwrap().push(format!("take {name} {note}"));
            let mut busy = self.busy.lock().unwrap();
            if *busy > 0 {
                *busy -= 1;
                return Ok(LockTake::HeldBy {
                    by: "other-pc".into(),
                    note: None,
                });
            }
            Ok(LockTake::Held("L1".into()))
        })
    }

    fn drop_lock<'a>(&'a self, _: &'a str, token: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move { self.log.lock().unwrap().push(format!("drop {token}")) })
    }
}

struct Rig {
    dir: tempfile::TempDir,
    am: Automerge,
    log: Arc<Mutex<Vec<String>>>,
}

fn script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// `view` is `gh pr view`'s answer; `main` is what `origin/main` is now; a real `gh pr merge` is logged to `gh.log`.
fn rig(view: &str, main: &str, busy: u32) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("gh.log");
    script(
        dir.path(),
        "gh",
        &format!(
            "echo \"$*\" >> {}\nif [ \"$2\" = view ]; then cat <<'EOF'\n{view}\nEOF\nfi",
            log.display()
        ),
    );
    script(
        dir.path(),
        "git",
        &format!("[ \"$1\" = rev-parse ] && echo {main}\nexit 0"),
    );
    let log = Arc::new(Mutex::new(Vec::new()));
    let locks = FakeLocks {
        busy: Mutex::new(busy),
        log: log.clone(),
    };
    let am = Automerge {
        locks: Arc::new(locks),
        ask_every: Duration::from_millis(5),
        wait: Duration::from_secs(5),
    };
    Rig { dir, am, log }
}

const CHECKED_MAIN: &str = "mmmm";

async fn land(r: &Rig) -> Ended {
    let key = CheckKey {
        pr: 7,
        head: "h7".into(),
        main: CHECKED_MAIN.into(),
    };
    let dir = r.dir.path();
    let (gh, git) = (dir.join("gh"), dir.join("git"));
    run(
        &r.am,
        &Target {
            gh: gh.as_os_str(),
            git: git.as_os_str(),
            repo_dir: dir,
            github_repo: "o/p",
            base: "main",
            computer: "pc",
            key: &key,
        },
    )
    .await
}

fn gh_calls(r: &Rig) -> String {
    std::fs::read_to_string(r.dir.path().join("gh.log")).unwrap_or_default()
}

const LABELLED: &str = r#"{"state":"OPEN","headRefOid":"h7","labels":[{"name":"automerge"}]}"#;

#[tokio::test]
async fn a_green_automerge_pull_request_is_squash_merged_under_the_lock_and_the_lock_is_dropped() {
    let r = rig(LABELLED, CHECKED_MAIN, 0);
    assert_eq!(land(&r).await, Ended::Merged);
    let log = r.log.lock().unwrap().clone();
    assert_eq!(log, ["take merge-main PR #7 on pc", "drop L1"]);
    assert!(
        gh_calls(&r).contains("pr merge 7 --squash --delete-branch --match-head-commit h7 --repo o/p"),
        "{}",
        gh_calls(&r)
    );
}

#[tokio::test]
async fn a_held_lock_is_waited_for() {
    let r = rig(LABELLED, CHECKED_MAIN, 3);
    assert_eq!(land(&r).await, Ended::Merged);
    let log = r.log.lock().unwrap().clone();
    assert_eq!(log.iter().filter(|l| l.starts_with("take")).count(), 4);
    assert_eq!(log.last().unwrap(), "drop L1");
}

#[tokio::test]
async fn a_lock_held_past_the_wait_is_a_failure_with_nothing_merged_or_dropped() {
    let mut r = rig(LABELLED, CHECKED_MAIN, u32::MAX);
    r.am.wait = Duration::from_millis(20);
    assert!(matches!(land(&r).await, Ended::Failed(w) if w.contains("other-pc")));
    assert!(!gh_calls(&r).contains("pr merge"));
    assert!(r.log.lock().unwrap().iter().all(|l| !l.starts_with("drop")));
}

#[tokio::test]
async fn main_moving_after_the_check_merges_nothing_and_drops_the_lock() {
    let r = rig(LABELLED, "newmain", 0);
    assert_eq!(land(&r).await, Ended::MainMoved);
    assert!(!gh_calls(&r).contains("pr merge"));
    assert_eq!(r.log.lock().unwrap().last().unwrap(), "drop L1");
}

#[tokio::test]
async fn a_pull_request_without_the_label_or_with_a_new_head_takes_no_lock_and_is_not_merged() {
    for view in [
        r#"{"state":"OPEN","headRefOid":"h7","labels":[{"name":"ci-ready"}]}"#,
        r#"{"state":"OPEN","headRefOid":"other","labels":[{"name":"automerge"}]}"#,
        r#"{"state":"CLOSED","headRefOid":"h7","labels":[{"name":"automerge"}]}"#,
    ] {
        let r = rig(view, CHECKED_MAIN, 0);
        assert_eq!(land(&r).await, Ended::NotAsked, "{view}");
        assert!(r.log.lock().unwrap().is_empty());
        assert!(!gh_calls(&r).contains("pr merge"));
    }
}

#[tokio::test]
async fn a_head_pushed_while_the_lock_was_waited_for_is_not_merged() {
    let r = rig(LABELLED, CHECKED_MAIN, 0);
    // The first read of the pull request is the checked head; every later one is a new head.
    let moved = r#"{"state":"OPEN","headRefOid":"h8","labels":[{"name":"automerge"}]}"#;
    script(
        r.dir.path(),
        "gh",
        &format!(
            "echo \"$*\" >> {log}\nif [ \"$2\" = view ]; then\n if [ -e {seen} ]; then echo '{moved}'; else touch {seen}; echo '{LABELLED}'; fi\nfi",
            log = r.dir.path().join("gh.log").display(),
            seen = r.dir.path().join("seen").display(),
        ),
    );
    assert_eq!(land(&r).await, Ended::NotAsked);
    assert!(!gh_calls(&r).contains("pr merge"));
    assert_eq!(r.log.lock().unwrap().last().unwrap(), "drop L1");
}
