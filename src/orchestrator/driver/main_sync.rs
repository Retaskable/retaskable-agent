//! The main sync (docs/specs/agent/06.7-chores-never-idle.md, "A main sync is a chore too"; `maybeSyncMain` in the TypeScript
//! agent's `orchestratorSync.ts`): about once a minute a project whose base branch is behind the default branch tells the
//! store (`chore_trigger behind_main`), which makes the `sync_main` task (one per branch at a time) and sends it back to
//! this computer through `health`. The task is run by `chores.rs` (`sync_task`): the merge in its own sync worktree, plain
//! code, then the item through CI and the checks with no review. Only from a computer that takes tasks: the task is made for
//! the computer that asks.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::project::ProjectLoop;
use crate::backlog::pool::{Trigger, TriggerAnswer};
use crate::backlog::runs::LockTake;
use crate::config::SyncMain;
use crate::git_bin::command;
use crate::intents::main_pr::Why;
use crate::main_sync::{Attempt, attempt, behind_count, pr_body, pr_title, push, sync_due};
use crate::merge_update::run as git;
use crate::runs::chore::{ChoreKind, chore_lock_name};
use crate::team_branch::need_team_branch;

/// How often a project looks at whether main moved.
const LOOK_EVERY: Duration = Duration::from_secs(60);

/// The sync worktree's folder under the project's `worktrees_dir`.
pub(super) const WORKTREE: &str = "sync-main";

/// The store's `sync_main-<base>` lock as a team-branch request holds it (`hold_sync_lock`).
pub(super) enum SyncLock {
    /// Taken here: the token to give back (`release_sync_lock`).
    Mine(String),
    /// A main sync is running here, or another computer holds the lock.
    Busy,
}

/// What the merge attempt came to.
pub(super) enum Done {
    Clean { url: String },
    Conflict { files: Vec<String> },
}

pub(super) struct State {
    looked: Option<Instant>,
    /// The main commit a sync was last asked for (or found not due): nothing is asked again until main moves.
    handled: Option<String>,
    gh: PathBuf,
}

impl State {
    /// The `gh` program the project's code host is asked through.
    pub(super) fn gh(&self) -> PathBuf {
        self.gh.clone()
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            looked: None,
            handled: None,
            gh: PathBuf::from("gh"),
        }
    }
}

#[cfg(test)]
impl State {
    pub(super) fn with_gh(gh: PathBuf) -> Self {
        Self { gh, ..Self::default() }
    }

    /// Lets the next pass look again at once, as if the minute had passed (tests: one look that failed on a loaded
    /// machine shouldn't decide a test whose few seconds never reach the next minute's look).
    pub(super) fn look_again(&mut self) {
        self.looked = None;
    }
}

impl ProjectLoop {
    /// One pass: (not `stopping`, at most once a minute) tells the store when main moved and the base branch is behind.
    /// A failure is logged and kept as the project's sync note.
    pub(super) async fn main_sync(
        &mut self,
        repo: u64,
        token: Option<&str>,
        machine: &str,
        stopping: bool,
        now: Instant,
    ) {
        let state = &mut self.run_loop.main_sync;
        if stopping
            || !self.takes_tasks
            || state.looked.is_some_and(|t| now.duration_since(t) < LOOK_EVERY)
            || self.project.sync_main == SyncMain::Off
        {
            return;
        }
        self.run_loop.main_sync.looked = Some(now);
        if let Err(e) = self.start_sync(repo, token, machine).await {
            tracing::warn!("{}: main sync didn't start: {e:#}", self.id);
            self.live
                .set_sync_main(&self.id, Some(format!("Syncing main didn't start: {e:#}")));
        }
    }

    /// Whether a main sync is open here: its chore item (`chores.rs`) is still going.
    pub(super) fn sync_running(&self) -> bool {
        self.chores.values().any(|w| w.kind == "sync_main")
    }

    /// Takes the `sync_main-<base>` lock for a request that must not run across a main sync (the merge control and Open
    /// PR to main): the store's lock also stands for another computer's sync, which the loop's own state can't see.
    /// Beaten with the runs' locks until `release_sync_lock`. The TS agent reads only the store's items for this
    /// (`teamBranchActions.ts:88`), which a sync in flight isn't one of yet.
    pub(super) async fn hold_sync_lock(&mut self, shared: &super::Shared<'_>) -> Result<SyncLock, Why> {
        if self.sync_running() {
            return Ok(SyncLock::Busy);
        }
        let (Some(repo), Ok(Some(token))) = (self.repo, shared.links.store_token().await) else {
            return Err(Why::Refused(
                "The shared backlog isn't reachable from here yet. Try again in a minute.".into(),
            ));
        };
        let name = chore_lock_name(ChoreKind::SyncMain, &self.project.base_branch).unwrap_or_default();
        let note = format!("merge control on {}", shared.links.machine);
        match self.client.lock_take(repo, Some(&token), &name, Some(&note)).await {
            Ok(LockTake::Held(lock)) => {
                self.run_loop.runs().locks().add(&lock, &name, Instant::now());
                Ok(SyncLock::Mine(lock))
            }
            Ok(LockTake::HeldBy { .. }) => Ok(SyncLock::Busy),
            Err(e) => {
                self.store_error("lock_take", &e);
                Err(Why::Failed(format!(
                    "Couldn't ask the store whether main is syncing: {e}"
                )))
            }
        }
    }

