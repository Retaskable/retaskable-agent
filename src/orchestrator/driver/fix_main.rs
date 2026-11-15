//! Fix main (docs/specs/agent/06.7-chores-never-idle.md, "The baseline also runs when Retaskable is idle"): when a
//! project has a lane free and `run_take` handed out nothing, the baseline runs on the base branch's head, off the
//! loop's thread so the beats go on. Blocking checks failing with no fix main open: this computer takes the
//! store's `fix_main` lock and adds a fix main chore for that commit, once per commit. Plain code.
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::task::JoinHandle;

use super::project::ProjectLoop;
use crate::backlog::runs::{LockTake, short_sha};
use crate::backlog::{Item, ItemStatus};
use crate::baseline::{Baseline, baseline};
use crate::checks::CheckRun;
use crate::config::OnOff;
use crate::lane_pool::LanePool;
use crate::runs::chore::{ChoreKind, chore_lock_name};

#[derive(Default)]
pub(super) struct State {
    /// The baseline running off-thread, with the commit it ran on.
    task: Option<JoinHandle<Result<(String, Baseline)>>>,
    /// The base commit already dealt with: nothing more is done for it until the branch moves.
    handled: Option<String>,
    /// A fix main is open or was just added for `handled`: its `noFixSha` is looked for on later passes.
    awaiting: bool,
}

impl ProjectLoop {
    /// One idle look at the base branch (the caller knows `run_take` handed out nothing).
    pub(super) async fn idle_baseline(
        &mut self,
        repo: u64,
        token: Option<&str>,
        pool: &LanePool,
        machine: &str,
        now: Instant,
    ) {
        if let Some(task) = self.run_loop.fix_main.task.take_if(|t| t.is_finished()) {
            match task.await {
                Ok(Ok((sha, Baseline::Done(runs)))) => {
                    self.queue_fix_main(repo, token, &sha, &runs, machine, now).await
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
            if self.run_loop.fix_main.awaiting {
                self.look_for_no_fix(repo, token, &head).await;
            }
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

    /// The baseline on `sha` is in: queue a fix main when its blocking checks fail and none is open.
    async fn queue_fix_main(
        &mut self,
        repo: u64,
        token: Option<&str>,
        sha: &str,
        runs: &Arc<Vec<CheckRun>>,
        machine: &str,
        now: Instant,
    ) {
        let failing: Vec<&CheckRun> = runs.iter().filter(|r| !r.passed() && r.blocking).collect();
        self.run_loop.fix_main.awaiting = false;
        if !failing.is_empty() {
            let items = match self.client.list(repo, token).await {
                Ok(items) => items,
                Err(e) => return self.store_error("list", &e),
            };
            let fixes: Vec<&Item> = items.iter().filter(|i| i.kind.as_deref() == Some("fix_main")).collect();
            let open = |i: &&Item| {
                !matches!(
                    i.status,
                    ItemStatus::OnMain | ItemStatus::InProduction | ItemStatus::Skipped
                )
            };
            if fixes.iter().any(|i| i.no_fix_sha.as_deref() == Some(sha)) {
                self.note_no_fix(sha);
                self.run_loop.fix_main.handled = Some(sha.into());
                return;
            } else if fixes.iter().any(open) {
                self.run_loop.fix_main.awaiting = true; // already on it
            } else if !self.add_fix_main(repo, token, sha, &failing, machine, now).await {
                return;
            } else {
                self.run_loop.fix_main.awaiting = true;
            }
        }
        self.live.set_fix_main(&self.id, None);
        self.run_loop.fix_main.handled = Some(sha.into());
    }

    fn note_no_fix(&mut self, sha: &str) {
        let note = format!(
            "Fixing main found nothing to fix at {}. It tries again when main moves.",
            short_sha(sha)
        );
        self.live.set_fix_main(&self.id, Some(note));
        self.run_loop.fix_main.awaiting = false;
    }

    /// The fix main for the handled commit may have ended finding nothing to fix: ask the store, without the baseline.
    async fn look_for_no_fix(&mut self, repo: u64, token: Option<&str>, sha: &str) {
        match self.client.list(repo, token).await {
            Ok(items) => {
                let found = items
                    .iter()
                    .any(|i| i.kind.as_deref() == Some("fix_main") && i.no_fix_sha.as_deref() == Some(sha));
                if found {
                    self.note_no_fix(sha);
                }
            }
            Err(e) => self.store_error("list", &e),
        }
    }

    /// Takes the `fix_main` lock and adds the chore. False when it didn't happen (a holder, or the store), to try
    /// again on a later pass.
    async fn add_fix_main(
        &mut self,
        repo: u64,
        token: Option<&str>,
        sha: &str,
        failing: &[&CheckRun],
        machine: &str,
        now: Instant,
    ) -> bool {
        let name = chore_lock_name(ChoreKind::FixMain, &self.project.base_branch).unwrap_or_default();
        let note = format!("fix main on {}", machine);
        let lock = match self.client.lock_take(repo, token, &name, Some(&note)).await {
            Ok(LockTake::Held(lock)) => lock,
            Ok(LockTake::HeldBy { by, .. }) => {
                tracing::info!("{}: {by} holds {name}; leaving fix main to them", self.id);
                return false;
            }
            Err(e) => {
                self.store_error("lock_take", &e);
                return false;
            }
        };
        let locks = self.run_loop.runs().locks();
        locks.add(&lock, &name, now);
        let report = failing
            .iter()
            .map(|r| {
                format!(
                    "{} failed on main at {}:\n\n{}",
                    r.name,
                    short_sha(sha),
                    r.output_tail.trim_end()
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let id = format!("chore-fix-main-{}", short_sha(sha));
        let added = self.client.add_fix_main(repo, token, &id, sha, &report).await;
        if let Err(e) = self.client.lock_drop(repo, token, &lock).await {
            self.store_error("lock_drop", &e);
        }
        locks.remove(&lock);
        match added {
            Ok(()) => true,
            Err(e) => {
                self.store_error("add", &e);
                false
            }
        }
    }
}
