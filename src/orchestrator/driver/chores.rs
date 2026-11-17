//! Chore tasks as local items (docs/specs/pools.md section 3d; the TypeScript agent's `addTaskChore`, `addFixChore` and
//! `syncTask`): a chore task the store sends (a `run` of `chore`, `fix` or `sync`: `fix_main`, a playtest fix, a smoke fix, `tidy`,
//! `tests`, `bugs`, a main sync) becomes an item with the task's id and no store item behind it. It takes a lane,
//! a session and a change and goes through the item flow like a claimed item; the loop writes nothing to the store for
//! it but the merge turn (a chore entry) and the task's end. A chore that stops is parked and reported (`chore_stop.rs`): the
//! store decides what becomes of it.
use std::path::{Path, PathBuf};

use super::Shared;
use super::main_sync::{Done, sync};
use super::project::ProjectLoop;
use crate::backlog::pool::{TaskEnd, TaskInput};
use crate::chore_items::{ChoreWork, kind_of, session_kind, sync_work};
use crate::item_lifecycle::ItemState;
use crate::main_sync::behind_count;
use crate::merge_update::run as git;
use crate::runs::chore::ChoreKind;

impl ProjectLoop {
    /// Whether the loop holds `id`: a leased item, or a chore item (which has no lease).
    pub(super) fn holds(&self, id: &str) -> bool {
        self.leases.contains_key(id) || self.chores.contains_key(id)
    }

    /// Takes a chore item into `runs` in `state`, kept in the state file.
    pub(super) fn take_chore(&mut self, id: &str, work: ChoreWork, state: ItemState) {
        let item = work.item(id);
        let mut run = crate::item_lifecycle::ItemRun::new();
        run.state = state;
        run.slice = item.slice;
        self.runs.insert(id.to_string(), run);
        self.items.insert(id.to_string(), item);
        self.chores.insert(id.to_string(), work);
        self.save_leases();
    }

    /// A chore task: its item queued for a lane, or the task ended where nothing can be done for it. `true` when an
    /// item was queued (something may start).
    pub(super) async fn chore_task(&mut self, task: &TaskInput, name: &str, shared: &Shared<'_>) -> bool {
        if self.chores.contains_key(&task.id) {
            // Sent again after a restart: its item is already here.
            self.pool.runs.start(&task.id, &task.id, task.tier);
            return false;
        }
        match kind_of(name) {
            Some(ChoreKind::SyncMain) => self.sync_task(task, shared).await,
            Some(_) if let Some(kind) = session_kind(name) => {
                let base = base_head(self.project.repo_path.clone(), self.project.base_branch.clone()).await;
                let work = crate::chore_items::work_of(task, kind, base.as_deref());
                tracing::info!("{}: task {}: {}", self.id, task.id, work.title);
                self.take_chore(&task.id, work, ItemState::Queued);
                self.pool.runs.start(&task.id, &task.id, task.tier);
                true
            }
            _ => {
                let note = format!("No chore called {name} here.");
                tracing::warn!("{}: task {} ({}): {note}", self.id, task.id, task.subject);
                self.end_task(TaskEnd {
                    note: Some(note),
                    ..TaskEnd::done(&task.id, false)
                });
                false
            }
        }
    }

