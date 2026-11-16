//! The main sync (docs/specs/agent/06.7-chores-never-idle.md, "A main sync is a chore too"): about once a minute
//! a project whose base branch is behind the default branch merges main into a branch off the base, off the loop's
//! thread, in its own sync worktree, and adds a `sync_main` item for it. Plain code. The store's `sync_main-<base>` lock is held from before the merge until the add.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::task::JoinHandle;

use super::project::ProjectLoop;
use crate::backlog::runs::{LockTake, short_sha};
use crate::config::SyncMain;
use crate::git_bin::command;
use crate::intents::main_pr::Why;
use crate::main_sync::{Attempt, attempt, behind_count, item_title, pr_body, pr_title, push, sync_due};
use crate::merge_update::run as git;
use crate::runs::chore::{ChoreKind, chore_lock_name};
use crate::team_branch::need_team_branch;

/// How often a project looks at whether main moved.
const LOOK_EVERY: Duration = Duration::from_secs(60);

/// The sync worktree's folder under the project's `worktrees_dir`.
const WORKTREE: &str = "sync-main";

/// The store's `sync_main-<base>` lock as a team-branch request holds it (`hold_sync_lock`).
pub(super) enum SyncLock {
    /// Taken here: the token to give back (`release_sync_lock`).
    Mine(String),
    /// A main sync is running here, or another computer holds the lock.
    Busy,
}

/// What the merge attempt came to.
enum Done {
    Clean { branch: String, url: String },
    Conflict { files: Vec<String> },
}

struct Running {
    task: JoinHandle<Result<Done>>,
    /// The store's lock, held until the add is done.
    lock: String,
    sha: String,
}

pub(super) struct State {
    running: Option<Running>,
    looked: Option<Instant>,
    /// The main commit a sync was last made (or found) for: nothing starts again until main moves.
    handled: Option<String>,
    gh: PathBuf,
}

