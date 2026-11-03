//! One project's orchestrator state and loop: the mode switch, the per-pass decision, and claiming and
//! starting work for that project alone. The lanes and the sessions across every project are the
//! [`super::Machine`]'s.
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::Shared;
use super::report;
use crate::backlog::{BacklogError, ClaimNextAgent, ClaimNextAnswer, Client, Item, ItemStatus};
use crate::config::ProjectConfig;
use crate::engines::{EngineId, Role, engine_for, lane_engine, program_of};
use crate::env::Env;
use crate::implementer::new_session_id;
use crate::item_lifecycle::{ItemRun, ItemState};
use crate::lane_pool::{Holder, LanePool};
use crate::orchestrator::fresh_start::{FreshStart, fresh_for_engine};
use crate::orchestrator::guard::PortGuard;
use crate::orchestrator::live::{HeldLane, LiveHandle, now_iso8601};
use crate::orchestrator::session::branch_name;
use crate::orchestrator::steps::{Running, StepRequest, state_name};
use crate::orchestrator::{Action, ProjectRuns, apply, decide, tick_payload};
use crate::store::{RunRecord, Store};
use crate::telemetry::events::parse_iso_ms;
use crate::telemetry::{Event, EventLog, Telemetry, event, now_ms, watchdog};

/// The variable that switches the orchestrator on.
pub const MODE_VAR: &str = "RETASKABLE_ORCHESTRATOR";

/// What the orchestrator may do. Off by default, so installing a build never starts it by surprise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrchestratorMode {
    /// Nothing runs.
    #[default]
    Off,
    /// Reads the backlog and writes `tick` events saying what a tick would do. Takes no action.
    Dry,
    /// Claims work and applies starts, but only with a [`SessionStarter`] and while the port guard
    /// (`guard.rs`) sees no other agent on this computer. With no starter it behaves exactly like `Dry`.
    Claim,
    /// Takes work as short runs on pieces (pieces.md section 3), with the same port guard as `claim`. The kinds
    /// it can take are the [`RunStarter`](super::RunStarter)s it is given; with none it drops old runs and asks for nothing.
    Runs,
}

impl OrchestratorMode {
    /// The mode `RETASKABLE_ORCHESTRATOR` names (`off`, `dry`, `claim` or `runs`, any case), and a warning
    /// for a value that is none of them, which counts as `off`.
    pub fn from_env(env: &Env) -> (Self, Option<String>) {
        let Some(raw) = env.get(MODE_VAR) else {
            return (Self::Off, None);
        };
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => (Self::Off, None),
            "dry" => (Self::Dry, None),
            "claim" => (Self::Claim, None),
            "runs" => (Self::Runs, None),
            other => (
                Self::Off,
                Some(format!(
                    "{MODE_VAR}={other:?} isn't off, dry, claim or runs, so the orchestrator stays off."
                )),
            ),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Dry => "dry",
            Self::Claim => "claim",
            Self::Runs => "runs",
        }
    }

    /// Reads the mode and logs it once, with the warning or the note that `claim` isn't built.
    pub fn read_and_log(env: &Env) -> Self {
        let (mode, warning) = Self::from_env(env);
        if let Some(w) = warning {
            tracing::warn!("{w}");
        }
        match mode {
            Self::Off => {
                tracing::info!("orchestrator: off (set {MODE_VAR}=dry to read the backlog and log what it would do)")
            }
            Self::Dry => {
                tracing::info!("orchestrator: dry (reads the backlog, logs tick events, never claims or starts work)")
            }
            Self::Claim => tracing::warn!(
                "orchestrator: claim (claims work only once a session runner is given; until then it runs dry)"
            ),
            Self::Runs => tracing::info!(
                "orchestrator: runs (takes work as short runs of the kinds it can run; with none it only drops old runs)"
            ),
        }
        mode
    }
}

/// What one pass of a project's loop did.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// The backlog was read and a `tick` event written; `would` is what the tick decided to do, which
    /// nothing did.
    Ticked { would: Vec<String> },
    /// The pass was skipped after a `store_error` event.
    Skipped,
}

/// The mode the loops really run in. `claim` or `runs` with another agent on this computer runs dry instead,
/// logged loudly, so two agents never take work from one backlog.
pub async fn effective_mode(mode: OrchestratorMode, guard: &PortGuard) -> OrchestratorMode {
    if !matches!(mode, OrchestratorMode::Claim | OrchestratorMode::Runs) {
        return mode;
    }
    match guard.refusal().await {
        Some(why) => {
            tracing::error!("orchestrator: {} refused, running dry instead: {why}", mode.name());
            OrchestratorMode::Dry
        }
        None => mode,
    }
}

