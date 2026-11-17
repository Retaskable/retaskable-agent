//! Taking back what a restart left: the leased items and, beside them, the chore items (`chore_items`), each in the
//! state its record says (the TS agent's `recover`).
use super::project::ProjectLoop;
use crate::backlog::{Item, ItemStatus};
use crate::item_lifecycle::ItemState;
use crate::lane_pool::{Holder, LanePool};
use crate::orchestrator::steps::state_name;
use crate::telemetry::{Event, event};

impl ProjectLoop {
    /// On the first pass, takes back the items this computer still holds a lease on, each as its saved run
    /// says (the TS agent's `recover`: items keep their state and lane across a restart). An implementing item
    /// takes its lane again and has its session resumed (on the next schedule, as its running step: the
    /// starter counts it, and the loop never starts a second); one in checking takes its lane and the loop
    /// steps it as usual; one that needs a human has no lane and keeps its lease. A lease with no saved run
    /// (an older state file) starts again as queued. An item with a run the store no longer shows as handed
    /// out (taken by another agent, finished, gone) is dropped. A needs_clarification item was parked, so it
    /// holds no lease and isn't here: the store hands it back when it is answered. With this computer's name (`machine`), a needs_human item held only for Cursor's limit with
    /// no pull request isn't taken back: its record is dropped and its lease moved to `unreleased` with the
    /// give-back note, for `flush_owed` to send.
    pub(super) fn readopt(&mut self, items: &[Item], pool: &mut LanePool, machine: &str) {
        if std::mem::replace(&mut self.adopted, true)
            || (self.leases.is_empty() && self.parked.is_empty() && self.chores.is_empty())
        {
            return;
        }
        for (id, token) in std::mem::take(&mut self.leases) {
            let record = self.records.remove(&id);
            let Some(item) = items.iter().find(|i| i.id == id) else {
                tracing::info!("{}: no longer holds {id}, dropping its lease", self.id);
                continue;
            };
            if item.pr_url.is_none()
                && record.as_ref().is_some_and(|r| {
                    r.state == "needs_human"
                        && r.needs_human_reason.as_deref() == Some(super::settle::CURSOR_LIMIT_STOP)
                })
                && matches!(
                    item.status,
                    ItemStatus::Claimed | ItemStatus::Building | ItemStatus::NeedsPerson
                )
            {
                tracing::info!("{}: giving back {id}, held only for Cursor's limit", self.id);
                let why = format!("Given back: Cursor's usage limit was hit on {machine} before anything was pushed");
                self.list_unreleased(id, token, why);
                continue;
            }
            let handed = matches!(item.status, ItemStatus::Claimed | ItemStatus::Building);
            let (state, ok) = match record.as_ref().map(|r| r.state.as_str()) {
                Some("needs_human") => (ItemState::NeedsHuman, handed || item.status == ItemStatus::NeedsPerson),
                Some("checking") => (ItemState::Checking, handed || item.status == ItemStatus::InReview),
                Some("reviewing") => (ItemState::Reviewing, handed || item.status == ItemStatus::InReview),
                Some("implementing") => (ItemState::Implementing, handed),
                _ => (ItemState::Queued, handed),
            };
            if !ok {
                tracing::info!("{}: no longer holds {id}, dropping its lease", self.id);
                continue;
            }
            tracing::info!("{}: still holds {id} ({}), carrying on", self.id, state_name(state));
            self.take_lease(item, token, state);
            let Some(record) = record else { continue };
            if let Some(run) = self.runs.get_mut(&id) {
                record.flow.restore_counters(run);
            }
            let session = record.impl_session_id.clone();
            let mut lane = None;
            if matches!(
                state,
                ItemState::Checking | ItemState::Reviewing | ItemState::Implementing
            ) {
                lane = record.lane.filter(|&k| self.retake_lane(pool, &id, k));
                // Its lane is gone (fewer lanes now, or taken), or it was cut off before a session id was saved:
                // it starts again as queued. TS can't get here, since a lane belongs to its item until freed.
                if lane.is_none() || (state == ItemState::Implementing && session.is_none()) {
                    self.free_lane(pool, &id, "queued", "nothing to resume");
                    self.runs.get_mut(&id).expect("just taken").state = ItemState::Queued;
                    continue;
                }
            }
            if state == ItemState::Implementing {
                self.to_resume.push(id.clone());
            }
            let session = session.map(|s| (s, record.impl_session_engine));
            self.remember(&id, lane, session, record.impl_context_tokens);
            if let Some(saved) = self.records.get_mut(&id) {
                saved.silence_kills = record.silence_kills;
                saved.cursor_limit_hits = record.cursor_limit_hits;
                saved.not_started = record.not_started;
                saved.flow = record.flow.clone();
                if record.not_started {
                    self.fresh_launched.insert(id.clone());
                }
            }
        }
        self.readopt_chores(pool);
        self.restore_parked(items);
        self.restore_task_tiers();
        self.save_leases();
    }

