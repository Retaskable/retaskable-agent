//! How work the loop started comes back to it: a session's end over the outcome channel ([`SessionOutcome`],
//! settled on the next wake), a step's end through the project's `Running` set, and what either means for the
//! item's lane and lease (freed, parked, or given back to the store, with the releases and parks the store
//! hasn't taken yet sent again later: the TS agent's `unreleased`).
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::{Links, Machine, ProjectLoop, SessionStarter, Shared};
use crate::item_lifecycle::{ImplementerOutcome, ItemRun, ItemState};
use crate::lane_pool::{Holder, LanePool};
use crate::orchestrator::steps::{StepRequest, has_step, needs_lane, state_name};
use crate::telemetry::{Event, event};

/// How long an own failure before any session ran (the TS agent's `ownFailure`) pauses claiming of new
/// work for every project on this computer (section 7.3): a problem with this computer, not the item, so
/// hammering the store for more work would likely just fail the same way.
pub(super) const CLAIM_PAUSE: Duration = Duration::from_secs(30 * 60);

/// How a started session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionResult {
    /// The implementer exited normally (it finished, synced main, or asked questions).
    Implementer(ImplementerOutcome),
    /// A failure before any session ran (lane prep, or the session wouldn't even start): this computer's own
    /// problem, not the item's (the TS `ownFailure`), so the item is given back to the store's queue and
    /// this project's claiming pauses ([`hand_back`](ProjectLoop::hand_back)). The text says why.
    Failed(String),
    /// A session ran but ended in a way that needs a person: it crashed, exited without an answer, gave up,
    /// found nothing left to build, left steps only a person can do, or answered with a status this agent
    /// doesn't know — any of these may have left real work behind, so the run goes to `needs_human` instead
    /// of back to the queue, freeing its lane and keeping its lease. The text says why.
    NeedsHuman(String),
}

/// A session's end, sent back to the loop that started it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOutcome {
    pub project: String,
    pub item_id: String,
    pub lane: u32,
    pub result: SessionResult,
}

/// The sending end of the outcome channel: cloned into every [`StartRequest`](super::StartRequest).
#[derive(Debug, Clone)]
pub struct OutcomeSender(pub(super) mpsc::UnboundedSender<SessionOutcome>);

impl OutcomeSender {
    /// A sender and the receiving end of its channel.
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<SessionOutcome>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self(sender), receiver)
    }

    /// Reports a session's end. A loop that has stopped hears nothing, which is fine: nothing is left to settle.
    pub fn send(&self, outcome: SessionOutcome) {
        let _ = self.0.send(outcome);
    }
}

impl PartialEq for OutcomeSender {
    fn eq(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }
}

impl Eq for OutcomeSender {}

impl ProjectLoop {
    /// Gives an item's lane and lease back: its run is dropped, the lane freed (`lane_freed`), and the lease
    /// moved to `unreleased` (saved before anything is sent, so a restart never strands the claim) and
    /// released at once. `state` is the item's state for the `lease` event and `why` the reason, as in the
    /// TS agent's `lane_freed` (`<state>: <note>`). An own failure before any session ran, so the error goes
    /// in the local event log (not the item's: it isn't the item's fault) and claiming pauses, for every
    /// project on this computer, for [`CLAIM_PAUSE`] (section 7.3, [`Machine::claim_paused_until`]), restarting
    /// cleared.
    pub(super) async fn hand_back(
        &mut self,
        pool: &mut LanePool,
        item_id: &str,
        state: &str,
        why: &str,
        shared: &mut Shared<'_>,
    ) {
        self.runs.remove(item_id);
        self.items.remove(item_id);
        self.free_lane(pool, item_id, state, why);
        *shared.claim_paused_until = Some(Instant::now() + CLAIM_PAUSE);
        self.events.add_in(
            &self.id,
            Event::Error(event::Error {
                item: Some(item_id.to_string()),
                r#where: Some("start".into()),
                error: Some(why.to_string()),
                ..Default::default()
            }),
        );
        if let Some(token) = self.leases.remove(item_id) {
            self.unreleased.insert(item_id.to_string(), token);
            self.save_leases();
            self.events.add_in(
                &self.id,
                Event::Lease(event::Lease {
                    item: Some(item_id.to_string()),
                    change: Some("released".into()),
                    state: Some(state.into()),
                    ..Default::default()
                }),
            );
            self.flush_owed(shared.links).await;
        }
    }