/// What a started implementer needs: the item, the lane it holds and its lease token (the TS agent's
/// `hosted.lease`, which every later store write about the item carries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
    pub project: String,
    pub item_id: String,
    pub lane: u32,
    pub token: String,
    /// The claimed item as the store handed it out (title, slice, remaining, spec refs).
    pub item: Item,
    /// The session's id, saved in the state file before it starts: the one `--session-id` names for a new
    /// session, or the saved one `--resume` continues.
    pub session_id: String,
    /// The engine the session runs on: the one it started on when `resume`, else the lane's.
    pub engine: EngineId,
    /// Whether to resume `session_id` (a restart cut it off) rather than start it.
    pub resume: bool,
    /// Where the session reports how it ended; the loop reads it on its next wake.
    pub outcomes: super::OutcomeSender,
    /// The project's latest `state.usage` reading (section 7.1), for picking the implementer's
    /// model tier. `None` when there's no reading yet, which is treated as a full window.
    pub usage: Option<Value>,
    /// `None` starts the session fresh in a prepared lane, as a newly claimed item does. `Some` restarts an
    /// implementer in its existing lane after it ran out of turns or had grown too large (06.2-implementer.md
    /// "at most maxTurns turns"): the lane is reused as-is, keeping any uncommitted work, and this text is the
    /// session's prompt verbatim.
    pub follow_up: Option<String>,
    /// Every item the store listed, for a fresh session's **Items not built yet** list (and the later
    /// check of its `waitsOn` against it).
    pub items: Vec<Item>,
    /// How many times a person started this slice over: the branch is `…-s<slice>-r<attempt>` above 0.
    pub attempt: u32,
    /// A Start over's notes, for the fresh session's first prompt as notes from the last attempt.
    pub notes: Option<String>,
}

/// What [`SessionStarter::abandon`] lets go of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Abandon {
    pub project: String,
    pub branch: Option<String>,
    pub pr_url: Option<String>,
}

/// How `ProjectLoop::launch` starts an implementer session.
pub(super) enum Launch {
    /// A newly claimed item: a fresh session id in a freshly prepared lane.
    Fresh,
    /// A session a restart of this computer cut off: resumes the saved session id in its existing lane.
    Resume(String),
    /// A session that ran out of turns or had grown too large: a fresh session id, reusing the existing lane
    /// as-is, with this text as the picking-up prompt.
    Continuing(String),
    /// A person's Start over: a fresh session in a freshly prepared lane on the new attempt's branch, with these
    /// notes (if any) in its first prompt.
    StartOver(Option<String>),
}

/// Runs the sessions and steps for the work the loop schedules. Built in a later piece (implementer-session and
/// the review stages); until one is given to the [`super::Machine`], `claim` mode claims nothing, so no claimed item is
/// left with nothing working on it.
pub trait SessionStarter: Send + Sync {
    /// Called once the item is `implementing` and its lane held. Must not block: start the session and return;
    /// it reports its end through `request.outcomes`. An `Err` (the session was refused or couldn't start)
    /// gives the lane and the claim straight back.
    fn start(&self, request: StartRequest) -> Result<(), String>;
    /// Sessions running now, on every project: the pseudocode's `activeSessions`, counted against `concurrency`.
    fn active(&self) -> u32;
    /// Start over's first step: closes the item's old PR (`gh pr close`), or drops its old branch when there is
    /// no PR. A slice that was merged is never touched. An `Err` stops the Start over, leaving the item as it is.
    fn abandon(&self, _old: Abandon) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    /// The item's next step for its state (`steps::has_step`, except `implementing`, whose step is its session),
    /// run as its own task: the run as the step left it. `None` when no step is built for that state yet: the item
    /// waits where it is, and the loop asks again on its next wake.
    /// The loop asks again only once it ends. A step that leaves the item in a state that runs nothing in a lane
    /// (`steps::needs_lane`) has pushed its work first: the loop then frees the lane.
    fn advance(&self, request: StepRequest) -> Option<BoxFuture<'static, ItemRun>>;
    /// Runs `command` in `dir` as a Play of lane `lane` (visible, unsandboxed, `RETASKABLE_LANE=<lane>`).
    fn play_lane(&self, dir: &std::path::Path, command: &str, lane: u32) -> Result<Box<dyn LanePlay>, String> {
        crate::play::start_in_lane(dir, command, lane).map(|g| Box::new(g) as Box<dyn LanePlay>)
    }
}