    /// Takes `lane` back for an item that held it before the restart, if it is still free.
    pub(super) fn retake_lane(&mut self, pool: &mut LanePool, item_id: &str, lane: u32) -> bool {
        let holder = Holder::Item {
            project: self.id.clone(),
            item: item_id.to_string(),
        };
        let taken = pool.take(lane, holder);
        if taken {
            self.events.add_in(
                &self.id,
                Event::LaneTaken(event::LaneTaken {
                    item: Some(item_id.to_string()),
                    lane: Some(lane),
                    ..Default::default()
                }),
            );
        }
        taken
    }

    /// Takes back the chore items a restart left (they hold no lease, so `chores` is what remembers them): each in the
    /// state its record says, with the lane it held when it works in one. One that had stopped for a person is dropped,
    /// as a chore never waits for one.
    fn readopt_chores(&mut self, pool: &mut LanePool) {
        for (id, work) in std::mem::take(&mut self.chores) {
            let record = self.records.remove(&id).unwrap_or_default();
            let state = match record.state.as_str() {
                "implementing" => ItemState::Implementing,
                "checking" => ItemState::Checking,
                "reviewing" => ItemState::Reviewing,
                "awaiting_ci" => ItemState::AwaitingCi,
                "merging" => ItemState::Merging,
                "awaiting_merge" => ItemState::AwaitingMerge,
                "needs_human" | "needs_clarification" | "merged" | "done" => {
                    tracing::info!("{}: chore {id} was over before the restart, dropping it", self.id);
                    continue;
                }
                _ => ItemState::Queued,
            };
            tracing::info!(
                "{}: still runs chore {id} ({}), carrying on",
                self.id,
                state_name(state)
            );
            self.take_chore(&id, work, state);
            if let Some(run) = self.runs.get_mut(&id) {
                record.flow.restore_counters(run);
            }
            let session = record.impl_session_id.clone();
            let mut lane = None;
            if matches!(
                state,
                ItemState::Checking | ItemState::Reviewing | ItemState::Implementing
            ) {
                lane = record.lane.filter(|&k| self.retake_lane(pool, &id, k));
                if lane.is_none() || (state == ItemState::Implementing && session.is_none()) {
                    self.free_lane(pool, &id, "queued", "nothing to resume");
                    self.runs.get_mut(&id).expect("just taken").state = ItemState::Queued;
                    continue;
                }
            }
            if state == ItemState::Implementing {
                self.to_resume.push(id.clone());
            }
            let session = session.map(|s| (s, record.impl_session_engine));
            self.remember(&id, lane, session, record.impl_context_tokens);
            if let Some(saved) = self.records.get_mut(&id) {
                saved.silence_kills = record.silence_kills;
                saved.cursor_limit_hits = record.cursor_limit_hits;
                saved.not_started = record.not_started;
                saved.flow = record.flow.clone();
                if record.not_started {
                    self.fresh_launched.insert(id.clone());
                }
            }
        }
        self.save_leases();
    }
}