    /// The store's `sync_main` task (`syncTask`): merges main into a branch off the base in the sync worktree, plain code.
    /// Clean, its change is open and the item waits for CI; a conflict queues the item for a session. Nothing to do any
    /// more (main caught up, a sync is open here) ends the task ok. A failure to merge is the task's, not a retry.
    async fn sync_task(&mut self, task: &TaskInput, shared: &Shared<'_>) -> bool {
        let id = task.id.clone();
        let open = |lp: &Self| lp.chores.values().any(|w| w.kind == "sync_main") || lp.sync_running();
        if open(self) {
            self.end_task(TaskEnd::done(&id, true));
            return false;
        }
        // The store names the branches (`sync` on the task); a task from an older store has none, so they are this project's.
        let (base, named_main) = match &task.sync {
            Some(sync) => (sync.branch.clone(), Some(sync.main.clone())),
            None => {
                if self.seen.default_branch.is_none() {
                    self.seen.default_branch =
                        super::report::default_branch(&self.project.github_repo, Path::new(&self.project.repo_path))
                            .await;
                }
                (self.project.base_branch.clone(), self.seen.default_branch.clone())
            }
        };
        let Some(main) = named_main.filter(|m| *m != base) else {
            self.end_task(TaskEnd::done(&id, true));
            return false;
        };
        let (repo_path, b, m) = (PathBuf::from(&self.project.repo_path), base.clone(), main.clone());
        let looked = tokio::task::spawn_blocking(move || -> anyhow::Result<(u32, String)> {
            crate::lanes::fetch_latest(&repo_path)?;
            Ok((
                behind_count(&repo_path, &b, &m)?,
                git(
                    &crate::git_bin::command,
                    &repo_path,
                    &["rev-parse", &format!("origin/{m}")],
                )?,
            ))
        })
        .await;
        let (behind, sha) = match looked {
            Ok(Ok(found)) => found,
            Ok(Err(e)) => return self.sync_failed(&id, format!("{e:#}")),
            Err(e) => return self.sync_failed(&id, e.to_string()),
        };
        // The store decided it was due; main may have been caught up since.
        if behind == 0 {
            self.end_task(TaskEnd::done(&id, true));
            return false;
        }
        let worktree = Path::new(&self.project.worktrees_dir).join(super::main_sync::WORKTREE);
        let gh = shared
            .links
            .env
            .get("RETASKABLE_GH")
            .map_or_else(|| PathBuf::from("gh"), PathBuf::from);
        let (repo_path, b, m) = (PathBuf::from(&self.project.repo_path), base.clone(), main.clone());
        let merged = tokio::task::spawn_blocking(move || sync(&repo_path, &worktree, &gh, &b, &m)).await;
        let done = match merged {
            Ok(Ok(done)) => done,
            Ok(Err(e)) => return self.sync_failed(&id, format!("{e:#}")),
            Err(e) => return self.sync_failed(&id, e.to_string()),
        };
        self.pool.runs.start(&id, &id, task.tier);
        match done {
            Done::Clean { url, .. } => {
                let work = sync_work(task, &sha, &main, &base, &[]);
                self.take_chore(&id, work, ItemState::AwaitingCi);
                self.refresh_record_now(&id);
                if let Some(record) = self.records.get_mut(&id) {
                    record.flow.pr_number = crate::drafts::land::pr_number(&url);
                    record.flow.pr_url = Some(url);
                    record.flow.ci_wait_since = Some(crate::telemetry::now_ms());
                }
                self.save_leases();
                false
            }
            Done::Conflict { files } => {
                let work = sync_work(task, &sha, &main, &base, &files);
                self.take_chore(&id, work, ItemState::Queued);
                true
            }
        }
    }

    fn sync_failed(&mut self, id: &str, why: String) -> bool {
        tracing::warn!("{}: the main sync for task {id} didn't run: {why}", self.id);
        self.live
            .set_sync_main(&self.id, Some(format!("Syncing main didn't run: {why}")));
        self.end_task(TaskEnd {
            note: Some(format!("Syncing main didn't run: {why}")),
            ..TaskEnd::done(id, false)
        });
        false
    }

    /// Saves the record of an item just taken in a state that holds no lane.
    fn refresh_record_now(&mut self, id: &str) {
        let tokens = self.records.get(id).and_then(|r| r.impl_context_tokens);
        self.remember(id, None, None, tokens);
        self.save_leases();
    }

    /// Lets go of a chore item: its run, record and saved work.
    pub(super) fn forget_chore(&mut self, id: &str) {
        self.runs.remove(id);
        self.items.remove(id);
        self.chores.remove(id);
        self.save_leases();
    }

    /// Chores that merged, once their tasks have ended (`end_finished_tasks` read them as done first).
    pub(super) fn forget_finished_chores(&mut self) {
        let done: Vec<String> = (self.runs.iter())
            .filter(|(id, run)| {
                self.chores.contains_key(*id)
                    && matches!(run.state, ItemState::Merged | ItemState::Done)
                    && !self.pool.runs.running().values().any(|t| &t.item == *id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in done {
            self.forget_chore(&id);
        }
    }
}

/// The base branch's commit as of the last fetch (a fix main's `redSha`).
async fn base_head(repo_path: String, base: String) -> Option<String> {
    let at = format!("origin/{base}");
    super::report::git(
        Path::new(&repo_path),
        &["rev-parse", "--verify", "--quiet", &format!("{at}^{{commit}}")],
    )
    .await
}
