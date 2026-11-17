//! After the merge: the staging smoke (docs/specs/agent/06.3-review.md, "After the merge: the
//! staging smoke"; `agent/src/smoke.ts` in the TypeScript agent). With a `deploy.staging.smoke`
//! command whose staging branch is the base branch, every advance of `origin/<base_branch>` (a
//! merge, a chore's too) is checked once staging is running it: [`run`] polls the
//! deploy's `version.json` until it serves the merged commit or a later one, then runs
//! the smoke command in its own reused worktree ([`crate::lanes::prepare_smoke_checkout`]).
//! [`run_with`] is the same with a newer merge able to call it off while it waits and the `running`
//! state reported; `orchestrator/driver/smoke_run.rs` calls it after a merge, keeps each state as the
//! store's `smoke` fact and reports a failure with `chore_trigger staging_smoke` (the store decides the fix).
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::{DeployTarget, ProjectConfig};
use crate::lanes;
use crate::proc;
use crate::store::{SmokeState, SmokeStatus};
use crate::telemetry::events::{iso_utc_millis, now_ms};

/// `deploy.staging.versionPath`'s default (06.3-review.md: "default `/version.json`").
const DEFAULT_VERSION_PATH: &str = "/version.json";
/// How often the deploy is polled (06.3-review.md: "every 30 seconds").
/// (Shortened under test, whose fake staging starts serving a commit at a moment the test chooses.)
const POLL_INTERVAL: Duration = if cfg!(test) {
    Duration::from_millis(50)
} else {
    Duration::from_secs(30)
};
/// How long the deploy is given before giving up (06.3-review.md: "No deploy within 20 minutes").
const DEPLOY_WAIT_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// How long the smoke command itself is given (06.3-review.md: "a 15-minute timeout").
const SMOKE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How much of a failed command's output is kept, the same as stage 1's checks.
const TAIL_LINES: usize = 200;

/// The project's staging target, when the post-merge smoke applies to it at all: it needs a
/// `smoke` command, and staging has to deploy from the project's own base branch (06.3-review.md:
/// "whose staging branch is the base branch").
pub fn enabled(cfg: &ProjectConfig) -> Option<&DeployTarget> {
    let staging = cfg.deploy.staging.as_ref()?;
    if staging.smoke.is_some() && staging.branch == cfg.base_branch {
        Some(staging)
    } else {
        None
    }
}

/// Runs the whole post-merge smoke for `sha` against `target`: waits for the deploy to catch up,
/// then runs the smoke command in the reused smoke worktree. Never errors on a failed or slow
/// deploy or command — those are `Failed`/`Timeout` states, not [`Result::Err`]; `Err` is only a
/// local problem (git or the filesystem).
pub fn run(repo_path: &Path, worktrees_dir: &Path, target: &DeployTarget, sha: &str) -> Result<SmokeState> {
    Ok(run_with(repo_path, worktrees_dir, target, sha, &|| false, &|_| {})?.expect("a run nothing calls off ends"))
}

/// [`run`] that stops, answering `None`, once `cancelled` says so while it still waits for the deploy (a newer merge
/// replaces it: its deploy has this one's changes too; a smoke command already running is let finish), and that
/// reports the `running` state to `progress` when it starts the command.
pub fn run_with(
    repo_path: &Path,
    worktrees_dir: &Path,
    target: &DeployTarget,
    sha: &str,
    cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(SmokeState),
) -> Result<Option<SmokeState>> {
    let url = target.url.clone().unwrap_or_default();
    let version_path = target
        .version_path
        .clone()
        .unwrap_or_else(|| DEFAULT_VERSION_PATH.to_string());
    let http = reqwest::blocking::Client::new();
    match wait_until(
        || poll_once(&http, repo_path, &url, &version_path, sha),
        |wait| sleep_unless(wait, cancelled),
        POLL_INTERVAL,
        DEPLOY_WAIT_TIMEOUT,
        cancelled,
    ) {
        Wait::Cancelled => return Ok(None),
        Wait::TimedOut => {
            return Ok(Some(SmokeState {
                sha: sha.to_string(),
                status: SmokeStatus::Timeout,
                at: iso_utc_millis(now_ms()),
                url: Some(url),
                tail: None,
            }));
        }
        Wait::Live => {}
    }
    let Some(command) = target.smoke.as_deref() else {
        return Ok(Some(SmokeState {
            sha: sha.to_string(),
            status: SmokeStatus::Failed,
            at: iso_utc_millis(now_ms()),
            url: Some(url),
            tail: Some("deploy.staging.smoke isn't configured".to_string()),
        }));
    };
    progress(SmokeState {
        sha: sha.to_string(),
        status: SmokeStatus::Running,
        at: iso_utc_millis(now_ms()),
        url: Some(url.clone()),
        tail: None,
    });
    let dir = lanes::prepare_smoke_checkout(repo_path, worktrees_dir, sha)?;
    let (passed, tail) = run_smoke_command(&dir, command)?;
    Ok(Some(SmokeState {
        sha: sha.to_string(),
        status: if passed {
            SmokeStatus::Passed
        } else {
            SmokeStatus::Failed
        },
        at: iso_utc_millis(now_ms()),
        url: Some(url),
        tail: Some(tail),
    }))
}

