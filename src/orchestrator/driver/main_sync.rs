//! The main sync (docs/specs/agent/06.7-chores-never-idle.md, "A main sync is a chore too"): about once a minute
//! a project whose base branch is behind the default branch merges main into a branch off the base, off the loop's
//! thread, in its own sync worktree, and adds a `sync_main` item for it. Plain code. The store's `sync_main-<base>` lock is held from before the merge until the add.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::task::JoinHandle;

use super::project::ProjectLoop;
use crate::backlog::ItemStatus;
use crate::backlog::runs::{LockTake, short_sha};
use crate::config::SyncMain;
use crate::git_bin::command;
use crate::main_sync::{Attempt, attempt, behind_count, item_title, pr_body, pr_title, push, sync_due};
use crate::merge_update::run as git;
use crate::runs::chore::{ChoreKind, chore_lock_name};

/// How often a project looks at whether main moved.
const LOOK_EVERY: Duration = Duration::from_secs(60);

/// The sync worktree's folder under the project's `worktrees_dir`.
const WORKTREE: &str = "sync-main";

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
        let open = items.iter().any(|i| {
            i.kind.as_deref() == Some("sync_main")
                && !matches!(
                    i.status,
                    ItemStatus::OnMain | ItemStatus::InProduction | ItemStatus::Skipped
                )
        });
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
        self.run_loop.runs().locks().add(&lock, &name, now);
        let worktree = Path::new(&self.project.worktrees_dir).join(WORKTREE);
        let gh = self.run_loop.main_sync.gh.clone();
        let task = tokio::task::spawn_blocking(move || sync(&repo_path, &worktree, &gh, &base, &main));
        self.run_loop.main_sync.running = Some(Running { task, lock, sha });
        Ok(())
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
