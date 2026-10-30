//! Lease bookkeeping and session launching for [`super::ProjectLoop`]: taking a lease, recovering what a
//! restart still holds (the TS agent's `recover`), and starting or resuming an implementer session.
use super::{Links, ProjectLoop, SessionStarter, StartRequest};
use crate::backlog::{Item, ItemStatus};
use crate::implementer::new_session_id;
use crate::item_lifecycle::{ItemRun, ItemState};
use crate::lane_pool::{Holder, LanePool};
use crate::orchestrator::session::branch_name;
use crate::orchestrator::steps::state_name;
use crate::orchestrator::{Action, apply};
use crate::store::RunRecord;
use crate::telemetry::{Event, event};

impl ProjectLoop {
    /// Writes the held and the unreleased leases to the state file.
    pub(super) fn save_leases(&mut self) {
        self.store.set_leases(&self.leases);
        self.store.set_unreleased(&self.unreleased);
        self.store.set_unparked(&self.unparked);
        self.records.retain(|id, _| self.leases.contains_key(id));
        self.store.set_runs(&self.records);
        if let Err(e) = self.store.save() {
            tracing::warn!("{}: saving the held leases failed: {e:#}", self.id);
        }
    }

    /// Takes a lease into `runs` in `state` (queued for a new claim), with the `lease` event the TS agent's
    /// store writes.
    pub(super) fn take_lease(&mut self, item: &Item, token: String, state: ItemState) {
        let mut run = ItemRun::new();
        run.state = state;
        run.slice = item.slice;
        self.runs.insert(item.id.clone(), run);
        self.items.insert(item.id.clone(), item.clone());
        self.leases.insert(item.id.clone(), token);
        self.save_leases();
        self.events.add_in(
            &self.id,
            Event::Lease(event::Lease {
                item: Some(item.id.clone()),
                change: Some("taken".into()),
                state: Some(state_name(state).into()),
                ..Default::default()
            }),
        );
    }

