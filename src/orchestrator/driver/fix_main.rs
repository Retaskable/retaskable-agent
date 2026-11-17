//! Fix main (docs/specs/agent/06.7-chores-never-idle.md, "The baseline also runs when Retaskable is idle"; `reportRedMain`
//! in the TypeScript agent's `chores.ts`): when a project has a lane free and nothing else to start, the baseline runs on
//! the base branch's head, off the loop's thread so the beats go on, and on a GitHub project the host's CI on that
//! commit is read until it settles (`crate::main_ci`). Blocking checks failing, or CI red on the host, is only
//! **reported**: `chore_trigger red_main` with the failing checks' names and the brief the fix session is given. The store
//! decides whether a fix follows (it may skip: one is open, nothing to fix at that commit, paused, chores off) and
//! sends it through `health` as a task, which runs as a chore item (`chores.rs`). Reported once per commit, and again
//! after a failed try. Only from a computer that takes tasks: a waiting `red_main` holds back plan drafts in the store, so
//! it must be one that will run the fix. Both results are kept as the store's `baseline` and `main_ci` facts. Plain code.
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::task::JoinHandle;

use super::project::ProjectLoop;
use crate::backlog::pool::{Trigger, TriggerAnswer};
use crate::backlog::runs::short_sha;
use crate::baseline::{Baseline, baseline};
use crate::checks::CheckRun;
use crate::config::OnOff;
use crate::kept_facts::{self, BASELINE, MAIN_CI};
use crate::lane_pool::LanePool;
use crate::main_ci::{self, MainCi};

/// How long after a report the store did not take it another is tried (`NO_FIX_FRESH_MS`).
const RETRY_EVERY: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct State {
    /// The baseline running off-thread, with the commit it ran on.
    task: Option<JoinHandle<Result<(String, Baseline)>>>,
    /// The base commit already dealt with: nothing more is done for it until the branch moves.
    handled: Option<String>,
    /// The baseline last run, with the commit it ran on, kept until the host's CI is in for that commit too.
    done: Option<(String, Arc<Vec<CheckRun>>)>,
    /// The host's CI on the base branch's commit as last read, and the brief built from its failed checks' logs.
    ci: Option<(MainCi, String)>,
    ci_task: Option<JoinHandle<(MainCi, String)>>,
    ci_read: Option<Instant>,
    /// When the last report the store did not take was tried.
    failed_at: Option<Instant>,
}

#[cfg(test)]
impl State {
    /// Lets a report the store did not take be tried again at once, as if the wait had passed.
    pub(super) fn retry_now(&mut self) {
        self.failed_at = None;
    }
}

