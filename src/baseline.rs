//! The baseline (docs/specs/agent/06.7-chores-never-idle.md, "The baseline"): the project's checks
//! run once in the base checkout at the base branch's current commit, so a check that fails on a
//! pull request can be told apart from one that already fails on main. The result is cached per
//! commit in memory; the base checkout is taken through [`crate::base_hold::hold_base`], one user
//! at a time, and a busy one answers [`Baseline::Busy`] rather than waiting.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::Result;

use crate::base_hold::hold_base;
use crate::checks::{self, CheckRun};
use crate::config::CheckConfig;
use crate::lanes;
use crate::proc;
use crate::sync::lock;

type Cache = Mutex<HashMap<(String, String), Arc<Vec<CheckRun>>>>;

static CACHE: LazyLock<Cache> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// What asking for a baseline gave.
#[derive(Debug, Clone, PartialEq)]
pub enum Baseline {
    /// Something else has the base checkout; ask again shortly.
    Busy,
    /// Every check that ran on `base_sha` (the install first when it failed), passed or not.
    Done(Arc<Vec<CheckRun>>),
}

/// Runs `project`'s `checks` at `base_sha` in the base checkout (reset onto `origin/<base_branch>`,
/// installing dependencies first when it needs it), unless that commit was already run.
pub fn baseline(
    project: &str,
    repo_path: &Path,
    worktrees_dir: &Path,
    base_branch: &str,
    base_sha: &str,
    checks: &[CheckConfig],
) -> Result<Baseline> {
    let key = (project.to_string(), base_sha.to_string());
    if let Some(done) = lock(&CACHE).get(&key) {
        return Ok(Baseline::Done(done.clone()));
    }
    let Ok(_hold) = hold_base(project, "the baseline") else {
        return Ok(Baseline::Busy);
    };
    let dir = lanes::prepare_base_checkout(repo_path, worktrees_dir, base_branch)?;
    let runs = Arc::new(checks::run_all(&dir, checks, &proc::run_command_with_timeout));
    lock(&CACHE).insert(key, runs.clone());
    Ok(Baseline::Done(runs))
}

/// Marks each failed check in `runs` that also failed in `baseline` (by name) as `preexisting`.
pub fn mark_preexisting(runs: &mut [CheckRun], baseline: &[CheckRun]) {
    for run in runs.iter_mut().filter(|r| !r.passed()) {
        run.preexisting = baseline.iter().any(|b| b.name == run.name && !b.passed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git, repo_with_origin, run as run_git};

    fn check(name: &str, command: &str) -> CheckConfig {
        CheckConfig {
            name: name.to_string(),
            command: command.to_string(),
            blocking: true,
            timeout_sec: 30,
        }
    }

    /// Checks that each append a line to `counter`; `bad` also fails.
    fn counted(counter: &Path) -> Vec<CheckConfig> {
        let c = counter.display();
        vec![
            check("good", &format!("echo run >> \"{c}\"")),
            check("bad", &format!("echo run >> \"{c}\" && echo broke && exit 1")),
        ]
    }

    fn runs(counter: &Path) -> usize {
        std::fs::read_to_string(counter).map_or(0, |s| s.lines().count())
    }

    #[test]
    fn a_failing_check_is_reported_failed_a_passing_one_passed_and_the_same_sha_runs_nothing_more() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        let wt = root.path().join("wt");
        let counter = root.path().join("counter");
        let checks = counted(&counter);

        let Baseline::Done(first) = baseline("bl-t1", &repo, &wt, "main", "sha1", &checks).unwrap() else {
            panic!("busy");
        };
        assert!(first[0].passed());
        assert!(!first[1].passed());
        assert!(first[1].output_tail.contains("broke"));
        assert_eq!(runs(&counter), 2);

        let Baseline::Done(again) = baseline("bl-t1", &repo, &wt, "main", "sha1", &checks).unwrap() else {
            panic!("busy");
        };
        assert_eq!(again, first);
        assert_eq!(runs(&counter), 2);

        run_git(git(&repo).args(["commit", "--quiet", "--allow-empty", "-m", "more"]));
        run_git(git(&repo).args(["push", "--quiet", "origin", "main"]));
        assert!(matches!(
            baseline("bl-t1", &repo, &wt, "main", "sha2", &checks).unwrap(),
            Baseline::Done(_)
        ));
        assert_eq!(runs(&counter), 4);
    }

    #[test]
    fn a_busy_base_checkout_answers_busy_without_running_anything() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        let counter = root.path().join("counter");
        let _held = hold_base("bl-t2", "a sync").unwrap();

        let got = baseline(
            "bl-t2",
            &repo,
            &root.path().join("wt"),
            "main",
            "sha1",
            &counted(&counter),
        )
        .unwrap();

        assert_eq!(got, Baseline::Busy);
        assert_eq!(runs(&counter), 0);
    }

    #[test]
    fn mark_preexisting_flags_only_failures_that_also_fail_on_main() {
        let run = |name: &str, code: i32| CheckRun {
            name: name.to_string(),
            command: String::new(),
            blocking: true,
            exit_code: code,
            timed_out: false,
            output_tail: String::new(),
            preexisting: false,
        };
        let mut pr = vec![run("a", 1), run("b", 1), run("c", 0)];
        mark_preexisting(&mut pr, &[run("a", 1), run("b", 0), run("c", 1)]);
        assert_eq!(
            pr.iter().map(|r| r.preexisting).collect::<Vec<_>>(),
            [true, false, false]
        );
    }
}