    /// Gives back a lock `hold_sync_lock` took.
    pub(super) async fn release_sync_lock(&mut self, shared: &super::Shared<'_>, lock: &str) {
        self.run_loop.runs().locks().remove(lock);
        let (Some(repo), Ok(Some(token))) = (self.repo, shared.links.store_token().await) else {
            return;
        };
        if let Err(e) = self.client.lock_drop(repo, Some(&token), lock).await {
            self.store_error("lock_drop", &e);
        }
    }

    /// Finishes a restart a ship left undone (`team_branch::ship::unfinished_restart`): the pull request into main
    /// merged but the dev branch still stands at the head it merged, because a stop or a git failure came between the
    /// merge and `start_again_from_main`. Read only until there is something to do; then under the sync lock, so no
    /// other ship or sync runs across it, the lease-guarded restart. The TS agent only has the in-memory `restartNote`.
    async fn finish_restart(&mut self, repo: u64, token: Option<&str>, machine: &str, main: &str) -> bool {
        let (r, gh) = (
            PathBuf::from(&self.project.repo_path),
            self.run_loop.main_sync.gh.clone(),
        );
        let (github, base, main) = (
            self.project.github_repo.clone(),
            self.project.base_branch.clone(),
            main.to_string(),
        );
        let (r2, base2, main2) = (r.clone(), base.clone(), main.clone());
        let found = tokio::task::spawn_blocking(move || {
            crate::team_branch::ship::unfinished_restart(&gh, &r2, &github, &base2, &main2)
        })
        .await;
        let (number, head) = match found {
            Ok(Ok(Some(found))) => found,
            Ok(Ok(None)) => return false,
            Ok(Err(e)) => {
                tracing::warn!("{}: looking for an unfinished restart: {e:#}", self.id);
                return false;
            }
            Err(e) => {
                tracing::warn!("{}: looking for an unfinished restart: {e}", self.id);
                return false;
            }
        };
        let name = chore_lock_name(ChoreKind::SyncMain, &base).unwrap_or_default();
        let note = format!("restarting {base} on {machine}");
        let lock = match self.client.lock_take(repo, token, &name, Some(&note)).await {
            Ok(LockTake::Held(lock)) => lock,
            Ok(LockTake::HeldBy { .. }) => return false,
            Err(e) => {
                self.store_error("lock_take", &e);
                return false;
            }
        };
        self.run_loop.runs().locks().add(&lock, &name, Instant::now());
        let (b, m) = (base.clone(), main.clone());
        let warning =
            tokio::task::spawn_blocking(move || crate::team_branch::ship::start_again_from_main(&r, &b, &m, &head))
                .await
                .unwrap_or_else(|e| Some(e.to_string()));
        match warning {
            None => tracing::info!(
                "{}: PR #{number} into {main} had merged: started {base} again from {main}",
                self.id
            ),
            Some(why) => tracing::warn!(
                "{}: PR #{number} into {main} had merged; {base} wasn't restarted: {why}",
                self.id
            ),
        }
        self.run_loop.runs().locks().remove(&lock);
        if let Err(e) = self.client.lock_drop(repo, token, &lock).await {
            self.store_error("lock_drop", &e);
        }
        true
    }

    /// The look: fetches, counts how far the base is behind main and, when a sync is due and main has not been asked for
    /// already, tells the store (`chore_trigger behind_main`). The store makes the task, or skips it (paused, one open).
    async fn start_sync(&mut self, repo: u64, token: Option<&str>, machine: &str) -> Result<()> {
        if self.seen.default_branch.is_none() {
            self.seen.default_branch =
                super::report::default_branch(&self.project.github_repo, std::path::Path::new(&self.project.repo_path))
                    .await;
        }
        let Some(main) = self.seen.default_branch.clone() else {
            anyhow::bail!("gh couldn't say the default branch");
        };
        let p = &self.project;
        let (repo_path, base) = (PathBuf::from(&p.repo_path), p.base_branch.clone());
        if base == main {
            return Ok(());
        }
        let (r, b, m) = (repo_path.clone(), base.clone(), main.clone());
        let (behind, sha) = tokio::task::spawn_blocking(move || -> Result<(u32, String)> {
            crate::lanes::fetch_latest(&r)?;
            Ok((
                behind_count(&r, &b, &m)?,
                git(&command, &r, &["rev-parse", &format!("origin/{m}")])?,
            ))
        })
        .await??;
        // The counts above are from before a restart moved the base, so a pass that restarted looks again next time.
        if self.finish_restart(repo, token, machine, &main).await {
            return Ok(());
        }
        if self.run_loop.main_sync.handled.as_deref() == Some(sha.as_str()) {
            return Ok(());
        }
        if !sync_due(&base, &main, behind, self.project.sync_main, self.sync_running()) {
            return Ok(());
        }
        let asked = ask_for_sync(self.client.clone(), self.id.clone(), repo, token, &base, &main, behind).await;
        match asked {
            Ok(TriggerAnswer::Skipped(why)) => {
                tracing::info!("{}: the store makes no sync of {main} into {base}: {why}", self.id);
                self.run_loop.main_sync.handled = Some(sha);
            }
            Ok(_) => {
                self.run_loop.main_sync.handled = Some(sha);
                self.live.set_sync_main(&self.id, None);
            }
            Err(e) => {
                self.store_error("chore_trigger", &e);
                self.live.set_sync_main(
                    &self.id,
                    Some(format!("Couldn't ask the store to sync {main} into {base}: {e}")),
                );
            }
        }
        Ok(())
    }

