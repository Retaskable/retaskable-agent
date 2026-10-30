//! Picking up a rotation chore (docs/specs/agent/06.7-chores-never-idle.md) once the roadmap claim
//! loop above found nothing more to claim, a lane is free, and a session slot is left: a tidy-up,
//! tests, a bug fix or a spec draft, whichever kind the store's rotation hands out next. Code chores
//! never touch the shared backlog ([`crate::chores`]): only the kind (`chore_next`) and its
//! `chore-<kind>` lock go through the store.
use serde_json::json;

use super::{Links, ProjectLoop, SessionStarter};
use crate::backlog::{Item, ItemStatus};
use crate::chores::{ChoreKind, ChoreRecord};
use crate::config::OnOff;
use crate::item_lifecycle::{ItemRun, ItemState};
use crate::lane_pool::{LanePool, Want};
use crate::orchestrator::decide;
use crate::orchestrator::live::now_iso8601;
use crate::telemetry::{Event, event};

/// The one rotation chore in flight for this project (agent/06.7-chores-never-idle.md: "At most one
/// chore is in flight at a time"): its item id and kind, and the store's `chore-<kind>` lock name,
/// released once it ends. Not carried across a restart yet, unlike a roadmap item's lease: one cut
/// off mid-session leaves its lock held in the store until a person clears it or `chore_next` picks
/// that kind again elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChoreInFlight {
    pub(super) item_id: String,
    pub(super) kind: String,
    pub(super) lock: String,
}

impl ProjectLoop {
    /// Asks the store's rotation for the next chore and starts it, if this project's chores are on, none
    /// is already in flight, its own queue is otherwise empty ("whenever there's nothing else to do", so a
    /// chore never takes a lane a round-robin turn meant for a queued roadmap item), and a lane is free for
    /// one. The chore item lives only here (`chore-<n>`, never sent to the shared backlog); the store only
    /// ever sees the kind and its lock.
    pub(super) async fn claim_chore(
        &mut self,
        pool: &mut LanePool,
        slots: &mut u32,
        links: &Links,
        starter: &dyn SessionStarter,
    ) {
        if self.chore_inflight.is_some() || self.project.chores == OnOff::Off {
            return;
        }
        if self.runs.values().any(|r| r.state == ItemState::Queued) {
            return;
        }
        let want = Want {
            project: self.id.clone(),
            item: String::new(),
            chore: true,
            last_lane: None,
            fix_queued: false,
        };
        if pool.claim_lane(&want).is_none() {
            return;
        }
        if let Some(guard) = self.guard.clone()
            && let Some(why) = guard.refusal().await
        {
            tracing::error!("{}: not starting a chore: {why}", self.id);
            return;
        }
        let Some(repo) = self.repo else { return };
        let token = match links.keychain.get(&links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => {
                self.cant_start(format!("reading the backlog link: {e:#}"));
                return;
            }
        };
        let answer = match self.client.chore_next(repo, Some(&token), &[]).await {
            Err(e) => {
                self.store_error("chore_next", &e);
                return;
            }
            Ok(None) => return,
            Ok(Some(answer)) => answer,
        };
        let Some(kind) = ChoreKind::parse(&answer.kind) else {
            tracing::warn!("{}: chore_next answered the unknown kind {:?}", self.id, answer.kind);
            return;
        };
        let mut roster = self.store.chores();
        let id = roster.take_id();
        self.store.set_chores(&roster);
        if let Err(e) = self.store.save() {
            tracing::warn!("{}: saving the chore roster failed: {e:#}", self.id);
        }
        let item = Item {
            id: id.clone(),
            title: format!("{id}: {}", kind.label()),
            status: ItemStatus::Claimed,
            rank: 0,
            do_next: false,
            depends_on: Vec::new(),
            spec_refs: Vec::new(),
            slice: 1,
            remaining: None,
            pr_url: None,
            branch: None,
            kind: Some(format!("chore:{}", kind.as_str())),
            milestone: Some("Chores".to_string()),
        };
        let mut run = ItemRun::new();
        run.state = ItemState::Queued;
        run.slice = 1;
        run.chore = true;
        self.runs.insert(id.clone(), run);
        self.items.insert(id.clone(), item);
        self.chore_inflight = Some(ChoreInFlight {
            item_id: id.clone(),
            kind: answer.kind.clone(),
            lock: answer.lock.clone(),
        });
        self.events.add_in(
            &self.id,
            Event::Claim(event::Claim {
                answer: Some(json!(id)),
                ..Default::default()
            }),
        );
        let actions = decide(&self.id, false, &self.runs, pool, &self.project);
        *slots = slots.saturating_sub(self.apply_starts(&actions, pool, starter, links).await);
    }

    /// Ends the in-flight chore matching `item_id`: records it in the local roster's history (the last 30
    /// finished chores, success or not) and releases its `chore-<kind>` lock in the store, best-effort (a
    /// failed release is only logged; the store's own lock likely times out on its side). Does nothing for
    /// an item that isn't the one in flight (there is at most one).
    pub(super) async fn finish_chore(&mut self, item_id: &str, title: &str, outcome: &str, links: &Links) {
        if self.chore_inflight.as_ref().is_none_or(|c| c.item_id != item_id) {
            return;
        }
        let inflight = self.chore_inflight.take().unwrap();
        let mut roster = self.store.chores();
        roster.finished(ChoreRecord {
            id: item_id.to_string(),
            kind: inflight.kind.clone(),
            title: title.to_string(),
            outcome: outcome.to_string(),
            at: now_iso8601(),
        });
        self.store.set_chores(&roster);
        if let Err(e) = self.store.save() {
            tracing::warn!("{}: saving the chore roster failed: {e:#}", self.id);
        }
        let Some(repo) = self.repo else { return };
        let Ok(Some(token)) = links.keychain.get(&links.backlog_url) else {
            return;
        };
        if let Err(e) = self.client.lock(repo, Some(&token), &inflight.lock, false).await {
            tracing::warn!("{}: releasing the chore lock {} failed: {e}", self.id, inflight.lock);
        }
    }
}