/// A game running in an item's lane.
pub trait LanePlay: Send {
    /// Whether the game has closed.
    fn finished(&self) -> bool;
    /// Ends the game; it is [`finished`](Self::finished) once it is gone.
    fn stop(&self);
}

impl LanePlay for crate::play::LaneGame {
    fn finished(&self) -> bool {
        self.finished()
    }
    fn stop(&self) {
        self.stop()
    }
}

/// One project's orchestrator state, kept for as long as the agent runs. The lanes are not here: they
/// are the computer's, held by the [`super::Machine`].
pub struct ProjectLoop {
    pub(super) id: String,
    pub(super) project: ProjectConfig,
    /// Loaded so a project whose state file can't be read doesn't loop; only `claim` mode writes it (the leases).
    pub(super) store: Store,
    pub(super) runs: ProjectRuns,
    /// Lease token of each item this computer holds, by id; kept in the state file so a restart still holds them.
    pub(super) leases: BTreeMap<String, String>,
    /// Leases given back here that the store hasn't taken yet: item id to (lease token, the give-back's
    /// note), kept in the state file and sent again, `report` then `release`, on later passes (the TS
    /// agent's `unreleased`).
    pub(super) unreleased: BTreeMap<String, (String, String)>,
    /// Leases let go while their item waits, not yet taken by the store: item id to (lease token, what it
    /// waits for, the item's state). Kept in the state file and sent again on later passes.
    pub(super) unparked: BTreeMap<String, (String, String, String)>,
    /// What each leased item is doing (state, lane, session id, branch, slice), kept in the state file so a
    /// restart takes it back as it was. Only items in `leases` have one: it goes when the lease does.
    pub(super) records: BTreeMap<String, RunRecord>,
    /// Items taken back mid-session by the first pass, whose sessions are resumed on the next schedule.
    pub(super) to_resume: Vec<String>,
    /// The item the store handed out for each lease held, by id, for the session that starts on it.
    pub(super) items: BTreeMap<String, Item>,
    /// Where this project's sessions report their end; replaced with the machine's by [`super::Machine::add`].
    pub(super) outcomes: super::OutcomeSender,
    /// Whether the held leases were matched against the backlog yet (once, on the first pass).
    pub(super) adopted: bool,
    pub(super) mode: OrchestratorMode,
    /// Present in `claim` mode: asked again before each claim.
    pub(super) guard: Option<PortGuard>,
    pub(super) events: EventLog,
    pub(super) client: Arc<Client>,
    pub(super) live: LiveHandle,
    /// GitHub's numeric id of `project.github_repo`, asked of `gh` until it answers and then kept.
    pub(super) repo: Option<u64>,
    /// The steps running for this project's items.
    pub(super) running: Running,
    /// What the fetch saw that the store was told last (`report`).
    pub(super) seen: report::Seen,
    /// Set while this project's playtest grouping round is running, so the next minute's pass
    /// doesn't start a second one on top of it ([`super::Machine::playtest_rounds`]).
    pub(super) playtest_round_running: Arc<AtomicBool>,
    /// The watchdog's debounce state (docs/specs/agent-telemetry.md section 4): how long each rule's
    /// finding has held, across minutes.
    pub(super) telemetry: Telemetry,
    /// The run loop's state (`runs.rs`), used only by [`ProjectLoop::run_pass`].
    pub(super) run_loop: super::RunLoop,
    /// A session this size or smaller still resumes across a restart; a bigger one starts fresh instead
    /// (06.2-implementer.md "A session that's grown starts fresh", `MachineConfig::max_resume_context_tokens`).
    max_resume_context_tokens: u32,
    /// Continues waiting for a free lane (`item_commands.rs`).
    pub(super) continuing: Vec<super::item_commands::PendingContinue>,
    /// Start overs waiting for a free lane (`item_commands.rs`).
    pub(super) starting_over: Vec<super::item_commands::PendingContinue>,
    /// The lane each item last held here, so a Continue goes back to its worktree when it's free.
    pub(super) last_lane: BTreeMap<String, u32>,
    /// The games running in lanes (`play { lane }`), by lane, with the item each lane was held for.
    pub(super) playing: BTreeMap<u32, (String, Box<dyn LanePlay>)>,
    /// The spec draft running now, if any (`drafts.rs`): its ids only, in memory.
    pub(super) draft: Option<super::drafts::RunningDraft>,
    /// Approve and Merge presses being carried out, and the items of drafts merged since the store last took headers.
    pub(super) landing: super::land::Landing,
}