impl ProjectLoop {
    /// One idle look at the base branch (the caller knows nothing else is to start).
    pub(super) async fn idle_baseline(
        &mut self,
        repo: u64,
        token: Option<&str>,
        pool: &LanePool,
        machine: &str,
        now: Instant,
    ) {
        let facts = kept_facts::shared(self.store.dir());
        if let Some(task) = self.run_loop.fix_main.ci_task.take_if(|t| t.is_finished()) {
            match task.await {
                Ok(read) => {
                    let st = &mut self.run_loop.fix_main;
                    if st.ci.as_ref().is_none_or(|(ci, _)| *ci != read.0) {
                        facts.keep_result(MAIN_CI, &read.0.to_fact());
                    }
                    self.run_loop.fix_main.ci = Some(read);
                }
                Err(e) => tracing::warn!("{}: reading main's CI failed: {e}", self.id),
            }
        }
        if let Some(task) = self.run_loop.fix_main.task.take_if(|t| t.is_finished()) {
            match task.await {
                Ok(Ok((sha, Baseline::Done(runs)))) => {
                    facts.keep_result(BASELINE, &baseline_fact(&sha, &runs));
                    self.run_loop.fix_main.done = Some((sha.clone(), runs));
                    self.decide_main(repo, token, &sha, machine, now).await;
                }
                Ok(Ok(_)) => {} // busy, or main moved on: the next pass looks again
                Ok(Err(e)) => tracing::warn!("{}: the baseline didn't run: {e:#}", self.id),
                Err(e) => tracing::warn!("{}: the baseline task failed: {e}", self.id),
            }
            return;
        }
        if self.run_loop.fix_main.task.is_some()
            || self.project.chores == OnOff::Off
            || pool.claim_lane(&pool.run_want(&self.id, "")).is_none()
        {
            return;
        }
        let at = format!("origin/{}", self.project.base_branch);
        let Some(head) = super::report::git(
            std::path::Path::new(&self.project.repo_path),
            &["rev-parse", "--verify", "--quiet", &format!("{at}^{{commit}}")],
        )
        .await
        else {
            return;
        };
        if self.run_loop.fix_main.handled.as_deref() == Some(head.as_str()) {
            return;
        }
        self.read_main_ci(&head, now);
        if self
            .run_loop
            .fix_main
            .done
            .as_ref()
            .is_some_and(|(sha, _)| *sha == head)
        {
            // The baseline is in; the host's CI on this commit may have settled since.
            self.decide_main(repo, token, &head, machine, now).await;
            return;
        }
        let p = &self.project;
        let (id, repo_path, worktrees, base, checks) = (
            self.id.clone(),
            std::path::PathBuf::from(&p.repo_path),
            std::path::PathBuf::from(&p.worktrees_dir),
            p.base_branch.clone(),
            p.checks.clone(),
        );
        self.run_loop.fix_main.task = Some(tokio::task::spawn_blocking(move || {
            let mut sha = head;
            for _ in 0..2 {
                match baseline(&id, &repo_path, &worktrees, &base, &sha, &checks)? {
                    Baseline::Stale { head } => sha = head,
                    other => return Ok((sha, other)),
                }
            }
            Ok((sha, Baseline::Busy))
        }));
    }

    /// Starts reading the host's CI on `sha` off the loop's thread, unless it is settled for that commit, a read is
    /// running, the last was less than the store's `mainCiReadSeconds` ago (or the store's numbers aren't known), or the
    /// host can't be asked (not GitHub).
    fn read_main_ci(&mut self, sha: &str, now: Instant) {
        let Some(every) = self.project.tuning.store.map(|t| t.main_ci_every()) else {
            return;
        };
        let st = &mut self.run_loop.fix_main;
        if !main_ci::reads(&self.project)
            || st.ci_task.is_some()
            || st.ci.as_ref().is_some_and(|(ci, _)| ci.settled_for(sha))
            || st.ci_read.is_some_and(|t| now.duration_since(t) < every)
        {
            return;
        }
        st.ci_read = Some(now);
        let (gh, dir, repo, sha) = (
            self.run_loop.main_sync.gh(),
            std::path::PathBuf::from(&self.project.repo_path),
            self.project.github_repo.clone(),
            sha.to_string(),
        );
        self.run_loop.fix_main.ci_task = Some(tokio::task::spawn_blocking(move || {
            let ci = main_ci::read(&gh, &dir, &repo, &sha, &|_| {});
            let brief = if ci.failed.is_empty() {
                String::new()
            } else {
                main_ci::brief(&sha, &ci.failed, &main_ci::logs(&gh, &dir, &repo, &ci.failed, &|_| {}))
            };
            (ci, brief)
        }));
    }

    /// The baseline on `sha` is in: once the host's CI on it is too (at once when it can't be asked), reports main red
    /// when either is.
    async fn decide_main(&mut self, repo: u64, token: Option<&str>, sha: &str, _machine: &str, now: Instant) {
        let st = &self.run_loop.fix_main;
        let Some((_, runs)) = st.done.clone().filter(|(done, _)| done == sha) else {
            return;
        };
        let ci = st.ci.clone().filter(|(ci, _)| ci.settled_for(sha));
        if main_ci::reads(&self.project) && ci.is_none() {
            return; // still pending: read again shortly
        }
        let red: Vec<String> = ci
            .as_ref()
            .map(|(ci, _)| ci.red(sha).iter().map(|f| f.name.clone()).collect())
            .unwrap_or_default();
        let brief = ci.map(|(_, brief)| brief).filter(|_| !red.is_empty());
        self.report_red_main(repo, token, sha, &runs, red, brief, now).await;
    }

