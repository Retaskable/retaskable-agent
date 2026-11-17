//! The main sync's task side (docs/specs/agent/06.7-chores-never-idle.md, "A main sync is a chore too"; `syncTask` in the
//! TypeScript agent): the store decides when a project's base branch is behind the default branch (from `health.main`, `main_facts.rs`)
//! and sends the `sync_main` task; `chores.rs` (`sync_task`) runs it: the merge in its own sync worktree here, plain
//! code, then the item through CI and the checks with no review. A person's Sync now (`start_sync_now`) asks the store
//! for the same task at once. This agent sends no other word about main being behind.
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};

use super::project::ProjectLoop;
use crate::backlog::pool::{Trigger, TriggerAnswer};
use crate::backlog::runs::LockTake;
use crate::git_bin::command;
use crate::intents::main_pr::Why;
use crate::main_sync::{Attempt, attempt, behind_count, pr_body, pr_title, push};
use crate::merge_update::run as git;
use crate::runs::chore::{ChoreKind, chore_lock_name};
use crate::team_branch::need_team_branch;

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
    gh: PathBuf,
}

impl Default for State {
    fn default() -> Self {
        Self {
            gh: PathBuf::from("gh"),
        }
    }
}

#[cfg(test)]
impl State {
    pub(super) fn with_gh(gh: PathBuf) -> Self {
        Self { gh }
    }
}

impl ProjectLoop {
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
    pub(super) async fn finish_restart(&mut self, repo: u64, token: Option<&str>, machine: &str, main: &str) -> bool {
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

    /// A person's Sync now (`sync_now.rs`; `syncNow` in `teamBranchActions.ts`): asks the store for the sync at once (`behind_main`, a
    /// person's ask), in the TS agent's order of refusals. Answers the id of the task the
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
        if self.sync_running() {
            return Err(Why::Refused(format!(
                "A sync of {main} into {base} is already in flight."
            )));
        }
        // The integration-branch pattern catches up by a rebase, which a person's ask runs at once, here.
        if crate::main_rebase::uses_rebase(&self.project) {
            return self.rebase_now(&main).await;
        }
        // The task is made for the computer that asks and comes back through its `health`: one that takes none can't run it.
        if !self.takes_tasks {
            return Err(Why::Refused(
                "This computer takes no tasks from the store, so it can't run the sync.".into(),
            ));
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

impl ProjectLoop {
    /// Sync now on the integration-branch pattern (`rebaseNow` with `force` in the TS agent): rebases the base branch onto main at
    /// once, the person's ask, never the store's. It refuses what a rebase must not do (nothing behind, something still to go into
    /// the branch, the checkout in use) and answers why a conflict or a push that failed stopped it. There is no memory of an
    /// earlier conflict here, so it always tries again.
    async fn rebase_now(&mut self, main: &str) -> Result<String, Why> {
        use crate::main_rebase::{Rebased, WORKTREE, heads, rebase_onto_main};
        let (repo, worktree) = (
            PathBuf::from(&self.project.repo_path),
            Path::new(&self.project.worktrees_dir).join(WORKTREE),
        );
        let (base, main) = (self.project.base_branch.clone(), main.to_string());
        let waiting = self.waiting_on_base();
        let hold = crate::base_hold::hold_base(&self.id, "a rebase")
            .map_err(|_| Why::Refused("The planner's worktree is busy; try again in a moment.".into()))?;
        let (r, b, m) = (repo.clone(), base.clone(), main.clone());
        let done = tokio::task::spawn_blocking(move || -> Result<Result<Rebased, String>> {
            crate::lanes::fetch_latest(&r)?;
            let behind = behind_count(&r, &b, &m)?;
            let (Some(base_sha), Some(main_sha)) = heads(&r, &b, &m) else {
                return Ok(Err(format!("Couldn't read {b} or {m}.")));
            };
            if behind == 0 {
                return Ok(Err(format!("{b} already has all of {m}.")));
            }
            if !waiting.is_empty() {
                return Ok(Err(format!("Waiting for {} to go into {b} first.", waiting.join(", "))));
            }
            rebase_onto_main(&r, &worktree, &b, &m, &base_sha, &main_sha).map(Ok)
        })
        .await;
        drop(hold);
        match done {
            Ok(Ok(Ok(Rebased::Done))) => Ok(String::new()),
            Ok(Ok(Ok(Rebased::Conflict(files)))) => Err(Why::Refused(format!(
                "{base} needs a rebase onto {main}: it conflicts in {}.",
                files.join(", ")
            ))),
            Ok(Ok(Ok(Rebased::Failed(why)))) | Ok(Ok(Err(why))) => Err(Why::Refused(why)),
            Ok(Err(e)) => Err(Why::Failed(format!("Couldn't rebase {base} onto {main}: {e:#}"))),
            Err(e) => Err(Why::Failed(format!("Couldn't rebase {base} onto {main}: {e}"))),
        }
    }
}
