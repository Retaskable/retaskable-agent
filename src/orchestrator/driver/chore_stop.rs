//! A chore that stopped (pools-on-server.md section 14.5; the TypeScript agent's `stopped` and `drop` in `poolTasks.ts`). The
//! agent decides nothing: a chore item that needs a person or clarification is **parked**, and `health` reports its task
//! as `running: [{ id, state: "needs_human", why }]`. The store decides what becomes of it (a playtest fix and a main sync
//! become an item a person continues, every other chore is dropped) and either way answers `stop: [id]`, which is the cue to
//! close the chore's change, free its lane and end the task not ok. A fix main that stopped at a known commit then tells the
//! store (`chore_nofix`), as it always did.
use super::Shared;
use super::project::{Abandon, ProjectLoop, SessionStarter};
use crate::backlog::pool::TaskEnd;
use crate::item_lifecycle::ItemState;
use crate::lane_pool::LanePool;

impl ProjectLoop {
    /// Why the chore item `id` is parked, or `None` when it is no chore or has not stopped: it needs a person or clarification.
    pub(super) fn parked_chore(&self, id: &str) -> Option<String> {
        let run = self.runs.get(id).filter(|_| self.chores.contains_key(id))?;
        if !matches!(run.state, ItemState::NeedsHuman | ItemState::NeedsClarification) {
            return None;
        }
        Some(
            (self.records.get(id))
                .and_then(|r| r.needs_human_reason.clone())
                .unwrap_or_else(|| "it stopped".into()),
        )
    }

    /// The store answered `stop` for these task ids. Each that is a parked chore is closed, its lane freed and its task
    /// ended not ok. Any other id is only logged: its session is the store's to cut through the lease, as before.
    pub(super) async fn stop_parked_chores(
        &mut self,
        ids: &[String],
        pool: &mut LanePool,
        starter: &dyn SessionStarter,
        shared: &Shared<'_>,
    ) {
        for id in ids {
            let Some(task) = self.pool.runs.running().get(id).cloned() else {
                continue;
            };
            let Some(why) = self.parked_chore(&task.item) else {
                tracing::warn!("{}: the store let go of task {id}, which still runs here", self.id);
                continue;
            };
            self.drop_chore(&task.item, &why, pool, starter, shared).await;
            self.pool.runs.stopped(id);
            self.end_task(TaskEnd::done(id, false));
        }
        self.save_pool();
    }

    /// Closes the parked chore item `id`'s change, tells the store a fix main found nothing to fix, frees its lane and lets
    /// the item go.
    async fn drop_chore(
        &mut self,
        id: &str,
        why: &str,
        pool: &mut LanePool,
        starter: &dyn SessionStarter,
        shared: &Shared<'_>,
    ) {
        let record = self.records.get(id).cloned();
        let pr_url = record
            .as_ref()
            .and_then(|r| r.flow.pr_url.clone())
            .or_else(|| self.items.get(id).and_then(|i| i.pr_url.clone()));
        let branch = record.as_ref().and_then(|r| r.branch.clone());
        tracing::info!("{}: chore {id} dropped, the store said stop: {why}", self.id);
        let old = Abandon {
            project: self.id.clone(),
            branch: branch.filter(|_| pr_url.is_none()),
            pr_url,
        };
        if let Err(e) = starter.abandon(old).await {
            tracing::warn!("{}: closing chore {id}'s change: {e}", self.id);
        }
        if let Some(work) = self.chores.get(id).cloned() {
            self.tell_no_fix(&work, shared).await;
        }
        self.free_lane(pool, id, "dropped", why);
        self.forget_chore(id);
    }

    /// A fix main that ended without a merge: the store keeps the commit so no computer queues another for it
    /// (`choreEnded`, `chore_nofix`).
    async fn tell_no_fix(&mut self, work: &crate::chore_items::ChoreWork, shared: &Shared<'_>) {
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
}
