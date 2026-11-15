//! Telling a failing check of a run's lane from one that fails on main too (06.7-chores-never-idle.md, "The
//! baseline"). The lane's checks have run once; this asks the baseline for `origin/<base>`'s commit and, while
//! something else has the base checkout, asks again every 30 seconds without running the lane's checks again.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use crate::baseline::{self, Baseline};
use crate::checks::CheckRun;
use crate::config::CheckConfig;

/// How often a busy baseline is asked again.
pub(super) const RETRY_EVERY: Duration = Duration::from_secs(30);
/// How long a busy baseline is waited for before the failures count against the change.
pub(super) const GIVE_UP_AFTER: Duration = Duration::from_secs(10 * 60);

/// What asking the baseline gave, as far as the wait cares.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Answer {
    Busy,
    /// Main moved on while asking: ask again for its new commit.
    Stale,
    Done,
    /// The baseline itself couldn't run.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Decision {
    /// Compare with the baseline's checks.
    Compare,
    /// Ask again after this long.
    Again(Duration),
    /// The failures count against the change.
    Blame,
}

/// What to do after `answer`, `elapsed` since the first failure was seen.
pub(super) fn decide(elapsed: Duration, answer: Answer) -> Decision {
    match answer {
        Answer::Done => Decision::Compare,
        Answer::Failed => Decision::Blame,
        _ if elapsed >= GIVE_UP_AFTER => Decision::Blame,
        Answer::Busy => Decision::Again(RETRY_EVERY),
        Answer::Stale => Decision::Again(Duration::ZERO),
    }
}

/// Where the baseline runs for a project.
pub(super) struct BaselineStage<'a> {
    pub project: &'a str,
    pub repo_path: &'a Path,
    pub worktrees_dir: &'a Path,
    pub checks: &'a [CheckConfig],
}

impl BaselineStage<'_> {
    /// Marks the failures in `runs` that also fail on `origin/<base_branch>`'s commit as `preexisting`, and returns
    /// the notes to keep. A failure that can't be compared stays the change's.
    pub(super) async fn mark(&self, lane: &Path, base_branch: &str, runs: &mut [CheckRun]) -> Vec<String> {
        let started = tokio::time::Instant::now();
        let (lane, base_branch) = (lane.to_path_buf(), base_branch.to_string());
        let ask = || {
            let (stage, lane, base) = (self.owned(), lane.clone(), base_branch.clone());
            async move {
                tokio::task::spawn_blocking(move || {
                    let sha = crate::lanes::ref_sha(&lane, &format!("origin/{base}"))?;
                    baseline::baseline(&stage.0, &stage.1, &stage.2, &base, &sha, &stage.3)
                })
                .await?
            }
        };
        compare(ask, || started.elapsed(), |d| tokio::time::sleep(d), runs).await
    }

    fn owned(&self) -> (String, PathBuf, PathBuf, Vec<CheckConfig>) {
        (
            self.project.to_string(),
            self.repo_path.to_path_buf(),
            self.worktrees_dir.to_path_buf(),
            self.checks.to_vec(),
        )
    }
}

