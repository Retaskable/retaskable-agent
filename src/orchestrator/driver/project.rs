//! One project's orchestrator state and loop: the mode switch, the per-pass decision, and claiming and
//! starting work for that project alone. The lanes and the sessions across every project are the
//! [`super::Machine`]'s.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::report;
pub(super) use super::signed_out::signed_out;
use super::start_gate::StartGate;
use super::{LanePlay, Shared};
use crate::backlog::{Client, Item, ItemStatus};
use crate::config::ProjectConfig;
use crate::engines::{EngineId, Role, engine_for, program_of};
use crate::env::Env;
use crate::implementer::new_session_id;
use crate::item_lifecycle::{ItemRun, ItemState};
use crate::lane_pool::{Holder, LanePool};
use crate::orchestrator::guard::PortGuard;
use crate::orchestrator::live::{HeldLane, LiveHandle, now_iso8601};
use crate::orchestrator::session::branch_name;
use crate::orchestrator::steps::{Running, StepRequest, state_name};
use crate::orchestrator::{Action, ProjectRuns, apply, tick_payload};
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
    /// The `engine-placement` release flag: Off, only a Claude session starts, and a fresh start stays on Claude.
    pub placement: bool,
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
    /// The engine that takes over from `engine` if it is at its usage limit when the code gates send findings
    /// back (engines/03 section 3.5): the session starts fresh on it. `None` when `engine` isn't limited or
    /// nothing else can take it.
    pub takeover: Option<EngineId>,
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
    /// [`Continuing`](Self::Continuing) on this engine, not the lane's: the session's own engine is at its usage
    /// limit and this one takes over.
    ContinuingOn(String, EngineId),
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
    /// Whether a fresh Claude implementer start may begin now, given the project's `state.usage`: no when the model
    /// it would run on has a cap and no slot free (07.1). `pending` counts the Claude implementer starts already made this pass, which haven't taken their slot yet. Asked only while `model-concurrency-caps` is On.
    fn implementer_may_start(&self, _usage: Option<&serde_json::Value>, _pending: u32) -> bool {
        true
    }
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
    /// Starts a read-only triager session on a stopped item (06.9). Must not block; it reports its end through
    /// `request.outcomes` as [`SessionResult::Triage`](super::SessionResult::Triage). An `Err` counts as a failed look.
    fn triage(&self, _request: super::TriageRequest) -> Result<(), String> {
        Err("this computer has no triager".into())
    }
    /// Starts the top-tier read-only second look at a triager's `human` answer with a suggestion (06.9 bullet 3). Must
    /// not block; it reports its end as [`SessionResult::SecondLook`](super::SessionResult::SecondLook). An `Err`
    /// leaves the answer as `human`.
    fn second_look(&self, _request: super::SecondLookRequest) -> Result<(), String> {
        Err("this computer has no second look".into())
    }
    /// Runs `command` in `dir` as a Play of lane `lane` (visible, unsandboxed, `RETASKABLE_LANE=<lane>`).
    fn play_lane(&self, dir: &std::path::Path, command: &str, lane: u32) -> Result<Box<dyn LanePlay>, String> {
        crate::play::start_in_lane(dir, command, lane).map(|g| Box::new(g) as Box<dyn LanePlay>)
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
    /// Items a person skipped whose `skip` the store hasn't taken yet; sent once their release has landed, and
    /// again on later passes until it answers (the TS agent's `unreleased.skip`). Kept in the state file.
    pub(super) unskipped: std::collections::BTreeSet<String>,
    /// Playtest answers for the store's try-out queue not yet taken (id to passed and notes); see `playtest_owed.rs`.
    pub(super) unanswered: BTreeMap<String, (bool, Option<String>)>,
    /// Leases let go while their item waits, not yet taken by the store: item id to (lease token, what it
    /// waits for, the item's state). Kept in the state file and sent again on later passes.
    pub(super) unparked: BTreeMap<String, (String, String, String)>,
    /// What each leased item is doing (state, lane, session id, branch, slice), kept in the state file so a
    /// restart takes it back as it was. Only items in `leases` have one: it goes when the lease does.
    pub(super) records: BTreeMap<String, RunRecord>,
    /// Items taken back mid-session by the first pass, whose sessions are resumed on the next schedule.
    pub(super) to_resume: Vec<String>,
    /// The computer's data folder, where session transcripts are kept (none in a loop built without one).
    pub(super) data_dir: Option<PathBuf>,
    /// Whether the saved `state.usage` has been checked once against the weekly cutoff (`weekly-limit-reading`).
    pub(super) week_checked: bool,
    /// Claude sessions' usage readings that settled since the pacer last took them (`driver/auto.rs`).
    pub(super) pace_readings: Vec<Value>,
    /// Whether the saved usage has been given to the pacer once.
    pub(super) pace_checked: bool,
    /// `autoConcurrency.enabled`, set by the machine before it settles a session: with the `auto-concurrency` and
    /// `auto-concurrency-week` flags it also keeps the week reading (`settle.rs`).
    pub(super) auto_enabled: bool,
    /// Items whose session the watchdog killed, by when it is resumed (`own-failure-handling`).
    pub(super) silenced: BTreeMap<String, std::time::Instant>,
    /// Claude implementer starts made in this scheduling pass, which haven't taken a model slot yet.
    pub(super) claude_starts: u32,
    /// Each item's own failures in a row (`state.ownFailures`): a session that wouldn't start, or a hand-back.
    pub(super) own_failures: BTreeMap<String, u32>,
    /// Items whose session was started as a `Launch::Fresh` and hasn't ended yet; one that couldn't start is
    /// started fresh again rather than resumed by an id that never ran.
    pub(super) fresh_launched: std::collections::BTreeSet<String>,
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
    /// The Retaskable release flags (`ask-claude` gates the draft step's chat messages).
    pub(super) flags: Arc<crate::flags::Flags>,
    pub(super) live: LiveHandle,
    /// GitHub's numeric id of `project.github_repo`, asked of `gh` until it answers and then kept.
    pub(super) repo: Option<u64>,
    /// The steps running for this project's items.
    pub(super) running: Running,
    /// The steps in `running`, by item, for their `step_end`.
    pub(super) open_steps: std::collections::HashMap<String, super::step_events::OpenStep>,
    /// What the fetch saw that the store was told last (`report`).
    pub(super) seen: report::Seen,
    /// What the chat was told about approved drafts that is still waiting on staging.
    pub(super) follow: super::follow::Follow,
    /// Set while this project's playtest grouping round is running, so the next minute's pass
    /// doesn't start a second one on top of it ([`super::Machine::playtest_rounds`]).
    pub(super) playtest_round_running: Arc<AtomicBool>,
    /// Set while this project's ideas round is running, so a second one doesn't start on top of it.
    pub(super) ideas_round_running: Arc<AtomicBool>,
    /// The daily vet pass's state and its own running guard ([`super::vet_daily`]).
    pub(super) vet_daily: Arc<std::sync::Mutex<super::vet_daily::State>>,
    pub(super) vet_running: Arc<AtomicBool>,
    /// The daily ideas round's asker and last tried day ([`super::ideas_daily`]).
    pub(super) ideas_daily: Arc<std::sync::Mutex<super::ideas_daily::State>>,
    /// Set while this project's deprecated-specs check is running, and its asker and last checked day
    /// ([`super::deprecations`]).
    pub(super) deprecations_running: Arc<AtomicBool>,
    pub(super) deprecations: Arc<std::sync::Mutex<super::deprecations::State>>,
    /// The watchdog's debounce state (docs/specs/agent-telemetry.md section 4): how long each rule's
    /// finding has held, across minutes.
    pub(super) telemetry: Telemetry,
    /// The run loop's state (`runs.rs`), used only by [`ProjectLoop::run_pass`].
    pub(super) run_loop: super::RunLoop,
    /// A session this size or smaller still resumes across a restart; a bigger one starts fresh instead
    /// (06.2-implementer.md "A session that's grown starts fresh", `MachineConfig::max_resume_context_tokens`).
    pub(super) max_resume_context_tokens: u32,
    /// Continues waiting for a free lane (`item_commands.rs`).
    pub(super) continuing: Vec<super::item_commands::PendingContinue>,
    /// Failed playtest answers waiting for a lane: the item and the message to resume the implementer with.
    pub(super) playtest_fails: Vec<super::item_commands::PendingContinue>,
    /// Start overs waiting for a free lane (`item_commands.rs`).
    pub(super) starting_over: Vec<super::item_commands::PendingContinue>,
    /// The lane each item last held here, so a Continue goes back to its worktree when it's free.
    pub(super) last_lane: BTreeMap<String, u32>,
    /// The games running in lanes (`play { lane }`), by lane, with the item each lane was held for.
    pub(super) playing: BTreeMap<u32, (String, Box<dyn LanePlay>)>,
    /// The spec draft running now, if any (`drafts.rs`): its ids only, in memory.
    pub(super) draft: Option<super::drafts::RunningDraft>,
    /// Store failures of the draft and ask tasks, written to the event log by [`Self::drain_failures`].
    pub(super) failures: crate::backlog::failures::Failures,
    /// Approve and Merge presses being carried out, and the items of drafts merged since the store last took headers.
    pub(super) landing: super::land::Landing,
    /// The stopped items a triager is looking into or about to (`claudeOnIt`), in memory only.
    pub(super) triaging: std::collections::BTreeSet<String>,
    /// Retaskable bugs sent to the hub that haven't been answered (triage_bug.rs).
    pub(super) bug_sends: Vec<super::triage_bug::BugSend>,
    /// Triager answers waiting on their second look, by item (`claudeOnIt` stays on meanwhile), in memory only.
    pub(super) second_looking: BTreeMap<String, super::triage_run::Held>,
    /// Triager answers whose second look hasn't started yet: item, its lane (0 for none) and the answer.
    pub(super) second_due: Vec<(String, u32, super::triage_run::Held)>,
    /// Items whose entry in `continuing` or `starting_over` is Claude's decision, not a person's (06.9 bullet 17).
    pub(super) claude_pending: std::collections::BTreeSet<String>,
    /// The `triage-session` flag as of the last pass.
    pub(super) triage_flag: bool,
    /// The `engine-placement` release flag, read with `triage_flag`.
    pub(super) placement_flag: bool,
    /// The chat's asks, waiting and running (`asks.rs`).
    pub(super) asks: super::asks::Asks,
    /// The trial plan's session running now, if any (`trial.rs`), in memory only.
    pub(super) trial: Option<super::trial::RunningTrial>,
    /// How many items the backlog held when this project was last read.
    pub(super) backlog_items: usize,
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
        let vet = store.vet();
        live.set_vet(&id, vet.as_ref().map(|r| r.counts.clone()));
        Self {
            run_loop: super::RunLoop::new(&id),
            id,
            project,
            leases: store.leases(),
            unreleased: store.unreleased(),
            unskipped: store.unskipped(),
            unanswered: store.unanswered(),
            unparked: store.unparked(),
            records: store.runs(),
            to_resume: Vec::new(),
            data_dir: None,
            week_checked: false,
            pace_readings: Vec::new(),
            pace_checked: false,
            auto_enabled: false,
            silenced: BTreeMap::new(),
            claude_starts: 0,
            own_failures: store.own_failures(),
            fresh_launched: Default::default(),
            items: BTreeMap::new(),
            outcomes: super::OutcomeSender(mpsc::unbounded_channel().0),
            store,
            runs: ProjectRuns::new(),
            adopted: false,
            mode: live.snapshot().mode,
            guard: None,
            events,
            client: Arc::new(client),
            flags: Arc::new(crate::flags::Flags::from_env()),
            live,
            repo: None,
            running: Running::default(),
            open_steps: Default::default(),
            seen: report::Seen::default(),
            follow: Default::default(),
            playtest_round_running: Arc::new(AtomicBool::new(false)),
            ideas_round_running: Arc::new(AtomicBool::new(false)),
            vet_daily: Arc::new(std::sync::Mutex::new(super::vet_daily::State::from_record(vet))),
            vet_running: Arc::new(AtomicBool::new(false)),
            ideas_daily: Default::default(),
            deprecations_running: Default::default(),
            deprecations: Default::default(),
            telemetry: Telemetry::new(),
            max_resume_context_tokens,
            continuing: Vec::new(),
            playtest_fails: Vec::new(),
            starting_over: Vec::new(),
            last_lane: BTreeMap::new(),
            playing: BTreeMap::new(),
            draft: None,
            failures: Default::default(),
            landing: Default::default(),
            triaging: Default::default(),
            bug_sends: Vec::new(),
            second_looking: Default::default(),
            second_due: Vec::new(),
            claude_pending: Default::default(),
            triage_flag: false,
            placement_flag: false,
            asks: Default::default(),
            trial: None,
            backlog_items: 0,
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
        .with_guard(guard)
        .with_data_dir(dir))
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
    pub(super) async fn record(
        &mut self,
        items: Vec<Item>,
        paused: bool,
        cant_start: Option<&'static str>,
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
                        branch: items
                            .iter()
                            .find(|i| &i.id == item)
                            .map(|i| branch_name(i, self.attempt_of(item))),
                        playing: self.playing.contains_key(&lane),
                        item_id: item.clone(),
                    })
                }
                _ => None,
            })
            .collect();
        // Behind the `live-sessions-reporting` release flag: Off keeps `sessions` at 0.
        let sessions = if self.flags.on("live-sessions-reporting").await {
            self.live.sessions().snapshot().len()
        } else {
            0
        };
        let payload = tick_payload(paused, pool, sessions);
        let would: Vec<String> = actions.iter().map(super::start_gate::describe).collect();
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
        let held_items = self.held_items(&items, &held);
        self.live.ticked(&self.id, paused, now_iso8601(), held, decision);
        self.live.set_items(&self.id, held_items);
        let mut payload = payload;
        if let Some(ci) = self.live.snapshot().ci {
            payload.extra.insert("ci".into(), ci.tick_json());
        }
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
        let anomaly_rules = self.flags.on("watchdog-anomaly-rules").await;
        let cant_start = cant_start.filter(|_| anomaly_rules);
        self.check_watchdog(paused, cant_start, &actions, pool, anomaly_rules);
        Step::Ticked { would }
    }

    /// Why no new work can start on this computer right now, for the watchdog, by the same rules `claim` uses:
    /// a claim pause; with the start rules Off, Claude signed out or the weekly cutoff; with them On, no lane
    /// whose engine (after fall-over) may start.
    pub(super) async fn start_blocked(&mut self, shared: &Shared<'_>, pool: &LanePool) -> Option<&'static str> {
        if shared.claim_paused_until.is_some_and(|u| std::time::Instant::now() < u) {
            return Some("claim paused");
        }
        let gate = StartGate::new(self.flags.on("engine-start-rules").await, self, pool, shared);
        if Self::claim_held_by_sign_out(shared, gate.as_ref()) {
            return Some("Claude signed out");
        }
        let Some(gate) = gate else {
            let week = self.flags.on("weekly-cutoff").await
                && shared.engine_limits.week_cut_off(std::time::Instant::now())
                && !Self::limit_fallback(shared);
            return week.then_some("weekly limit");
        };
        let lanes = pool.rules().lanes;
        (lanes > 0 && !(1..=lanes).any(|k| gate.lane_may_start(self, shared, k))).then_some("usage limit")
    }

    /// This minute's watchdog rules (docs/specs/agent-telemetry.md section 4): finds what's quietly gone
    /// wrong against the state this tick already has (`actions`, `pool`) and times it against findings
    /// from minutes before.
    fn check_watchdog(
        &mut self,
        paused: bool,
        cant_start: Option<&str>,
        actions: &[Action],
        pool: &LanePool,
        anomaly_rules: bool,
    ) {
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
            cant_start,
            runs: &self.runs,
            leases: &self.leases,
            records: &self.records,
            running: &self.running,
            pool,
            actions,
            recent: &recent,
            now_ms,
            anomaly_rules,
            // Behind `watchdog-anomaly-rules`: Off, no restart reaches the rules.
            restart: anomaly_rules
                .then(|| watchdog::RestartView::of(crate::restart::state()))
                .flatten(),
            lease_idle_minutes: self.project.tuning.lease_idle_minutes,
            idle_lanes_minutes: self.project.tuning.idle_lanes_minutes,
            no_progress_minutes: self.project.tuning.no_progress_minutes,
        });
        self.telemetry.minute(&self.id, found, now_ms, &mut self.events);
        self.live.set_anomalies(&self.id, self.telemetry.open_anomalies());
    }

    /// Writes the held and the unreleased leases to the state file.
    pub(super) fn save_leases(&mut self) {
        self.store.set_leases(&self.leases);
        self.store.set_unreleased(&self.unreleased);
        self.store.set_unskipped(&self.unskipped);
        self.store.set_unanswered(&self.unanswered);
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
    /// holds no lease and isn't here: the store hands it back when it is answered. With `give_back_on` (the
    /// `cursor-give-back` flag) and this computer's name, a needs_human item held only for Cursor's limit with
    /// no pull request isn't taken back: its record is dropped and its lease moved to `unreleased` with the
    /// give-back note, for `flush_owed` to send.
    pub(super) fn readopt(&mut self, items: &[Item], pool: &mut LanePool, give_back_on: Option<&str>) {
        if std::mem::replace(&mut self.adopted, true) || self.leases.is_empty() {
            return;
        }
        for (id, token) in std::mem::take(&mut self.leases) {
            let record = self.records.remove(&id);
            let Some(item) = items.iter().find(|i| i.id == id) else {
                tracing::info!("{}: no longer holds {id}, dropping its lease", self.id);
                continue;
            };
            if let Some(machine) = give_back_on
                && item.pr_url.is_none()
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
                self.unreleased.insert(id, (token, why));
                continue;
            }
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
            if let Some(saved) = self.records.get_mut(&id) {
                saved.silence_kills = record.silence_kills;
                saved.cursor_limit_hits = record.cursor_limit_hits;
                saved.not_started = record.not_started;
                if record.not_started {
                    self.fresh_launched.insert(id.clone());
                }
            }
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
        // The triage facts of the stop carry over; only the run's own fields are rewritten.
        let record = RunRecord {
            state: state_name(run.state).into(),
            lane,
            impl_session_id: session,
            impl_session_engine: engine,
            branch: Some(branch_name(item, attempt)),
            slice: run.slice,
            impl_context_tokens: context_tokens,
            attempt,
            ..self.records.get(item_id).cloned().unwrap_or_default()
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
        let kind_fresh = matches!(kind, Launch::Fresh);
        let mut notes = None;
        let mut on_engine = None;
        let (resume, follow_up) = match kind {
            Launch::Fresh => (None, None),
            Launch::Resume(session_id) => (Some(session_id), None),
            Launch::Continuing(follow_up) => (None, Some(follow_up)),
            Launch::ContinuingOn(follow_up, to) => {
                on_engine = Some(to);
                (None, Some(follow_up))
            }
            Launch::StartOver(text) => {
                notes = text;
                (None, None)
            }
        };
        if kind_fresh {
            self.fresh_launched.insert(item_id.to_string());
        } else {
            self.fresh_launched.remove(item_id);
        }
        if let Some(record) = self.records.get_mut(item_id) {
            record.not_started = false;
        }
        let session_id = resume.clone().unwrap_or_else(new_session_id);
        // A resumed session stays on its engine; a new one starts on the lane's.
        let engine = match &resume {
            Some(_) => self
                .records
                .get(item_id)
                .map(|r| r.impl_session_engine)
                .unwrap_or_default(),
            None if let Some(to) = on_engine => to,
            None => match StartGate::new(self.flags.on("engine-start-rules").await, self, pool, shared) {
                Some(gate) => gate.engine_now(self, shared, lane),
                None => program_of(engine_for(
                    &shared.links.engine_choice,
                    self.project.engines.as_ref(),
                    Role::Implementer,
                    Some(lane),
                )),
            },
        };
        // A resumed session keeps its last known size; a fresh or picking-up one starts with none of its own yet.
        let context_tokens = resume
            .is_some()
            .then(|| self.records.get(item_id).and_then(|r| r.impl_context_tokens))
            .flatten();
        self.remember(item_id, Some(lane), Some((session_id.clone(), engine)), context_tokens);
        self.save_leases();
        let fall_over = self.flags.on("engine-fall-over").await;
        let takeover = self.takeover_unless_local(pool, shared, (item_id, &session_id, engine), fall_over);
        let request = StartRequest {
            project: self.id.clone(),
            item_id: item_id.to_string(),
            lane,
            token: self.leases.get(item_id).cloned().unwrap_or_default(),
            item,
            session_id,
            engine,
            placement: self.flags.on("engine-placement").await,
            resume: resume.is_some(),
            outcomes: self.outcomes.clone(),
            usage: self.store.usage(),
            follow_up,
            items: self.items.values().cloned().collect(),
            attempt: self.attempt_of(item_id),
            notes,
            takeover,
        };
        match starter.start(request) {
            Ok(()) => {
                self.events.add_in(
                    &self.id,
                    Event::SessionStart(event::SessionStart {
                        item: Some(item_id.to_string()),
                        lane: Some(lane),
                        role: Some("implementer".into()),
                        engine: Some(engine.to_string()),
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
        let gate = StartGate::new(self.flags.on("engine-start-rules").await, self, pool, shared);
        let blocked_from_starting = gate.is_none() && signed_out(shared) || shared.stopping;
        let caps_on = self.flags.on("model-concurrency-caps").await;
        let mut started = 0;
        let mut todo = actions.to_vec();
        // Starts the model cap held off a Claude lane are placed again on the lanes it leaves (a Cursor lane).
        for round in 0..2 {
            let mut capped = 0;
            for action in todo.iter().filter(|a| **a != Action::AskForWork) {
                if let Action::StartImplementer { lane, .. } = action {
                    if blocked_from_starting || gate.as_ref().is_some_and(|g| g.holds(self, shared, *lane)) {
                        continue;
                    }
                    if self.runs_claude(shared, gate.as_ref(), *lane) && self.model_full(starter, caps_on) {
                        capped += 1;
                        continue;
                    }
                }
                if !apply(&self.id, action, &mut self.runs, pool, &self.project, &mut self.events) {
                    continue;
                }
                if let Action::StartImplementer { item_id, lane } = action {
                    let on_claude = self.runs_claude(shared, gate.as_ref(), *lane);
                    let ok = self.launch(pool, item_id, *lane, Launch::Fresh, starter, shared).await;
                    started += u32::from(ok);
                    self.claude_starts += u32::from(ok && on_claude);
                }
            }
            if round == 1 || capped == 0 {
                break;
            }
            todo = self
                .decide_gated(pool, shared, gate.as_ref(), true)
                .into_iter()
                .filter(|a| matches!(a, Action::StartImplementer { .. }))
                .take(capped)
                .collect();
        }
        started
    }
}