impl Default for State {
    fn default() -> Self {
        Self {
            running: None,
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
    /// One pass: finishes a sync that came back, else (not `stopping`, at most once a minute) starts one if main
    /// moved. A failure to start is logged and kept as the project's sync note.
    pub(super) async fn main_sync(
        &mut self,
        repo: u64,
        token: Option<&str>,
        machine: &str,
        stopping: bool,
        now: Instant,
    ) {
        if let Some(r) = self.run_loop.main_sync.running.take_if(|r| r.task.is_finished()) {
            let done = r.task.await.map_err(anyhow::Error::from).and_then(|d| d);
            self.finish_sync(repo, token, &r.lock, &r.sha, done).await;
            return;
        }
        let state = &mut self.run_loop.main_sync;
        if stopping
            || state.running.is_some()
            || state.looked.is_some_and(|t| now.duration_since(t) < LOOK_EVERY)
            || self.project.sync_main == SyncMain::Off
        {
            return;
        }
        self.run_loop.main_sync.looked = Some(now);
        if let Err(e) = self.start_sync(repo, token, machine, now).await {
            tracing::warn!("{}: main sync didn't start: {e:#}", self.id);
            self.live
                .set_sync_main(&self.id, Some(format!("Syncing main didn't start: {e:#}")));
        }
    }

    /// Whether this loop is running a main sync (merging main into a branch off the base).
    pub(super) fn sync_running(&self) -> bool {
        self.run_loop.main_sync.running.is_some()
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

    async fn start_sync(&mut self, repo: u64, token: Option<&str>, machine: &str, now: Instant) -> Result<()> {
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
        let items = match self.client.list(repo, token).await {
            Ok(items) => items,
            Err(e) => {
                self.store_error("list", &e);
                return Ok(());
            }
        };
        let id = format!("chore-sync-main-{}", short_sha(&sha));
        if items
            .iter()
            .any(|i| i.kind.as_deref() == Some("sync_main") && i.id == id)
        {
            // Made for this commit already, whatever became of it (even skipped): wait for main to move.
            self.run_loop.main_sync.handled = Some(sha);
            return Ok(());
        }
        let open = crate::team_branch::held_in(&items).0;
        if !sync_due(&base, &main, behind, self.project.sync_main, open) {
            return Ok(());
        }
        let name = chore_lock_name(ChoreKind::SyncMain, &base).unwrap_or_default();
        let note = format!("main sync on {machine}");
        let lock = match self.client.lock_take(repo, token, &name, Some(&note)).await {
            Ok(LockTake::Held(lock)) => lock,
            Ok(LockTake::HeldBy { by, .. }) => {
                tracing::info!("{}: {by} holds {name}; leaving the main sync to them", self.id);
                return Ok(());
            }
            Err(e) => {
                self.store_error("lock_take", &e);
                return Ok(());
            }
        };
        self.run_sync(lock, &name, sha, main, now);
        Ok(())
    }

    /// Starts the merge in the sync worktree, off the loop's thread, holding the store's `lock` until the add is done.
    fn run_sync(&mut self, lock: String, name: &str, sha: String, main: String, now: Instant) {
        self.run_loop.runs().locks().add(&lock, name, now);
        let (repo_path, base) = (PathBuf::from(&self.project.repo_path), self.project.base_branch.clone());
        let worktree = Path::new(&self.project.worktrees_dir).join(WORKTREE);
        let gh = self.run_loop.main_sync.gh.clone();
        let task = tokio::task::spawn_blocking(move || sync(&repo_path, &worktree, &gh, &base, &main));
        self.run_loop.main_sync.running = Some(Running { task, lock, sha });
    }

    /// A person's Sync now (`sync_now.rs`): starts the sync at once, past the minute's gate, `handled` and `syncMain`
    /// being off, in the TS agent's order of refusals. Answers the id of the chore item the sync will add.
    pub(super) async fn start_sync_now(
        &mut self,
        repo: u64,
        token: Option<&str>,
        machine: &str,
    ) -> Result<String, Why> {
        if self.seen.default_branch.is_none() {
            self.seen.default_branch =
                super::report::default_branch(&self.project.github_repo, Path::new(&self.project.repo_path)).await;
        }
        let main = need_team_branch(&self.project.base_branch, self.seen.default_branch.as_deref())
            .map_err(|why| Why::Refused(why.into()))?
            .to_string();
        let base = self.project.base_branch.clone();
        let in_flight = || Why::Refused(format!("A sync of {main} into {base} is already in flight."));
        let items = match self.client.list(repo, token).await {
            Ok(items) => items,
            Err(e) => {
                self.store_error("list", &e);
                return Err(Why::Failed(format!(
                    "Couldn't ask the store to sync {main} into {base}: {e}"
                )));
            }
        };
        // A sync this loop is running, or one open in the store (another computer's, or one waiting to be merged).
        if self.run_loop.main_sync.running.is_some() || crate::team_branch::held_in(&items).0 {
            return Err(in_flight());
        }
        let (repo_path, b, m) = (PathBuf::from(&self.project.repo_path), base.clone(), main.clone());
        let (behind, sha) = tokio::task::spawn_blocking(move || -> Result<(u32, String)> {
            crate::lanes::fetch_latest(&repo_path)?;
            Ok((
                behind_count(&repo_path, &b, &m)?,
                git(&command, &repo_path, &["rev-parse", &format!("origin/{m}")])?,
            ))
        })
        .await
        .map_err(|e| Why::Failed(e.to_string()))?
        .map_err(|e| Why::Failed(format!("{e:#}")))?;
        if behind == 0 {
            return Err(Why::Refused(format!("{base} already has all of {main}.")));
        }
        let id = format!("chore-sync-main-{}", short_sha(&sha));
        if items.iter().any(|i| i.id == id) {
            // The store keeps one sync item per main commit, whatever became of it (even skipped).
            return Err(Why::Refused(format!(
                "A sync of {main} into {base} was already made for {main}'s latest commit."
            )));
        }
        let name = chore_lock_name(ChoreKind::SyncMain, &base).unwrap_or_default();
        let note = format!("main sync on {machine}");
        let lock = match self.client.lock_take(repo, token, &name, Some(&note)).await {
            Ok(LockTake::Held(lock)) => lock,
            Ok(LockTake::HeldBy { .. }) => return Err(in_flight()),
            Err(e) => {
                self.store_error("lock_take", &e);
                return Err(Why::Failed(format!(
                    "Couldn't ask the store to sync {main} into {base}: {e}"
                )));
            }
        };
        self.run_sync(lock, &name, sha, main, Instant::now());
        Ok(id)
    }

    /// The merge attempt is in: add the item, then give the lock back whatever happened.
    async fn finish_sync(&mut self, repo: u64, token: Option<&str>, lock: &str, sha: &str, done: Result<Done>) {
        let base = self.project.base_branch.clone();
        let id = format!("chore-sync-main-{}", short_sha(sha));
        let added = match &done {
            Ok(Done::Clean { branch, url }) => {
                self.client
                    .add_sync_main(repo, token, &id, &item_title(&base), sha, Some(branch), Some(url), &[])
                    .await
            }
            Ok(Done::Conflict { files }) => {
                self.client
                    .add_sync_main(repo, token, &id, &item_title(&base), sha, None, None, files)
                    .await
            }
            Err(e) => {
                tracing::warn!("{}: main sync didn't run: {e:#}", self.id);
                self.live
                    .set_sync_main(&self.id, Some(format!("Syncing main didn't run: {e:#}")));
                Ok(())
            }
        };
        if let Err(e) = self.client.lock_drop(repo, token, lock).await {
            self.store_error("lock_drop", &e);
        }
        self.run_loop.runs().locks().remove(lock);
        match added {
            Err(e) if e.code() == Some("conflict") => self.run_loop.main_sync.handled = Some(sha.into()),
            Ok(()) if done.is_ok() => {
                self.run_loop.main_sync.handled = Some(sha.into());
                self.live.set_sync_main(&self.id, None)
            }
            Ok(()) => {}
            Err(e) => {
                self.store_error("add", &e);
                self.live
                    .set_sync_main(&self.id, Some(format!("Syncing main couldn't be added: {e}")));
            }
        }
    }
}

/// Merges main into a branch off the base in the sync worktree; when clean, pushes it and opens (or finds) its PR.
fn sync(repo: &Path, worktree: &Path, gh: &Path, base: &str, main: &str) -> Result<Done> {
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
            Ok(Done::Clean {
                branch,
                url: change.url,
            })
        }
    }
}