impl ProjectLoop {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        project: ProjectConfig,
        store: Store,
        events: EventLog,
        client: Client,
        live: LiveHandle,
        max_resume_context_tokens: u32,
    ) -> Self {
        live.register(&id);
        Self {
            run_loop: super::RunLoop::new(&id),
            id,
            project,
            leases: store.leases(),
            unreleased: store.unreleased(),
            unparked: store.unparked(),
            records: store.runs(),
            to_resume: Vec::new(),
            items: BTreeMap::new(),
            outcomes: super::OutcomeSender(mpsc::unbounded_channel().0),
            store,
            runs: ProjectRuns::new(),
            adopted: false,
            mode: live.snapshot().mode,
            guard: None,
            events,
            client: Arc::new(client),
            live,
            repo: None,
            running: Running::default(),
            seen: report::Seen::default(),
            playtest_round_running: Arc::new(AtomicBool::new(false)),
            telemetry: Telemetry::new(),
            max_resume_context_tokens,
            continuing: Vec::new(),
            starting_over: Vec::new(),
            last_lane: BTreeMap::new(),
            playing: BTreeMap::new(),
            draft: None,
            landing: Default::default(),
        }
    }

    /// Loads the project's state and opens its event log under `<data dir>/projects/<id>`.
    pub fn open(
        dir: &Path,
        project: &ProjectConfig,
        client: Client,
        live: LiveHandle,
        guard: Option<PortGuard>,
        max_resume_context_tokens: u32,
    ) -> Result<Self> {
        let id = project.id_or_derived();
        let project_dir = dir.join("projects").join(&id);
        let store = Store::load(&project_dir).with_context(|| format!("loading {}", project_dir.display()))?;
        let mut events = EventLog::new();
        events.open(&project_dir.join("events"));
        Ok(Self::new(
            id,
            project.clone(),
            store,
            events,
            client,
            live,
            max_resume_context_tokens,
        )
        .with_guard(guard))
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
        self.check_watchdog(paused, &actions, pool);
        Step::Ticked { would }
    }

    /// This minute's watchdog rules (docs/specs/agent-telemetry.md section 4): finds what's quietly gone
    /// wrong against the state this tick already has (`actions`, `pool`) and times it against findings
    /// from minutes before.
    fn check_watchdog(&mut self, paused: bool, actions: &[Action], pool: &LanePool) {
        let recent: Vec<watchdog::RecentEvent> = self
            .events
            .recent()
            .iter()
            .filter_map(|e| {
                let at_ms = parse_iso_ms(&e.at)?;
                let event = e.event()?;
                Some(watchdog::RecentEvent { at_ms, event })
            })
            .collect();
        let now_ms = now_ms();
        let found = watchdog::check(&watchdog::Input {
            project: &self.id,
            paused,
            runs: &self.runs,
            leases: &self.leases,
            records: &self.records,
            running: &self.running,
            pool,
            actions,
            recent: &recent,
            now_ms,
            lease_idle_minutes: self.project.tuning.lease_idle_minutes,
            idle_lanes_minutes: self.project.tuning.idle_lanes_minutes,
            no_progress_minutes: self.project.tuning.no_progress_minutes,
        });
        self.telemetry.minute(&self.id, found, now_ms, &mut self.events);
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
            let session = session.map(|s| (s, record.impl_session_engine));
            self.remember(&id, lane, session, record.impl_context_tokens);
        }
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

    /// Writes the item's run into its record: its state now, `lane`, its branch and slice, `session` and
    /// `context_tokens` (the implementer session's id and its last known size). Does nothing for an item with
    /// no lease. Call `save_leases` after.
    /// The item's Start over attempt on its current slice: 0 until a person starts it over.
    pub(super) fn attempt_of(&self, item_id: &str) -> u32 {
        let slice = self.runs.get(item_id).map_or(0, |r| r.slice);
        self.records
            .get(item_id)
            .filter(|r| r.slice == slice)
            .map_or(0, |r| r.attempt)
    }

    pub(super) fn remember(
        &mut self,
        item_id: &str,
        lane: Option<u32>,
        session: Option<(String, EngineId)>,
        context_tokens: Option<u64>,
    ) {
        let (Some(run), Some(item)) = (self.runs.get(item_id), self.items.get(item_id)) else {
            return;
        };
        if !self.leases.contains_key(item_id) {
            return;
        }
        let (session, engine) = match session {
            Some((id, engine)) => (Some(id), engine),
            None => self
                .records
                .get(item_id)
                .map(|r| (r.impl_session_id.clone(), r.impl_session_engine))
                .unwrap_or_default(),
        };
        let attempt = self.attempt_of(item_id);
        let record = RunRecord {
            state: state_name(run.state).into(),
            lane,
            impl_session_id: session,
            impl_session_engine: engine,
            branch: Some(branch_name(item, attempt)),
            slice: run.slice,
            impl_context_tokens: context_tokens,
            attempt,
        };
        self.records.insert(item_id.to_string(), record);
    }

    /// Refreshes the saved run of an item whose state just moved. A run that was parked or given back has no
    /// lease or record now; the rest are saved as they stand.
    pub(super) fn refresh_record(&mut self, pool: &LanePool, item_id: &str) {
        if self.records.contains_key(item_id) && self.leases.contains_key(item_id) {
            let context_tokens = self.records.get(item_id).and_then(|r| r.impl_context_tokens);
            self.remember(item_id, pool.lane_of(&self.id, item_id), None, context_tokens);
            self.save_leases();
        }
    }

    /// Resumes the sessions the first pass took back mid-run, each as its item's running step. One whose last
    /// known size is over `max_resume_context_tokens` starts fresh instead (06.2-implementer.md "A session
    /// that's grown starts fresh"), keeping its lane as-is. Left for a
    /// later pass, untouched, while Claude Code is signed out on this computer (section 7.3): resuming now
    /// would only fail the same way.
    pub(super) async fn resume_sessions(
        &mut self,
        pool: &mut LanePool,
        starter: &dyn SessionStarter,
        shared: &mut Shared<'_>,
    ) {
        if signed_out(shared) {
            return;
        }
        for id in std::mem::take(&mut self.to_resume) {
            let record = self.records.get(&id).cloned();
            if let Some(RunRecord {
                lane: Some(lane),
                impl_session_id: Some(session),
                impl_session_engine,
                impl_context_tokens,
                ..
            }) = record
            {
                let lane_now = lane_engine(&shared.links.engine_choice, lane).map(program_of);
                let kind = match impl_context_tokens {
                    _ if let Some(reason) = fresh_for_engine(impl_session_engine, lane_now) => {
                        tracing::info!("{}: {id}'s session can't resume ({reason:?}), starting fresh", self.id);
                        self.fresh_launch(&id, lane, reason, "Carry on with the slice.")
                    }
                    Some(tokens) if tokens > u64::from(self.max_resume_context_tokens) => {
                        tracing::info!(
                            "{}: {id}'s session had grown to {tokens} tokens, starting fresh instead of resuming it",
                            self.id
                        );
                        self.fresh_launch(&id, lane, FreshStart::Grown(tokens), "Carry on with the slice.")
                    }
                    _ => {
                        tracing::info!("{}: resuming the session of {id} on lane {lane}", self.id);
                        Launch::Resume(session)
                    }
                };
                self.launch(pool, &id, lane, kind, starter, shared).await;
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
        shared: &mut Shared<'_>,
    ) -> bool {
        let Some(item) = self.items.get(item_id).cloned() else {
            let why = "the session didn't start: the item is unknown here";
            self.hand_back(pool, item_id, "queued", why, shared).await;
            return false;
        };
        let mut notes = None;
        let (resume, follow_up) = match kind {
            Launch::Fresh => (None, None),
            Launch::Resume(session_id) => (Some(session_id), None),
            Launch::Continuing(follow_up) => (None, Some(follow_up)),
            Launch::StartOver(text) => {
                notes = text;
                (None, None)
            }
        };
        let session_id = resume.clone().unwrap_or_else(new_session_id);
        // A resumed session stays on its engine; a new one starts on the lane's.
        let engine = match &resume {
            Some(_) => self
                .records
                .get(item_id)
                .map(|r| r.impl_session_engine)
                .unwrap_or_default(),
            None => program_of(engine_for(
                &shared.links.engine_choice,
                self.project.engines.as_ref(),
                Role::Implementer,
                Some(lane),
            )),
        };
        // A resumed session keeps its last known size; a fresh or picking-up one starts with none of its own yet.
        let context_tokens = resume
            .is_some()
            .then(|| self.records.get(item_id).and_then(|r| r.impl_context_tokens))
            .flatten();
        self.remember(item_id, Some(lane), Some((session_id.clone(), engine)), context_tokens);
        self.save_leases();
        let request = StartRequest {
            project: self.id.clone(),
            item_id: item_id.to_string(),
            lane,
            token: self.leases.get(item_id).cloned().unwrap_or_default(),
            item,
            session_id,
            engine,
            resume: resume.is_some(),
            outcomes: self.outcomes.clone(),
            usage: self.store.usage(),
            follow_up,
            items: self.items.values().cloned().collect(),
            attempt: self.attempt_of(item_id),
            notes,
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
                self.hand_back(pool, item_id, "queued", &why, shared).await;
                false
            }
        }
    }

    /// Applies the starts decided to the real runs and pool, and calls the starter for each that took its
    /// lane. A start the starter refuses gives the lane and the claim straight back. A start left for a
    /// later pass, untouched, while Claude Code is signed out on this computer (section 7.3): the item stays
    /// queued with its lane free, so `decide` plans the same start again once someone signs back in. How many
    /// sessions started that take a session slot.
    pub(super) async fn apply_starts(
        &mut self,
        actions: &[Action],
        pool: &mut LanePool,
        starter: &dyn SessionStarter,
        shared: &mut Shared<'_>,
    ) -> u32 {
        let blocked_from_starting = signed_out(shared) || shared.stopping;
        let mut started = 0;
        for action in actions.iter().filter(|a| **a != Action::AskForWork) {
            if blocked_from_starting && matches!(action, Action::StartImplementer { .. }) {
                continue;
            }
            if !apply(&self.id, action, &mut self.runs, pool, &self.project, &mut self.events) {
                continue;
            }
            if let Action::StartImplementer { item_id, lane } = action {
                started += u32::from(self.launch(pool, item_id, *lane, Launch::Fresh, starter, shared).await);
            }
        }
        started
    }

    /// Asks the store for work while a lane is free and a session slot is left (`slots`, used up as work
    /// starts), and starts what it hands out. The tick's `claim`, the last answer: the item id, `none` or
    /// `failed`; `None` when it wasn't asked (no slot or lane, the guard, an own failure's claim pause,
    /// Claude signed out on this computer, or no link).
    pub(super) async fn claim(
        &mut self,
        pool: &mut LanePool,
        slots: &mut u32,
        starter: &dyn SessionStarter,
        shared: &mut Shared<'_>,
    ) -> Option<String> {
        if shared
            .claim_paused_until
            .is_some_and(|until| std::time::Instant::now() < until)
            || signed_out(shared)
            || shared.stopping
        {
            return None;
        }
        let mut last = None;
        // At most once per lane, so a store that keeps handing out work no lane takes can't hold the pass.
        for _ in 0..pool.rules().lanes {
            let asks = decide(&self.id, false, &self.runs, pool, &self.project).contains(&Action::AskForWork);
            if *slots == 0 || !asks {
                break;
            }
            let Some((answer, more)) = self.claim_once(pool, slots, starter, shared).await else {
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
    pub(super) async fn claim_once(
        &mut self,
        pool: &mut LanePool,
        slots: &mut u32,
        starter: &dyn SessionStarter,
        shared: &mut Shared<'_>,
    ) -> Option<(String, bool)> {
        // Asked before every claim, so a TS agent started since is noticed at once.
        if let Some(guard) = self.guard.clone()
            && let Some(why) = guard.refusal().await
        {
            tracing::error!("{}: not claiming: {why}", self.id);
            return None;
        }
        let repo = self.repo?;
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return None,
            Err(e) => {
                self.cant_start(format!("reading the backlog link: {e:#}"));
                return None;
            }
        };
        let agent = ClaimNextAgent {
            machine: shared.links.machine.clone(),
            environment: shared.links.environment.clone(),
            base_branch: self.project.base_branch.clone(),
            tags: shared.links.tags.clone(),
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
                *slots = slots.saturating_sub(self.apply_starts(&actions, pool, starter, shared).await);
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

/// Whether Claude Code was found signed out on this computer recently enough that no new or resumed
/// session should start yet ([`Machine::claude_signed_out_until`](super::Machine), section 7.3).
pub(super) fn signed_out(shared: &Shared<'_>) -> bool {
    shared
        .claude_signed_out_until
        .is_some_and(|until| std::time::Instant::now() < until)
}

fn describe(action: &Action) -> String {
    match action {
        Action::StartImplementer { item_id, lane } => format!("start_implementer {item_id} in lane {lane}"),
        Action::NeedsHuman { item_id } => format!("needs_human {item_id}"),
        Action::AskForWork => "ask_for_work".into(),
    }
}