    /// On the first pass, takes back the items this computer still holds a lease on, each as its saved run
    /// says (the TS agent's `recover`: items keep their state and lane across a restart). An implementing item
    /// takes its lane again and has its session resumed (on the next schedule, as its running step: the
    /// starter counts it, and the loop never starts a second); one in checking takes its lane and the loop
    /// steps it as usual; one that needs a human has no lane and keeps its lease. A lease with no saved run
    /// (an older state file) starts again as queued. An item with a run the store no longer shows as handed
    /// out (taken by another agent, finished, gone) is dropped. A needs_clarification item was parked, so it
    /// holds no lease and isn't here: the store hands it back when it is answered.
    pub(super) fn readopt(&mut self, items: &[Item], pool: &mut LanePool) {
        if std::mem::replace(&mut self.adopted, true) || self.leases.is_empty() {
            return;
        }
        for (id, token) in std::mem::take(&mut self.leases) {
            let record = self.records.remove(&id);
            let Some(item) = items.iter().find(|i| i.id == id) else {
                tracing::info!("{}: no longer holds {id}, dropping its lease", self.id);
                continue;
            };
            let handed = matches!(item.status, ItemStatus::Claimed | ItemStatus::Building);
            let (state, ok) = match record.as_ref().map(|r| r.state.as_str()) {
                Some("needs_human") => (ItemState::NeedsHuman, handed || item.status == ItemStatus::NeedsPerson),
                Some("checking") => (ItemState::Checking, handed || item.status == ItemStatus::InReview),
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
            let session = record.impl_session_id.clone();
            let mut lane = None;
            if matches!(state, ItemState::Checking | ItemState::Implementing) {
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
            self.remember(&id, lane, session);
        }
        self.save_leases();
    }

    /// Takes `lane` back for an item that held it before the restart, if it is still free.
    fn retake_lane(&mut self, pool: &mut LanePool, item_id: &str, lane: u32) -> bool {
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

    /// Writes the item's run into its record: its state now, `lane`, its branch and slice, and `session` (or
    /// the one saved before). Does nothing for an item with no lease. Call `save_leases` after.
    fn remember(&mut self, item_id: &str, lane: Option<u32>, session: Option<String>) {
        let (Some(run), Some(item)) = (self.runs.get(item_id), self.items.get(item_id)) else {
            return;
        };
        if !self.leases.contains_key(item_id) {
            return;
        }
        let session = session.or_else(|| self.records.get(item_id).and_then(|r| r.impl_session_id.clone()));
        let record = RunRecord {
            state: state_name(run.state).into(),
            lane,
            impl_session_id: session,
            branch: Some(branch_name(item)),
            slice: run.slice,
        };
        self.records.insert(item_id.to_string(), record);
    }

    /// Refreshes the saved run of an item whose state just moved. A run that was parked or given back has no
    /// lease or record now; the rest are saved as they stand.
    pub(super) fn refresh_record(&mut self, pool: &LanePool, item_id: &str) {
        if self.records.contains_key(item_id) && self.leases.contains_key(item_id) {
            self.remember(item_id, pool.lane_of(&self.id, item_id), None);
            self.save_leases();
        }
    }

    /// Resumes the sessions the first pass took back mid-run, each as its item's running step.
    pub(super) async fn resume_sessions(&mut self, pool: &mut LanePool, starter: &dyn SessionStarter, links: &Links) {
        for id in std::mem::take(&mut self.to_resume) {
            let record = self.records.get(&id).cloned();
            if let Some(RunRecord {
                lane: Some(lane),
                impl_session_id: Some(session),
                ..
            }) = record
            {
                tracing::info!("{}: resuming the session of {id} on lane {lane}", self.id);
                self.launch(pool, &id, lane, Some(session), starter, links).await;
            }
        }
    }

    /// Starts (or, with a saved `resume` session id, resumes) the implementer session for an item that holds
    /// its lane. The session id is saved before the session starts. A start the starter refuses gives the
    /// lane and the claim straight back. Whether a session started that takes a session slot.
    async fn launch(
        &mut self,
        pool: &mut LanePool,
        item_id: &str,
        lane: u32,
        resume: Option<String>,
        starter: &dyn SessionStarter,
        links: &Links,
    ) -> bool {
        let Some(item) = self.items.get(item_id).cloned() else {
            let why = "the session didn't start: the item is unknown here";
            self.hand_back(pool, item_id, "queued", why, links).await;
            return false;
        };
        let session_id = resume.clone().unwrap_or_else(new_session_id);
        self.remember(item_id, Some(lane), Some(session_id.clone()));
        self.save_leases();
        let request = StartRequest {
            project: self.id.clone(),
            item_id: item_id.to_string(),
            lane,
            token: self.leases.get(item_id).cloned().unwrap_or_default(),
            item,
            session_id,
            resume: resume.is_some(),
            outcomes: self.outcomes.clone(),
        };
        match starter.start(request) {
            Ok(()) => {
                self.events.add_in(
                    &self.id,
                    Event::SessionStart(event::SessionStart {
                        item: Some(item_id.to_string()),
                        lane: Some(lane),
                        role: Some("implementer".into()),
                        engine: Some("claude".into()),
                        ..Default::default()
                    }),
                );
                // A "do next" item's session takes no slot (section 7).
                !self.items.get(item_id).is_some_and(|i| i.do_next)
            }
            Err(why) => {
                tracing::warn!("{}: the session for {item_id} didn't start: {why}", self.id);
                let why = format!("the session didn't start: {why}");
                self.hand_back(pool, item_id, "queued", &why, links).await;
                false
            }
        }
    }

    /// Applies the starts decided to the real runs and pool, and calls the starter for each that took its
    /// lane. A start the starter refuses gives the lane and the claim straight back. How many sessions started
    /// that take a session slot.
    pub(super) async fn apply_starts(
        &mut self,
        actions: &[Action],
        pool: &mut LanePool,
        starter: &dyn SessionStarter,
        links: &Links,
    ) -> u32 {
        let mut started = 0;
        for action in actions.iter().filter(|a| **a != Action::AskForWork) {
            if !apply(&self.id, action, &mut self.runs, pool, &self.project, &mut self.events) {
                continue;
            }
            if let Action::StartImplementer { item_id, lane } = action {
                started += u32::from(self.launch(pool, item_id, *lane, None, starter, links).await);
            }
        }
        started
    }
}
