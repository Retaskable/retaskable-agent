//! The staging smoke (docs/specs/agent/06.3-review.md, "The staging smoke"; `agent/src/smoke.ts`'s `runSmokeTask` in the
//! TypeScript agent). It runs only as a task the store sends (`job.kind == "smoke"`, `job.args` `{ sha, url, command }`,
//! decided 2026-11-17): the store picks the commit, writes the `smoke` fact and makes the fix after a failure, and this
//! computer only checks the task against its own settings ([`accept`]), waits for staging to serve the commit and runs
//! the project's command at it ([`run_with`]), and answers with a result ([`end_body`]). It starts nothing itself.
//! `orchestrator/driver/smoke_task.rs` calls it for each task.
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Map, Value, json};

use crate::config::{DeployTarget, ProjectConfig};
use crate::lanes;
use crate::proc;

/// `deploy.staging.versionPath`'s default (06.3-review.md: "default `/version.json`").
const DEFAULT_VERSION_PATH: &str = "/version.json";
/// How much of a failed command's output is told (`tailLines(…, 60)` in smoke.ts).
const TAIL_LINES: usize = 60;

/// How long each part of a smoke task waits (a test gives short ones).
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Between polls of the deploy (06.3-review.md: "every 30 seconds").
    pub poll: Duration,
    /// How long the deploy is given before the task ends `timeout` ("No deploy within 20 minutes"): the store's
    /// `deployWaitMinutes`.
    pub wait: Duration,
    /// The dependency install's limit (10 minutes).
    pub install: Duration,
    /// The smoke command's limit (15 minutes).
    pub smoke: Duration,
}

impl Timing {
    /// The timing with `wait` (the store's `deployWaitMinutes`) for the deploy; the rest is how the smoke runs here.
    pub fn waiting(wait: Duration) -> Self {
        Self {
            poll: Duration::from_secs(30),
            wait,
            install: proc::NPM_INSTALL_TIMEOUT,
            smoke: Duration::from_secs(15 * 60),
        }
    }
}

/// How a smoke task ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ran {
    Passed,
    /// The command failed, or could not be run: the words are the tail.
    Failed(String),
    /// The deploy never served the commit.
    Timeout,
    /// Called off while it waited (the agent is stopping): nothing is told, the store sends the task again.
    Cancelled,
}

/// The project's staging target when it has a smoke at all: a `smoke` command, a url, and staging deploying from the
/// base branch (smoke.ts `smokeTarget`).
pub fn enabled(cfg: &ProjectConfig) -> Option<&DeployTarget> {
    let staging = cfg.deploy.staging.as_ref()?;
    let url = staging.url.as_deref().map_or("", str::trim);
    let command = staging.smoke.as_deref().map_or("", str::trim);
    (!url.is_empty() && !command.is_empty() && staging.branch == cfg.base_branch).then_some(staging)
}

/// Checks a store's smoke task against this computer's own settings (smoke.ts `runSmokeTask`): the commit it names,
/// or in words why it is refused. The server's word is not the only check: the url and the command must be the ones
/// this project's settings hold (the command is one its owner approved), and it is never the server's text that runs.
pub fn accept(cfg: &ProjectConfig, args: &Map<String, Value>) -> Result<String, String> {
    let Some(target) = enabled(cfg) else {
        return Err("This project has no staging smoke here.".into());
    };
    let sha = args.get("sha").and_then(Value::as_str).unwrap_or_default();
    if !valid_sha(sha) {
        return Err("The task names no commit.".into());
    }
    if let Some(url) = args.get("url").and_then(Value::as_str)
        && url.trim_end_matches('/') != target.url.as_deref().map_or("", str::trim).trim_end_matches('/')
    {
        return Err("The task's url isn't this project's staging url.".into());
    }
    if let Some(command) = args.get("command").and_then(Value::as_str)
        && command.trim() != target.smoke.as_deref().map_or("", str::trim)
    {
        return Err("The task's command isn't this project's smoke command.".into());
    }
    Ok(sha.to_string())
}

