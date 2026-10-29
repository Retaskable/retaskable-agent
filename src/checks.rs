//! Stage 1: deterministic checks (docs/specs/agent/06.3-review.md, "Stage 1: deterministic
//! checks") — orchestrator code, never a model. First, an npm install if the lane needs one
//! (the same rule as the base checkout's, section 6.8); a failed install counts as a failed
//! blocking check named after the command, and the configured `checks` don't run. Otherwise
//! every `checks` command (section 10) runs in the lane, outside the sandbox (section 4.2), each
//! with its own timeout, keeping its exit code and the last ~200 lines of output. Alongside that,
//! `git diff --stat origin/<baseBranch>...HEAD`, the changed files, and whether the backlog file
//! is among them. [`StageOneResult::outcome`] is what `item_lifecycle::ItemRun::checks_finished`
//! (already ported) needs to decide whether the slice goes on to review or back to
//! `implementing`; wiring that decision into a real lane is the not-yet-built orchestrator loop's
//! job.
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::CheckConfig;
use crate::item_lifecycle::CheckOutcome;
use crate::lanes;
use crate::proc;

/// How much of a check's output stage 1 keeps (06.3-review.md: "the last ~200 lines").
const TAIL_LINES: usize = 200;

/// One command's result: a configured `checks` entry, or the npm install itself when it fails
/// (counted as "a failed blocking check named after the command", section 06.3-review.md).
#[derive(Debug, Clone, PartialEq)]
pub struct CheckRun {
    pub name: String,
    pub command: String,
    pub blocking: bool,
    pub exit_code: i32,
    pub timed_out: bool,
    pub output_tail: String,
}

impl CheckRun {
    pub fn passed(&self) -> bool {
        self.exit_code == 0 && !self.timed_out
    }
}

/// Stage 1's full evidence for a lane at its current head.
#[derive(Debug, Clone, PartialEq)]
pub struct StageOneResult {
    /// In the order they ran: the install first if the lane needed one, then every configured
    /// check — empty if the install failed.
    pub checks: Vec<CheckRun>,
    pub diff_stat: String,
    pub changed_files: Vec<String>,
    pub backlog_changed: bool,
}

impl StageOneResult {
    /// The blocking checks that didn't pass, in the order they ran.
    pub fn blocking_failures(&self) -> Vec<&CheckRun> {
        self.checks.iter().filter(|c| c.blocking && !c.passed()).collect()
    }

    /// `Failed` when any blocking check (or a failed install, which counts as one) didn't pass;
    /// a non-blocking failure is only evidence for the later review stages.
    pub fn outcome(&self) -> CheckOutcome {
        if self.blocking_failures().is_empty() {
            CheckOutcome::Passed
        } else {
            CheckOutcome::Failed
        }
    }
}

/// Runs stage 1 in `lane_dir`, already checked out at the slice's head.
pub fn run(lane_dir: &Path, base_branch: &str, checks: &[CheckConfig], backlog_path: &str) -> Result<StageOneResult> {
    run_with(
        lane_dir,
        base_branch,
        checks,
        backlog_path,
        proc::run_command_with_timeout,
    )
}

/// [`run`] with the command runner injected, so tests don't need a real `npm` or the configured
/// checks' real commands.
fn run_with(
    lane_dir: &Path,
    base_branch: &str,
    checks: &[CheckConfig],
    backlog_path: &str,
    runner: impl Fn(&str, &Path, Duration) -> Result<(i32, String, bool)>,
) -> Result<StageOneResult> {
    let mut runs = Vec::new();
    if proc::needs_npm_install(lane_dir) {
        let command = proc::npm_install_command(lane_dir);
        let install = run_timed(lane_dir, &runner, command, command, true, proc::NPM_INSTALL_TIMEOUT);
        let install_failed = !install.passed();
        runs.push(install);
        if install_failed {
            return finish(lane_dir, base_branch, backlog_path, runs);
        }
    }
    for check in checks {
        runs.push(run_timed(
            lane_dir,
            &runner,
            &check.name,
            &check.command,
            check.blocking,
            Duration::from_secs(check.timeout_sec),
        ));
    }
    finish(lane_dir, base_branch, backlog_path, runs)
}

fn finish(lane_dir: &Path, base_branch: &str, backlog_path: &str, checks: Vec<CheckRun>) -> Result<StageOneResult> {
    let changed_files = changed_files(lane_dir, base_branch)?;
    let backlog_changed = changed_files.iter().any(|f| f == backlog_path);
    Ok(StageOneResult {
        checks,
        diff_stat: diff_stat(lane_dir, base_branch)?,
        changed_files,
        backlog_changed,
    })
}

fn run_timed(
    lane_dir: &Path,
    runner: &impl Fn(&str, &Path, Duration) -> Result<(i32, String, bool)>,
    name: &str,
    command: &str,
    blocking: bool,
    timeout: Duration,
) -> CheckRun {
    match runner(command, lane_dir, timeout) {
        Ok((exit_code, output, timed_out)) => CheckRun {
            name: name.to_string(),
            command: command.to_string(),
            blocking,
            exit_code,
            timed_out,
            output_tail: proc::tail_lines(&output, TAIL_LINES),
        },
        // The command itself couldn't be started at all (e.g. the shell is missing), rather than
        // running and failing: still a failed check, not a reason to abort stage 1.
        Err(e) => CheckRun {
            name: name.to_string(),
            command: command.to_string(),
            blocking,
            exit_code: -1,
            timed_out: false,
            output_tail: format!("{e:#}"),
        },
    }
}

