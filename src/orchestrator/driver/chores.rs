//! Chore tasks as local items (docs/specs/pools.md section 3d; the TypeScript agent's `addTaskChore`, `addFixChore` and
//! `syncTask`): a chore task the store sends (`fix_main`, `fix_playtest:<key>`, `fix_smoke:<sha>`, `chore:tidy`, `tests`,
//! `bugs`, `chore:sync_main:<branch>`) becomes an item with the task's id and no store item behind it. It takes a lane,
//! a session and a change and goes through the item flow like a claimed item; the loop writes nothing to the store for
//! it but the merge turn (a chore entry) and the task's end. A chore never waits on a person: one that stops is dropped,
//! its change closed, and a fix main that found nothing to fix tells the store so (`chore_nofix`).
use std::path::{Path, PathBuf};

use super::Shared;
use super::main_sync::{Done, sync};
use super::project::{Abandon, ProjectLoop, SessionStarter};
use crate::backlog::pool::{TaskEnd, TaskInput};
use crate::chore_items::{ChoreWork, kind_of, session_kind, sync_work};
use crate::item_lifecycle::ItemState;
use crate::lane_pool::LanePool;
use crate::main_sync::{behind_count, sync_due};
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
        if self.seen.default_branch.is_none() {
            self.seen.default_branch =
                super::report::default_branch(&self.project.github_repo, Path::new(&self.project.repo_path)).await;
        }
        let base = self.project.base_branch.clone();
        let Some(main) = self.seen.default_branch.clone().filter(|m| *m != base) else {
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
        if !sync_due(&base, &main, behind, self.project.sync_main, false) {
            self.end_task(TaskEnd::done(&id, true));
            return false;
        }
        let worktree = Path::new(&self.project.worktrees_dir).join("sync-main");
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

    /// A chore never waits on a person (`needsHuman` -> `dropChore`): a chore item that stopped for one is dropped, its
    /// change closed, its lane freed and, for a fix main that found nothing to fix, the store told (`chore_nofix`). The
    /// TypeScript agent keeps a playtest fix and a main sync for a person to continue; here no page continues an item
    /// that has no store item, so those are dropped too and the task ends not ok (the store may send another).
    pub(super) async fn drop_stopped_chores(
        &mut self,
        pool: &mut LanePool,
        starter: &dyn SessionStarter,
        shared: &Shared<'_>,
    ) {
        let stopped: Vec<String> = (self.runs.iter())
            .filter(|(id, run)| {
                self.chores.contains_key(*id)
                    && matches!(run.state, ItemState::NeedsHuman | ItemState::NeedsClarification)
                    && !self.running.busy(id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in stopped {
            let why = (self.records.get(&id))
                .and_then(|r| r.needs_human_reason.clone())
                .unwrap_or_else(|| "it stopped".into());
            let record = self.records.get(&id).cloned();
            let pr_url = record
                .as_ref()
                .and_then(|r| r.flow.pr_url.clone())
                .or_else(|| self.items.get(&id).and_then(|i| i.pr_url.clone()));
            let branch = record.as_ref().and_then(|r| r.branch.clone());
            tracing::info!("{}: chore {id} dropped: {why}", self.id);
            let old = Abandon {
                project: self.id.clone(),
                branch: branch.filter(|_| pr_url.is_none()),
                pr_url,
            };
            if let Err(e) = starter.abandon(old).await {
                tracing::warn!("{}: closing chore {id}'s change: {e}", self.id);
            }
            if let Some(work) = self.chores.get(&id).cloned() {
                self.tell_no_fix(&work, shared).await;
            }
            self.free_lane(pool, &id, "dropped", &why);
            self.forget_chore(&id);
        }
    }

    /// A fix main that ended without a merge: the store keeps the commit so no computer queues another for it
    /// (`choreEnded`, `chore_nofix`).
    async fn tell_no_fix(&mut self, work: &ChoreWork, shared: &Shared<'_>) {
        let (Some(sha), Some(repo)) = (work.red_sha.as_deref(), self.repo) else {
            return;
        };
        let Ok(Some(token)) = shared.links.store_token().await else {
            return;
        };
        if let Err(e) = self
            .client
            .chore_nofix(repo, Some(&token), sha, &self.project.base_branch)
            .await
        {
            self.store_error("chore_nofix", &e);
        }
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