    /// Tells the store main is red at `sha` (`reportRedMain`), when the baseline's blocking checks fail or main's CI on the
    /// host did, and this computer takes the task the store may answer with. Once per commit; a try the store did not take
    /// is made again after [`RETRY_EVERY`].
    #[allow(clippy::too_many_arguments)]
    async fn report_red_main(
        &mut self,
        repo: u64,
        token: Option<&str>,
        sha: &str,
        runs: &Arc<Vec<CheckRun>>,
        red_ci: Vec<String>,
        ci_brief: Option<String>,
        now: Instant,
    ) {
        let failing: Vec<&CheckRun> = runs.iter().filter(|r| !r.passed() && r.blocking).collect();
        if failing.is_empty() && red_ci.is_empty() {
            self.live.set_fix_main(&self.id, None);
            self.run_loop.fix_main.handled = Some(sha.into());
            return;
        }
        if !self.takes_tasks {
            // This computer would not run the fix, so it does not ask for one: another computer reports it.
            self.run_loop.fix_main.handled = Some(sha.into());
            return;
        }
        if self
            .run_loop
            .fix_main
            .failed_at
            .is_some_and(|t| now.duration_since(t) < RETRY_EVERY)
        {
            return;
        }
        let trigger = Trigger::RedMain {
            branch: self.project.base_branch.clone(),
            sha: sha.to_string(),
            checks: failing.iter().map(|r| r.name.clone()).chain(red_ci).collect(),
            brief: fix_report(sha, &failing, ci_brief.as_deref()),
        };
        match self.client.chore_trigger(repo, token, &trigger).await {
            Ok(TriggerAnswer::Skipped(why)) => {
                tracing::info!(
                    "{}: main is red at {}; the store sends no fix: {why}",
                    self.id,
                    short_sha(sha)
                );
                if why.contains("nothing to fix") {
                    self.note_no_fix(sha);
                } else {
                    self.live.set_fix_main(&self.id, None);
                }
            }
            Ok(_) => self.live.set_fix_main(&self.id, None),
            Err(e) => {
                self.store_error("chore_trigger", &e);
                self.run_loop.fix_main.failed_at = Some(now);
                return;
            }
        }
        self.run_loop.fix_main.handled = Some(sha.into());
    }

    fn note_no_fix(&mut self, sha: &str) {
        let note = format!(
            "Fixing main found nothing to fix at {}. It tries again when main moves.",
            short_sha(sha)
        );
        self.live.set_fix_main(&self.id, Some(note));
    }
}

/// A fix main's brief: each failing baseline check's last lines, then main's CI brief from the host.
fn fix_report(sha: &str, failing: &[&CheckRun], ci_brief: Option<&str>) -> String {
    let mut parts: Vec<String> = failing
        .iter()
        .map(|r| {
            format!(
                "{} failed on main at {}:\n\n{}",
                r.name,
                short_sha(sha),
                r.output_tail.trim_end()
            )
        })
        .collect();
    parts.extend(ci_brief.map(str::to_string));
    parts.join("\n\n")
}

/// The store's `baseline` fact: `{ sha, results: [{ name, blocking, ok, durationSec }], at }`. The checks' output is
/// left out: a fact holds derived results only (spec 03.16), and the brief a fix main needs is built from the run
/// kept here. (The TS agent's results carry the output's `tail` too.)
fn baseline_fact(sha: &str, runs: &[CheckRun]) -> serde_json::Value {
    serde_json::json!({
        "sha": sha,
        "results": runs.iter().map(|r| serde_json::json!({
            "name": r.name,
            "blocking": r.blocking,
            "ok": r.passed(),
            "durationSec": r.seconds,
        })).collect::<Vec<_>>(),
        "at": crate::telemetry::events::iso_utc_millis(crate::telemetry::events::now_ms()),
    })
}