fn diff_stat(lane_dir: &Path, base_branch: &str) -> Result<String> {
    let range = format!("origin/{base_branch}...HEAD");
    lanes::git_output_in_worktree(lane_dir, &["diff", "--stat", &range]).context("git diff --stat")
}

fn changed_files(lane_dir: &Path, base_branch: &str) -> Result<Vec<String>> {
    let range = format!("origin/{base_branch}...HEAD");
    let out =
        lanes::git_output_in_worktree(lane_dir, &["diff", "--name-only", &range]).context("git diff --name-only")?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git, repo_with_origin, run as run_git};

    fn ok(_: &str, _: &Path, _: Duration) -> Result<(i32, String, bool)> {
        Ok((0, String::new(), false))
    }

    fn commit(repo: &Path, file: &str, contents: &str) {
        let path = repo.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        run_git(git(repo).args(["add", file]));
        run_git(git(repo).args(["commit", "--quiet", "-m", file]));
    }

    fn check(name: &str, command: &str, blocking: bool) -> CheckConfig {
        CheckConfig {
            name: name.to_string(),
            command: command.to_string(),
            blocking,
            timeout_sec: 30,
        }
    }

    #[test]
    fn run_collects_diff_evidence_and_passes_when_only_a_nonblocking_check_fails() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        commit(&repo, "docs/backlog.md", "- an item");
        commit(&repo, "src/new.rs", "fn new() {}");
        let checks = vec![check("test", "exit 0", true), check("lint", "exit 1", false)];

        let result = run_with(
            &repo,
            "main",
            &checks,
            "docs/backlog.md",
            proc::run_command_with_timeout,
        )
        .unwrap();

        assert_eq!(result.outcome(), CheckOutcome::Passed);
        assert!(result.blocking_failures().is_empty());
        assert_eq!(result.checks.len(), 2);
        assert!(result.checks[0].passed());
        assert!(!result.checks[1].passed());
        assert!(result.changed_files.contains(&"docs/backlog.md".to_string()));
        assert!(result.changed_files.contains(&"src/new.rs".to_string()));
        assert!(result.backlog_changed);
        assert!(result.diff_stat.contains("2 files changed"), "{}", result.diff_stat);
    }

    #[test]
    fn run_fails_when_a_blocking_check_fails_and_still_reports_the_diff() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        commit(&repo, "src/new.rs", "fn new() {}");
        let checks = vec![check("test", "exit 1", true)];

        let result = run_with(
            &repo,
            "main",
            &checks,
            "docs/backlog.md",
            proc::run_command_with_timeout,
        )
        .unwrap();

        assert_eq!(result.outcome(), CheckOutcome::Failed);
        assert_eq!(result.blocking_failures().len(), 1);
        assert!(!result.backlog_changed);
        assert!(!result.diff_stat.is_empty());
    }

    #[test]
    fn run_skips_the_configured_checks_when_the_install_fails() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        std::fs::write(repo.join("package.json"), "{}").unwrap();
        commit(&repo, "src/new.rs", "fn new() {}");
        let calls = std::cell::RefCell::new(Vec::new());
        let runner = |command: &str, _: &Path, _: Duration| {
            calls.borrow_mut().push(command.to_string());
            Ok((1, "boom".to_string(), false))
        };
        let checks = vec![check("test", "exit 0", true)];

        let result = run_with(&repo, "main", &checks, "docs/backlog.md", runner).unwrap();

        assert_eq!(calls.borrow().as_slice(), ["npm install".to_string()]);
        assert_eq!(result.checks.len(), 1);
        assert_eq!(result.checks[0].name, "npm install");
        assert!(result.checks[0].blocking);
        assert!(!result.checks[0].passed());
        assert!(result.checks[0].output_tail.contains("boom"));
        assert_eq!(result.outcome(), CheckOutcome::Failed);
    }

    #[test]
    fn run_installs_first_then_runs_the_checks_when_the_install_succeeds() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        std::fs::write(repo.join("package.json"), "{}").unwrap();
        commit(&repo, "src/new.rs", "fn new() {}");
        let checks = vec![check("test", "exit 0", true)];

        let result = run_with(&repo, "main", &checks, "docs/backlog.md", ok).unwrap();

        assert_eq!(result.checks.len(), 2);
        assert_eq!(result.checks[0].name, "npm install");
        assert_eq!(result.checks[1].name, "test");
        assert_eq!(result.outcome(), CheckOutcome::Passed);
    }

    #[test]
    fn a_timed_out_check_does_not_pass_even_with_exit_code_zero() {
        let run = CheckRun {
            name: "slow".to_string(),
            command: "sleep 100".to_string(),
            blocking: true,
            exit_code: 0,
            timed_out: true,
            output_tail: String::new(),
        };
        assert!(!run.passed());
    }

    #[test]
    fn outcome_is_passed_when_there_are_no_checks_at_all() {
        let result = StageOneResult {
            checks: Vec::new(),
            diff_stat: String::new(),
            changed_files: Vec::new(),
            backlog_changed: false,
        };
        assert_eq!(result.outcome(), CheckOutcome::Passed);
    }
}