    /// Lets go of the lease while the item waits for `waits_for` (the TS `park`): the lane is freed, the run
    /// kept, and the lease moved to `unparked` (saved first) and parked at once. The store keeps the item as
    /// this computer's, so no other computer is handed it.
    async fn park(&mut self, pool: &mut LanePool, item_id: &str, state: &str, waits_for: &str, links: &Links) {
        self.free_lane(pool, item_id, state, "waiting");
        if let Some(token) = self.leases.remove(item_id) {
            self.unparked
                .insert(item_id.to_string(), (token, waits_for.into(), state.into()));
            self.save_leases();
            self.flush_owed(links).await;
        }
    }

    /// Frees the item's lane, if it holds one, and writes `lane_freed` (`<state>: <note>`).
    pub(super) fn free_lane(&mut self, pool: &mut LanePool, item_id: &str, state: &str, why: &str) {
        let holder = Holder::Item {
            project: self.id.clone(),
            item: item_id.to_string(),
        };
        if let Some(lane) = pool.lane_of(&self.id, item_id)
            && pool.release(lane, &holder)
        {
            self.events.add_in(
                &self.id,
                Event::LaneFreed(event::LaneFreed {
                    item: Some(item_id.to_string()),
                    lane: Some(lane),
                    why: Some(format!("{state}: {why}")),
                    ..Default::default()
                }),
            );
        }
    }

    /// Sends the releases and parks still owed. One the store refuses (or can't reach) stays listed for
    /// the next pass; `stale_lease` means the lease was gone already, so it counts as done.
    pub(super) async fn flush_owed(&mut self, links: &Links) {
        let Some(repo) = self
            .repo
            .filter(|_| !self.unreleased.is_empty() || !self.unparked.is_empty())
        else {
            return;
        };
        let Ok(Some(token)) = links.keychain.get(&links.backlog_url) else {
            return;
        };
        for (id, lease) in self.unreleased.clone() {
            match self.client.release(repo, Some(&token), &id, &lease).await {
                Ok(()) => {}
                Err(e) if e.code() == Some("stale_lease") => {}
                Err(e) => {
                    self.store_error("release", &e);
                    continue;
                }
            }
            self.unreleased.remove(&id);
        }
        for (id, (lease, waits_for, state)) in self.unparked.clone() {
            match self.client.park(repo, Some(&token), &id, &lease, &waits_for).await {
                Ok(()) => {}
                Err(e) if e.code() == Some("stale_lease") => {}
                Err(e) => {
                    self.store_error("park", &e);
                    continue;
                }
            }
            self.unparked.remove(&id);
            self.events.add_in(
                &self.id,
                Event::Lease(event::Lease {
                    item: Some(id),
                    change: Some("parked".into()),
                    state: Some(state),
                    ..Default::default()
                }),
            );
        }
        self.save_leases();
    }

    /// What an item's run moving from `from` to its state now means, after a session or a step ended
    /// (`note` says which): an `item_state` event when it moved; questions for a person free the lane and
    /// park; any other state that runs nothing in a lane (`needs_lane`) frees it and keeps the lease (as the
    /// TS agent does for `needs_human` with no PR, and until a merge lands for the rest).
    async fn moved(&mut self, pool: &mut LanePool, item: &str, from: ItemState, note: &str, links: &Links) {
        let Some(to) = self.runs.get(item).map(|r| r.state) else {
            return;
        };
        if from != to {
            self.events.add_in(
                &self.id,
                Event::ItemState(event::ItemState {
                    item: Some(item.to_string()),
                    from: Some(state_name(from).into()),
                    to: Some(state_name(to).into()),
                    ..Default::default()
                }),
            );
        }
        match to {
            ItemState::NeedsClarification => self.park(pool, item, state_name(to), "answer", links).await,
            to if !needs_lane(to) => self.free_lane(pool, item, state_name(to), note),
            _ => {}
        }
        self.refresh_record(pool, item);
    }