    /// A person's Sync now (`sync_now.rs`; `syncNow` in `teamBranchActions.ts`): asks the store for the sync at once, past the
    /// minute's gate, `handled` and `syncMain` being off, in the TS agent's order of refusals. Answers the id of the task the
    /// store made, which `health` sends back to this computer to run.
    pub(super) async fn start_sync_now(
        &mut self,
        repo: u64,
        token: Option<&str>,
        _machine: &str,
    ) -> Result<String, Why> {
        if self.seen.default_branch.is_none() {
            self.seen.default_branch =
                super::report::default_branch(&self.project.github_repo, Path::new(&self.project.repo_path)).await;
        }
        let main = need_team_branch(&self.project.base_branch, self.seen.default_branch.as_deref())
            .map_err(|why| Why::Refused(why.into()))?
            .to_string();
        let base = self.project.base_branch.clone();
        if !self.takes_tasks {
            return Err(Why::Refused(
                "This computer takes no tasks from the store, so it can't run the sync.".into(),
            ));
        }
        if self.sync_running() {
            return Err(Why::Refused(format!(
                "A sync of {main} into {base} is already in flight."
            )));
        }
        let (repo_path, b, m) = (PathBuf::from(&self.project.repo_path), base.clone(), main.clone());
        let behind = tokio::task::spawn_blocking(move || -> Result<u32> {
            crate::lanes::fetch_latest(&repo_path)?;
            behind_count(&repo_path, &b, &m)
        })
        .await
        .map_err(|e| Why::Failed(e.to_string()))?
        .map_err(|e| Why::Failed(format!("{e:#}")))?;
        if behind == 0 {
            return Err(Why::Refused(format!("{base} already has all of {main}.")));
        }
        match ask_for_sync(self.client.clone(), self.id.clone(), repo, token, &base, &main, behind).await {
            Ok(TriggerAnswer::Task(id)) => Ok(id),
            Ok(TriggerAnswer::Queued) => Ok(String::new()),
            Ok(TriggerAnswer::Skipped(why)) if why == "open" => Err(Why::Refused(format!(
                "A sync of {main} into {base} is already in flight."
            ))),
            Ok(TriggerAnswer::Skipped(why)) => {
                Err(Why::Refused(format!("The project is {why}, so nothing was started.")))
            }
            Err(e) => {
                self.store_error("chore_trigger", &e);
                Err(Why::Failed(format!(
                    "Couldn't ask the store to sync {main} into {base}: {e}"
                )))
            }
        }
    }
}

/// Merges main into a branch off the base in the sync worktree; when clean, pushes it and opens (or finds) its PR.
pub(super) fn sync(repo: &Path, worktree: &Path, gh: &Path, base: &str, main: &str) -> Result<Done> {
    let origin_base = format!("origin/{base}");
    if worktree.join(".git").exists() {
        git(
            &command,
            worktree,
            &["checkout", "--quiet", "--force", "--detach", &origin_base],
        )?;
    } else {
        std::fs::create_dir_all(worktree.parent().context("no worktrees folder")?)?;
        let at = worktree.to_str().context("the sync worktree's path isn't text")?;
        git(
            &command,
            repo,
            &["worktree", "add", "--quiet", "--detach", at, &origin_base],
        )?;
    }
    match attempt(worktree, base, main)? {
        Attempt::Conflict { files } => Ok(Done::Conflict { files }),
        Attempt::Clean { branch, head, commits } => {
            push(worktree, &head, &branch)?;
            let change = crate::implementer::publish::open_or_create(
                gh,
                worktree,
                &branch,
                base,
                &pr_title(base, commits.len()),
                &pr_body(&commits),
            )?;
            let _ = git(&command, worktree, &["branch", "-D", &branch]);
            Ok(Done::Clean { url: change.url })
        }
    }
}

/// `chore_trigger behind_main`: the store makes the sync task, for this computer, unless one is open or the project is paused.
async fn ask_for_sync(
    client: std::sync::Arc<crate::backlog::Client>,
    project: String,
    repo: u64,
    token: Option<&str>,
    base: &str,
    main: &str,
    behind: u32,
) -> Result<TriggerAnswer, crate::backlog::BacklogError> {
    let trigger = Trigger::BehindMain {
        branch: base.to_string(),
        main: main.to_string(),
        behind,
        project,
    };
    client.chore_trigger(repo, token, &trigger).await
}