/// 7 to 40 hex digits (a commit's name, never a ref or a flag).
fn valid_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The body of the task's end (`jobs::end_body`'s shape): `ok: true` with `result { sha, outcome, tail? }` for every
/// outcome, which the store turns into the `smoke` fact; nothing for a task called off.
pub fn end_body(sha: &str, ran: &Ran) -> Option<Value> {
    let (outcome, tail) = match ran {
        Ran::Passed => ("passed", None),
        Ran::Failed(tail) => ("failed", Some(tail)),
        Ran::Timeout => ("timeout", None),
        Ran::Cancelled => return None,
    };
    let mut result = json!({ "sha": sha, "outcome": outcome });
    if let Some(tail) = tail {
        result["tail"] = json!(tail);
    }
    Some(json!({ "ok": true, "result": result }))
}

/// Waits for staging (`<url><versionPath>`, this project's own url and path) to serve `sha` or a later commit, then runs
/// the project's own install and smoke command in the reused smoke worktree detached at `sha`. Blocking; it stops,
/// answering [`Ran::Cancelled`], once `cancelled` says so while it waits (a smoke command already running is let finish).
/// A local problem (git, the filesystem) is a `Failed` with its words, as smoke.ts words an error.
pub fn run_with(
    repo_path: &Path,
    worktrees_dir: &Path,
    target: &DeployTarget,
    sha: &str,
    timing: &Timing,
    cancelled: &dyn Fn() -> bool,
) -> Ran {
    let url = target.url.as_deref().unwrap_or_default().trim().trim_end_matches('/');
    let version_path = target
        .version_path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(DEFAULT_VERSION_PATH);
    let version_path = if version_path.starts_with('/') {
        version_path.to_string()
    } else {
        format!("/{version_path}")
    };
    let http = reqwest::blocking::Client::new();
    match wait_until(
        || poll_once(&http, repo_path, url, &version_path, sha),
        |wait| sleep_unless(wait, cancelled),
        timing.poll,
        timing.wait,
        cancelled,
    ) {
        Wait::Cancelled => return Ran::Cancelled,
        Wait::TimedOut => return Ran::Timeout,
        Wait::Live => {}
    }
    let command = target.smoke.as_deref().unwrap_or_default().trim();
    let dir = match lanes::prepare_smoke_checkout(repo_path, worktrees_dir, sha) {
        Ok(dir) => dir,
        Err(e) => return Ran::Failed(format!("{e:#}")),
    };
    match run_smoke_command(&dir, command, timing) {
        Ok(None) => Ran::Passed,
        Ok(Some(tail)) => Ran::Failed(tail),
        Err(e) => Ran::Failed(format!("{e:#}")),
    }
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
    let Some(deployed_sha) = parse_deployed_sha(&body).filter(|s| valid_sha(s)) else {
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

/// Polls `poll` up to `timeout`/`interval` times, sleeping `interval` between attempts (never after the last one),
/// stopping as soon as `poll` answers true, and looking at `cancelled` before each poll. `poll` and `sleep` are injected so
/// a test runs every attempt without real waiting.
fn wait_until(
    mut poll: impl FnMut() -> bool,
    mut sleep: impl FnMut(Duration),
    interval: Duration,
    timeout: Duration,
    cancelled: &dyn Fn() -> bool,
) -> Wait {
    let max_polls = (timeout.as_millis() / interval.as_millis().max(1)).max(1);
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

/// Installs if the checkout needs it (the same rule as the base checkout's, section 6.8), then runs `command`. `None`
/// when it passed, else the last lines of the output (an install that failed, or a command that failed or timed out,
/// says which in its first line).
fn run_smoke_command(dir: &Path, command: &str, timing: &Timing) -> anyhow::Result<Option<String>> {
    use anyhow::Context;
    if proc::needs_npm_install(dir) {
        let install = proc::npm_install_command(dir);
        let (code, output, timed_out) = proc::run_command_with_timeout(install, dir, timing.install)
            .with_context(|| format!("running `{install}`"))?;
        if code != 0 || timed_out {
            let what = if timed_out {
                "timed out".to_string()
            } else {
                format!("failed ({code})")
            };
            return Ok(Some(format!(
                "`{install}` {what}:\n{}",
                proc::tail_lines(&output, TAIL_LINES)
            )));
        }
    }
    let (code, output, timed_out) =
        proc::run_command_with_timeout(command, dir, timing.smoke).with_context(|| format!("running `{command}`"))?;
    let out = proc::tail_lines(&output, TAIL_LINES);
    Ok(if timed_out {
        Some(format!(
            "Timed out after {} minutes.\n{out}",
            timing.smoke.as_secs() / 60
        ))
    } else if code != 0 {
        Some(out)
    } else {
        None
    })
}

#[cfg(test)]
mod tests;
