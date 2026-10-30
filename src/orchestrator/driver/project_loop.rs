//! `ProjectLoop`'s own lane and lease bookkeeping: loading and saving its state file, taking a lease
//! into `runs`, taking an item's leases back on a restart ([`readopt`](ProjectLoop::readopt)), starting
//! (or resuming) an implementer session in a held lane, and asking the store for more work
//! ([`claim`](ProjectLoop::claim)/[`claim_once`](ProjectLoop::claim_once)). Settling a session or step's
//! end lives in `settle.rs`; telling the store what the fetch saw lives in `report.rs`.
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{Launch, Links, OutcomeSender, ProjectLoop, SessionStarter, StartRequest, Step, describe, report};
use crate::backlog::{BacklogError, ClaimNextAgent, ClaimNextAnswer, Client, Item, ItemStatus};
use crate::config::ProjectConfig;
use crate::implementer::new_session_id;
use crate::item_lifecycle::{ItemRun, ItemState};
use crate::lane_pool::{Holder, LanePool};
use crate::link::TokenStore;
use crate::orchestrator::guard::PortGuard;
use crate::orchestrator::live::{HeldLane, LiveHandle, now_iso8601};
use crate::orchestrator::session::branch_name;
use crate::orchestrator::steps::state_name;
use crate::orchestrator::{Action, ProjectRuns, apply, decide, tick_payload};
use crate::store::{RunRecord, Store};
use crate::telemetry::{Event, EventLog, event};

impl ProjectLoop {
    pub fn new(
        id: String,
        project: ProjectConfig,
        store: Store,
        events: EventLog,
        client: Client,
        live: LiveHandle,
    ) -> Self {
        live.register(&id);
        Self {
            id,
            project,
            leases: store.leases(),
            unreleased: store.unreleased(),
            unparked: store.unparked(),
            records: store.runs(),
            to_resume: Vec::new(),
            items: BTreeMap::new(),
            outcomes: OutcomeSender(mpsc::unbounded_channel().0),
            store,
            runs: ProjectRuns::new(),
            adopted: false,
            mode: live.snapshot().mode,
            guard: None,
            events,
            client: Arc::new(client),
            live,
            repo: None,
            running: super::super::steps::Running::default(),
            seen: report::Seen::default(),
        }
    }

    /// Loads the project's state and opens its event log under `<data dir>/projects/<id>`.
    pub fn open(
        dir: &Path,
        project: &ProjectConfig,
        client: Client,
        live: LiveHandle,
        guard: Option<PortGuard>,
    ) -> Result<Self> {
        let id = project.id_or_derived();
        let project_dir = dir.join("projects").join(&id);
        let store = Store::load(&project_dir).with_context(|| format!("loading {}", project_dir.display()))?;
        let mut events = EventLog::new();
        events.open(&project_dir.join("events"));
        Ok(Self::new(id, project.clone(), store, events, client, live).with_guard(guard))
    }

    /// Asks the port guard before each claim (set in `claim` mode).
    pub fn with_guard(mut self, guard: Option<PortGuard>) -> Self {
        self.guard = guard;
        self
    }

    pub fn events(&self) -> &EventLog {
        &self.events
    }