/// One poll: true once `target_sha` is deployed (the version endpoint serves it or a later
/// commit). Any failure to reach it, parse it or resolve it locally just isn't deployed yet —
/// expected while the deploy is still rolling out, not an error worth stopping the wait for.
fn poll_once(
    http: &reqwest::blocking::Client,
    repo_path: &Path,
    url: &str,
    version_path: &str,
    target_sha: &str,
) -> bool {
    let Ok(response) = http
        .get(format!("{url}{version_path}"))
        .timeout(Duration::from_secs(10))
        .send()
    else {
        return false;
    };
    let Ok(body) = response.text() else {
        return false;
    };
    let Some(deployed_sha) = parse_deployed_sha(&body) else {
        return false;
    };
    is_ancestor(repo_path, target_sha, &deployed_sha).unwrap_or(false)
}

/// `{"sha": "..."}`'s `sha`.
pub(crate) fn parse_deployed_sha(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("sha")?
        .as_str()
        .map(str::to_string)
}

/// `origin/<base_branch>`'s current commit, after fetching unless this repo's fetch is still
/// fresh (docs/specs/agent/06.3-review.md: "It fetches and takes origin/<baseBranch>'s head as
/// the merged commit"). Not called from [`run`] (the merged commit is handed in as `sha`
/// already); it's here for the orchestrator-loop's not-yet-built trigger to resolve that commit
/// from.
pub fn remote_head_sha(repo_path: &Path, base_branch: &str) -> Result<String> {
    lanes::fetch_latest(repo_path)?;
    Ok(
        lanes::git_output_in_worktree(repo_path, &["rev-parse", &format!("origin/{base_branch}")])?
            .trim()
            .to_string(),
    )
}

/// Whether `ancestor` is `descendant`'s ancestor, or the same commit — fetching first when
/// `descendant` isn't known locally yet (06.3-review.md: "fetching first when the deployed
/// commit isn't known yet"). `false`, not an error, when `descendant` still isn't known after
/// that fetch (e.g. a deploy reporting a commit this repo never pushed).
fn is_ancestor(repo_path: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    if !commit_known(repo_path, descendant)? {
        lanes::fetch_latest(repo_path)?;
    }
    if !commit_known(repo_path, descendant)? {
        return Ok(false);
    }
    Ok(lanes::git_output_in_worktree(repo_path, &["merge-base", "--is-ancestor", ancestor, descendant]).is_ok())
}

fn commit_known(repo_path: &Path, sha: &str) -> Result<bool> {
    Ok(lanes::git_output_in_worktree(repo_path, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_ok())
}

/// Sleeps `wait`, in short steps so a call off (a newer merge, a stop) is heard within a fraction of a second rather than
/// after the 30-second poll interval (a stopping agent waits for this thread).
fn sleep_unless(wait: Duration, cancelled: &dyn Fn() -> bool) {
    const STEP: Duration = Duration::from_millis(200);
    let end = std::time::Instant::now() + wait;
    while !cancelled() {
        let left = end.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(STEP));
    }
}

/// How a wait for the deploy ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    Live,
    TimedOut,
    Cancelled,
}

/// Polls `poll` up to `timeout`/`interval` times, sleeping `interval` between attempts (never
/// after the last one), stopping as soon as `poll` answers true. `poll` and `sleep` are injected
/// so tests exercise every attempt without any real waiting.
#[cfg(test)]
fn wait_for_deploy_with(
    poll: impl FnMut() -> bool,
    sleep: impl FnMut(Duration),
    interval: Duration,
    timeout: Duration,
) -> Result<bool> {
    Ok(wait_until(poll, sleep, interval, timeout, &|| false) == Wait::Live)
}