    /// Settles one session's end on the run (`ItemRun::implementer_finished`, plain code), then what its new
    /// state means for the lane and lease ([`moved`](Self::moved)). A failed session gives its lane and lease
    /// back. A finished slice goes on to `checking`, keeping both, and the loop's next wake steps it.
    async fn settle(&mut self, outcome: SessionOutcome, pool: &mut LanePool, shared: &mut Shared<'_>) {
        let item = outcome.item_id;
        let Some(run) = self.runs.get_mut(&item).filter(|r| r.state == ItemState::Implementing) else {
            tracing::warn!(
                "{}: a session ended for {item}, which isn't implementing here; ignored",
                self.id
            );
            return;
        };
        self.events.add_in(
            &self.id,
            Event::SessionEnd(event::SessionEnd {
                item: Some(item.clone()),
                lane: Some(outcome.lane),
                role: Some("implementer".into()),
                engine: Some("claude".into()),
                ok: Some(matches!(outcome.result, SessionResult::Implementer(_))),
                ..Default::default()
            }),
        );
        match outcome.result {
            SessionResult::Failed(why) => self.hand_back(pool, &item, "queued", &why, shared).await,
            SessionResult::NeedsHuman(why) => {
                run.escalate();
                self.moved(pool, &item, ItemState::Implementing, &why, shared.links)
                    .await;
            }
            SessionResult::Implementer(result) => {
                run.implementer_finished(result, &self.project);
                let note = format!("the session on lane {} ended", outcome.lane);
                self.moved(pool, &item, ItemState::Implementing, &note, shared.links)
                    .await;
            }
        }
    }

    /// Takes in the steps that ended: each item's run as its step left it, then what its new state means
    /// ([`moved`](Self::moved)). A step that panicked is an `error` event and leaves the run as it was.
    pub(super) async fn take_finished(&mut self, pool: &mut LanePool, links: &Links) {
        for (item, after) in self.running.finished() {
            let Some(run) = self.runs.get_mut(&item) else { continue };
            let Some(after) = after else {
                tracing::error!("{}: a step for {item} panicked", self.id);
                let error = event::Error {
                    item: Some(item),
                    r#where: Some("step".into()),
                    error: Some("the step panicked".into()),
                    ..Default::default()
                };
                self.events.add_in(&self.id, Event::Error(error));
                continue;
            };
            let before = std::mem::replace(run, after);
            self.moved(pool, &item, before.state, "the step ended", links).await;
        }
    }

    /// Runs the next step of every item that has one and isn't already running one (the TS agent's
    /// `for (const it of items) this.advance(it.id)`), each as its own task. An `implementing` item's step
    /// is its session, started once (`apply_starts`) and settled when it reports its end, so it's skipped here.
    pub(super) fn advance(&mut self, pool: &LanePool, starter: &dyn SessionStarter) {
        let ready: Vec<(String, ItemRun)> = (self.runs.iter())
            .filter(|(id, run)| has_own_step(run.state) && !self.running.busy(id))
            .map(|(id, run)| (id.clone(), *run))
            .collect();
        for (item_id, run) in ready {
            let start = event::StepStart {
                item: Some(item_id.clone()),
                step: Some(state_name(run.state).into()),
                ..Default::default()
            };
            let request = StepRequest {
                project: self.id.clone(),
                lane: pool.lane_of(&self.id, &item_id),
                token: self.leases.get(&item_id).cloned().unwrap_or_default(),
                item_id: item_id.clone(),
                run,
            };
            // No step built for this state yet: the item waits where it is, and nothing is logged.
            let Some(step) = starter.advance(request) else { continue };
            self.events.add_in(&self.id, Event::StepStart(start));
            self.running.spawn(item_id, step);
        }
    }
}

/// The states [`SessionStarter::advance`] is asked to step: the TS step table, less `implementing`.
fn has_own_step(state: ItemState) -> bool {
    has_step(state) && state != ItemState::Implementing
}

impl Machine {
    /// Sends the releases and parks still owed by the `claim` mode projects (once a pass).
    pub(super) async fn flush_all_owed(&mut self) {
        for lp in (self.projects.iter_mut()).filter(|lp| lp.mode == super::OrchestratorMode::Claim) {
            lp.flush_owed(&self.links).await;
        }
    }

    /// Settles the sessions that ended since the last wake.
    pub(super) async fn settle_outcomes(&mut self) {
        while let Ok(outcome) = self.outcomes.try_recv() {
            match self.projects.iter_mut().find(|lp| lp.id == outcome.project) {
                Some(lp) => {
                    lp.settle(
                        outcome,
                        &mut self.pool,
                        &mut Shared {
                            links: &self.links,
                            claim_paused_until: &mut self.claim_paused_until,
                        },
                    )
                    .await
                }
                None => tracing::warn!("a session ended for the unknown project {}", outcome.project),
            }
        }
    }
}