/// [`BaselineStage::mark`] with the baseline, the clock and the sleep passed in.
async fn compare<A, AF, S, SF>(
    mut ask: A,
    elapsed: impl Fn() -> Duration,
    mut sleep: S,
    runs: &mut [CheckRun],
) -> Vec<String>
where
    A: FnMut() -> AF,
    AF: std::future::Future<Output = Result<Baseline>>,
    S: FnMut(Duration) -> SF,
    SF: std::future::Future<Output = ()>,
{
    let mut notes = Vec::new();
    loop {
        let answer = ask().await;
        let kind = match &answer {
            Ok(Baseline::Busy) => Answer::Busy,
            Ok(Baseline::Stale { .. }) => Answer::Stale,
            Ok(Baseline::Done(_)) => Answer::Done,
            Err(_) => Answer::Failed,
        };
        match decide(elapsed(), kind) {
            Decision::Compare => {
                if let Ok(Baseline::Done(main)) = answer {
                    baseline::mark_preexisting(runs, &main);
                }
                return notes;
            }
            Decision::Blame => {
                match answer {
                    Err(e) => notes.push(format!("Couldn't run main's checks to compare: {e:#}")),
                    _ => notes.push("Couldn't compare with main in time; the failures count against the change".into()),
                }
                return notes;
            }
            Decision::Again(after) => {
                if kind == Answer::Busy {
                    notes.push("Couldn't compare with main yet; checking again shortly".into());
                    sleep(after).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Arc;

    fn run(name: &str, ok: bool) -> CheckRun {
        CheckRun {
            name: name.into(),
            command: name.into(),
            blocking: true,
            exit_code: i32::from(!ok),
            timed_out: false,
            output_tail: String::new(),
            preexisting: false,
            seconds: 0.0,
        }
    }

    #[test]
    fn decide_is_a_function_of_elapsed_and_the_answer() {
        let at = |s| Duration::from_secs(s);
        assert_eq!(decide(at(0), Answer::Done), Decision::Compare);
        assert_eq!(decide(at(9999), Answer::Done), Decision::Compare);
        assert_eq!(decide(at(0), Answer::Failed), Decision::Blame);
        assert_eq!(decide(at(0), Answer::Busy), Decision::Again(RETRY_EVERY));
        assert_eq!(decide(at(599), Answer::Busy), Decision::Again(RETRY_EVERY));
        assert_eq!(decide(at(600), Answer::Busy), Decision::Blame);
        assert_eq!(decide(at(0), Answer::Stale), Decision::Again(Duration::ZERO));
        assert_eq!(decide(at(600), Answer::Stale), Decision::Blame);
    }

    #[tokio::test]
    async fn a_failure_that_fails_on_main_too_is_marked_and_one_that_only_fails_here_is_not() {
        let mut runs = vec![run("lint", false), run("test", false), run("build", true)];
        let main = Arc::new(vec![run("lint", false), run("test", true)]);
        let notes = compare(
            || async { Ok(Baseline::Done(main.clone())) },
            || Duration::ZERO,
            |_| async {},
            &mut runs,
        )
        .await;
        assert!(notes.is_empty());
        assert!(runs[0].preexisting);
        assert!(!runs[1].preexisting);
        assert!(!runs[2].preexisting);
    }

    #[tokio::test]
    async fn a_busy_baseline_is_asked_every_30_seconds_until_it_answers() {
        let clock = Cell::new(Duration::ZERO);
        let asks = Cell::new(0);
        let mut runs = vec![run("lint", false)];
        let main = Arc::new(vec![run("lint", false)]);
        let notes = compare(
            || {
                asks.set(asks.get() + 1);
                let answer = if asks.get() < 4 {
                    Ok(Baseline::Busy)
                } else {
                    Ok(Baseline::Done(main.clone()))
                };
                async move { answer }
            },
            || clock.get(),
            |d| {
                assert_eq!(d, Duration::from_secs(30));
                clock.set(clock.get() + d);
                async {}
            },
            &mut runs,
        )
        .await;
        assert_eq!(asks.get(), 4);
        assert_eq!(clock.get(), Duration::from_secs(90));
        assert_eq!(notes.len(), 3);
        assert!(notes[0].contains("Couldn't compare with main yet; checking again shortly"));
        assert!(runs[0].preexisting);
    }

    #[tokio::test]
    async fn a_baseline_busy_for_10_minutes_counts_the_failures_against_the_change() {
        let clock = Cell::new(Duration::ZERO);
        let asks = Cell::new(0);
        let mut runs = vec![run("lint", false)];
        compare(
            || {
                asks.set(asks.get() + 1);
                async { Ok(Baseline::Busy) }
            },
            || clock.get(),
            |d| {
                clock.set(clock.get() + d);
                async {}
            },
            &mut runs,
        )
        .await;
        assert_eq!(asks.get(), 21);
        assert!(!runs[0].preexisting);
    }

    #[tokio::test]
    async fn a_baseline_that_errors_counts_the_failures_at_once() {
        let mut runs = vec![run("lint", false)];
        let notes = compare(
            || async { Err(anyhow::anyhow!("no checkout")) },
            || Duration::ZERO,
            |_| async { panic!("no waiting") },
            &mut runs,
        )
        .await;
        assert!(!runs[0].preexisting);
        assert!(notes[0].contains("no checkout"));
    }
}