/// [`wait_for_deploy_with`] that looks at `cancelled` before each poll and stops if it says so.
fn wait_until(
    mut poll: impl FnMut() -> bool,
    mut sleep: impl FnMut(Duration),
    interval: Duration,
    timeout: Duration,
    cancelled: &dyn Fn() -> bool,
) -> Wait {
    let max_polls = (timeout.as_secs() / interval.as_secs().max(1)).max(1);
    for attempt in 0..max_polls {
        if cancelled() {
            return Wait::Cancelled;
        }
        if poll() {
            return Wait::Live;
        }
        if attempt + 1 < max_polls {
            sleep(interval);
        }
    }
    Wait::TimedOut
}

/// Installs if the checkout needs it (the same rule as the base checkout's, section 6.8), then
/// runs `command` with [`SMOKE_TIMEOUT`]. `Ok((false, tail))` for either a failed install or a
/// failed/timed-out command — the caller doesn't need to tell those apart beyond the tail it
/// shows.
fn run_smoke_command(dir: &Path, command: &str) -> Result<(bool, String)> {
    if proc::needs_npm_install(dir) {
        let install_command = proc::npm_install_command(dir);
        let (code, output, timed_out) = proc::run_command_with_timeout(install_command, dir, proc::NPM_INSTALL_TIMEOUT)
            .with_context(|| format!("running `{install_command}`"))?;
        if code != 0 || timed_out {
            return Ok((false, proc::tail_lines(&output, TAIL_LINES)));
        }
    }
    let (code, output, timed_out) =
        proc::run_command_with_timeout(command, dir, SMOKE_TIMEOUT).with_context(|| format!("running `{command}`"))?;
    Ok((code == 0 && !timed_out, proc::tail_lines(&output, TAIL_LINES)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::config::DeployConfig;

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
    fn a_resumable_state_is_waiting_or_running_and_a_finished_one_isnt() {
        let state = |status| SmokeState {
            sha: "abc".to_string(),
            status,
            at: "now".to_string(),
            url: None,
            tail: None,
        };
        assert!(state(SmokeStatus::Waiting).resumable());
        assert!(state(SmokeStatus::Running).resumable());
        assert!(!state(SmokeStatus::Passed).resumable());
        assert!(!state(SmokeStatus::Failed).resumable());
        assert!(!state(SmokeStatus::Timeout).resumable());
    }

    #[test]
    fn parse_deployed_sha_reads_the_sha_field_and_ignores_the_rest() {
        assert_eq!(
            parse_deployed_sha(r#"{"sha": "abc123", "other": 1}"#),
            Some("abc123".to_string())
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

    #[test]
    fn wait_for_deploy_with_stops_as_soon_as_poll_answers_true() {
        let calls = AtomicUsize::new(0);
        let sleeps = AtomicUsize::new(0);
        let deployed = wait_for_deploy_with(
            || calls.fetch_add(1, Ordering::SeqCst) == 2, // true on the 3rd attempt
            |_| {
                sleeps.fetch_add(1, Ordering::SeqCst);
            },
            Duration::from_secs(30),
            Duration::from_secs(20 * 60),
        )
        .unwrap();

        assert!(deployed);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(sleeps.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn wait_for_deploy_with_gives_up_after_the_timeout_without_a_trailing_sleep() {
        let polls = AtomicUsize::new(0);
        let sleeps = AtomicUsize::new(0);
        let deployed = wait_for_deploy_with(
            || {
                polls.fetch_add(1, Ordering::SeqCst);
                false
            },
            |_| {
                sleeps.fetch_add(1, Ordering::SeqCst);
            },
            Duration::from_secs(30),
            Duration::from_secs(90), // 3 attempts: 90s / 30s
        )
        .unwrap();

        assert!(!deployed);
        assert_eq!(polls.load(Ordering::SeqCst), 3);
        assert_eq!(sleeps.load(Ordering::SeqCst), 2);
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
    fn remote_head_sha_reports_origins_head_after_a_push_it_never_fetched_itself() {
        let root = tempfile::tempdir().unwrap();
        let repo = init_repo_with_origin(root.path());
        // A second clone pushes a commit the first repo's checkout has never fetched.
        let other = root.path().join("other-clone");
        clone_from_origin(&repo, &other);
        run_git(git(&other).args(["config", "user.email", "test@example.com"]));
        run_git(git(&other).args(["config", "user.name", "Test"]));
        std::fs::write(other.join("README.md"), "from the other clone").unwrap();
        run_git(git(&other).args(["add", "README.md"]));
        run_git(git(&other).args(["commit", "--quiet", "-m", "elsewhere"]));
        run_git(git(&other).args(["push", "--quiet", "origin", "main"]));
        let expected = head_sha(&other);

        let sha = remote_head_sha(&repo, "main").unwrap();

        assert_eq!(sha, expected);
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

    /// A one-shot local HTTP server answering every request with `body`, so `run`'s deploy poll
    /// has a real staging to ask instead of a real network call.
    fn serve_version_json(body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    #[test]
    fn run_passes_when_the_deploy_is_already_there_and_the_smoke_command_succeeds() {
        let root = tempfile::tempdir().unwrap();
        let repo = init_repo_with_origin(root.path());
        let worktrees = root.path().join("worktrees");
        let sha = head_sha(&repo);
        let url = serve_version_json(format!(r#"{{"sha": "{sha}"}}"#));
        let smoke_command = if cfg!(windows) { "exit /b 0" } else { "true" };
        let target = DeployTarget {
            branch: "main".to_string(),
            url: Some(url),
            smoke: Some(smoke_command.to_string()),
            version_path: None,
            rollout: None,
        };

        let state = run(&repo, &worktrees, &target, &sha).unwrap();

        assert_eq!(state.status, SmokeStatus::Passed);
        assert_eq!(state.sha, sha);
    }

    #[test]
    fn run_fails_when_the_deploy_is_there_but_the_smoke_command_fails() {
        let root = tempfile::tempdir().unwrap();
        let repo = init_repo_with_origin(root.path());
        let worktrees = root.path().join("worktrees");
        let sha = head_sha(&repo);
        let url = serve_version_json(format!(r#"{{"sha": "{sha}"}}"#));
        let smoke_command = if cfg!(windows) { "exit /b 1" } else { "false" };
        let target = DeployTarget {
            branch: "main".to_string(),
            url: Some(url),
            smoke: Some(smoke_command.to_string()),
            version_path: None,
            rollout: None,
        };

        let state = run(&repo, &worktrees, &target, &sha).unwrap();

        assert_eq!(state.status, SmokeStatus::Failed);
    }

    #[test]
    fn run_times_out_when_nothing_ever_answers_the_version_endpoint() {
        let root = tempfile::tempdir().unwrap();
        let repo = init_repo_with_origin(root.path());
        let sha = head_sha(&repo);
        // A free port nothing listens on: every poll fails to connect.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let target = DeployTarget {
            branch: "main".to_string(),
            url: Some(format!("http://127.0.0.1:{port}")),
            smoke: Some("true".to_string()),
            version_path: None,
            rollout: None,
        };
        let http = reqwest::blocking::Client::new();

        let deployed = wait_for_deploy_with(
            || poll_once(&http, &repo, target.url.as_deref().unwrap(), DEFAULT_VERSION_PATH, &sha),
            |_| {},
            Duration::from_millis(1),
            Duration::from_millis(3),
        )
        .unwrap();

        assert!(!deployed);
    }

    #[test]
    fn run_with_reports_running_once_the_deploy_is_live_and_before_the_command_ends() {
        let root = tempfile::tempdir().unwrap();
        let repo = init_repo_with_origin(root.path());
        let sha = head_sha(&repo);
        let url = serve_version_json(format!(r#"{{"sha": "{sha}"}}"#));
        let target = DeployTarget {
            branch: "main".to_string(),
            url: Some(url.clone()),
            smoke: Some(if cfg!(windows) { "exit /b 0" } else { "true" }.to_string()),
            version_path: None,
            rollout: None,
        };
        let seen = std::sync::Mutex::new(Vec::new());

        let state = run_with(&repo, &root.path().join("wt"), &target, &sha, &|| false, &|s| {
            seen.lock().unwrap().push(s);
        })
        .unwrap()
        .unwrap();

        assert_eq!(state.status, SmokeStatus::Passed);
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            (seen[0].status, seen[0].url.as_deref()),
            (SmokeStatus::Running, Some(url.as_str()))
        );
    }

    #[test]
    fn a_wait_called_off_by_a_newer_merge_runs_nothing_and_answers_none() {
        let root = tempfile::tempdir().unwrap();
        let repo = init_repo_with_origin(root.path());
        let target = DeployTarget {
            branch: "main".to_string(),
            url: Some("http://127.0.0.1:9".to_string()),
            smoke: Some("true".to_string()),
            version_path: None,
            rollout: None,
        };
        let polled = std::sync::atomic::AtomicBool::new(false);
        let got = run_with(&repo, &root.path().join("wt"), &target, "abc", &|| true, &|_| {
            polled.store(true, Ordering::SeqCst);
        })
        .unwrap();
        assert_eq!(got, None);
        assert!(!polled.load(Ordering::SeqCst));
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
}