    /// The pass's second half for a project whose items were read: records what its turns decided
    /// (`actions`, from `round_robin::plan`) against the computer's `pool`, and writes the `tick` event.
    pub(super) fn record(
        &mut self,
        items: Vec<Item>,
        paused: bool,
        actions: Vec<Action>,
        pool: &LanePool,
        live: Option<Option<String>>,
    ) -> Step {
        let mut states: BTreeMap<String, u32> = BTreeMap::new();
        for item in &items {
            if let Ok(Value::String(s)) = serde_json::to_value(item.status) {
                *states.entry(s).or_default() += 1;
            }
        }
        // The dry loop applies nothing, so the real runs, pool and log see only the `tick` event below.
        // The lanes held are the computer's: this project's are the ones it shows.
        let held: Vec<HeldLane> = pool
            .held_lanes()
            .filter_map(|lane| match pool.holder(lane)? {
                Holder::Item { project, item } | Holder::Play { project, item } if *project == self.id => {
                    Some(HeldLane {
                        lane,
                        title: items.iter().find(|i| &i.id == item).map(|i| i.title.clone()),
                        item_id: item.clone(),
                    })
                }
                _ => None,
            })
            .collect();
        let payload = tick_payload(paused, pool);
        let would: Vec<String> = actions.iter().map(describe).collect();
        let decision = match actions.first() {
            None if paused => "paused: would do nothing".to_string(),
            None => "would do nothing".to_string(),
            Some(Action::AskForWork) if live.is_some() => "asked for work".to_string(),
            Some(Action::AskForWork) => "would ask for work".to_string(),
            Some(Action::StartImplementer { item_id, lane }) if live.is_some() => {
                format!("started {item_id} on lane {lane}")
            }
            Some(Action::StartImplementer { item_id, lane }) => format!("would start {item_id} on lane {lane}"),
            Some(Action::NeedsHuman { .. }) => unreachable!("the plan lists no needs_human"),
        };
        self.live.ticked(&self.id, paused, now_iso8601(), held, decision);
        let mut payload = payload;
        payload.states = Some(json!(states));
        // `live` is the claim answer when this pass really applied its decisions (`claim` mode with a starter).
        payload.claim = Some(json!(live.clone().flatten().unwrap_or_else(|| "not asked".into())));
        if live.is_none() {
            payload.extra.insert("dry".into(), json!(would));
        }
        self.events.add_in(&self.id, Event::Tick(payload));
        if !would.is_empty() {
            tracing::debug!(
                "{}: tick {}: {}",
                self.id,
                if live.is_some() { "did" } else { "would" },
                would.join(", ")
            );
        }
        Step::Ticked { would }
    }

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
    pub(super) fn remember(&mut self, item_id: &str, lane: Option<u32>, session: Option<String>) {
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
                self.launch(pool, &id, lane, Launch::Resume(session), starter, links)
                    .await;
            }
        }
    }

    /// Starts (or, with a saved `resume` session id, resumes) the implementer session for an item that holds
    /// its lane. The session id is saved before the session starts. A start the starter refuses gives the
    /// lane and the claim straight back. Whether a session started that takes a session slot.
    pub(super) async fn launch(
        &mut self,
        pool: &mut LanePool,
        item_id: &str,
        lane: u32,
        kind: Launch,
        starter: &dyn SessionStarter,
        links: &Links,
    ) -> bool {
        let Some(item) = self.items.get(item_id).cloned() else {
            let why = "the session didn't start: the item is unknown here";
            self.hand_back(pool, item_id, "queued", why, links).await;
            return false;
        };
        let (resume, follow_up) = match kind {
            Launch::Fresh => (None, None),
            Launch::Resume(session_id) => (Some(session_id), None),
            Launch::Continuing(follow_up) => (None, Some(follow_up)),
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
            follow_up,
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
                started += u32::from(self.launch(pool, item_id, *lane, Launch::Fresh, starter, links).await);
            }
        }
        started
    }

    /// Asks the store for work while a lane is free and a session slot is left (`slots`, used up as work
    /// starts), and starts what it hands out. The tick's `claim`, the last answer: the item id, `none` or
    /// `failed`; `None` when it wasn't asked (no slot or lane, the guard, or no link).
    pub(super) async fn claim(
        &mut self,
        pool: &mut LanePool,
        slots: &mut u32,
        links: &Links,
        starter: &dyn SessionStarter,
    ) -> Option<String> {
        let mut last = None;
        // At most once per lane, so a store that keeps handing out work no lane takes can't hold the pass.
        for _ in 0..pool.rules().lanes {
            let asks = decide(&self.id, false, &self.runs, pool, &self.project).contains(&Action::AskForWork);
            if *slots == 0 || !asks {
                break;
            }
            let Some((answer, more)) = self.claim_once(pool, slots, links, starter).await else {
                break;
            };
            last = Some(answer);
            if !more {
                break;
            }
        }
        last
    }

    /// One `claim_next`, with its `claim` event: the answer and whether to ask again (something was handed out).
    async fn claim_once(
        &mut self,
        pool: &mut LanePool,
        slots: &mut u32,
        links: &Links,
        starter: &dyn SessionStarter,
    ) -> Option<(String, bool)> {
        // Asked before every claim, so a TS agent started since is noticed at once.
        if let Some(guard) = self.guard.clone()
            && let Some(why) = guard.refusal().await
        {
            tracing::error!("{}: not claiming: {why}", self.id);
            return None;
        }
        let repo = self.repo?;
        let token = match links.keychain.get(&links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return None,
            Err(e) => {
                self.cant_start(format!("reading the backlog link: {e:#}"));
                return None;
            }
        };
        let agent = ClaimNextAgent {
            machine: links.machine.clone(),
            environment: links.environment.clone(),
            base_branch: self.project.base_branch.clone(),
        };
        let (answer, more) = match self.client.claim_next(repo, Some(&token), &agent).await {
            Err(e) => {
                self.store_error("claim_next", &e);
                ("failed".to_string(), false)
            }
            Ok(ClaimNextAnswer::Nothing) => ("none".to_string(), false),
            // Handed out again while this computer still has it: it isn't taken twice.
            Ok(ClaimNextAnswer::Item { item, .. }) if self.runs.contains_key(&item.id) => {
                tracing::warn!(
                    "{}: the store handed out {} again, which this computer has",
                    self.id,
                    item.id
                );
                (item.id, false)
            }
            Ok(ClaimNextAnswer::Item { item, token }) => {
                self.take_lease(&item, token, ItemState::Queued);
                let actions = decide(&self.id, false, &self.runs, pool, &self.project);
                *slots = slots.saturating_sub(self.apply_starts(&actions, pool, starter, links).await);
                // A start refused hands the item back; asking again now could only be handed it again.
                let kept = self.runs.contains_key(&item.id);
                (item.id, kept)
            }
        };
        self.events.add_in(
            &self.id,
            Event::Claim(event::Claim {
                answer: Some(json!(answer)),
                ..Default::default()
            }),
        );
        Some((answer, more))
    }

    /// A pass that couldn't even ask the store (no repo id, no link): the page hears of it too.
    pub(super) fn cant_start(&self, message: String) {
        self.live.failed(&self.id, now_iso8601(), message);
    }

    pub(super) fn store_error(&mut self, op: &str, e: &BacklogError) {
        tracing::warn!("{}: {op} failed: {e}", self.id);
        self.live.failed(&self.id, now_iso8601(), e.to_string());
        self.events.add_in(
            &self.id,
            Event::StoreError(event::StoreError {
                op: Some(op.into()),
                code: e.code().map(|c| json!(c)),
                error: Some(e.to_string()),
                ..Default::default()
            }),
        );
    }
}
