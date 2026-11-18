//! One project's orchestrator state and loop: the mode switch, the per-pass decision, and claiming and
//! starting work for that project alone. The lanes and the sessions across every project are the
//! [`super::Machine`]'s.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::report;
use super::start_gate::StartGate;
use super::{LanePlay, Shared};

use crate::backlog::{BacklogError, Client, Item};
use crate::config::ProjectConfig;
use crate::engines::EngineId;
use crate::implementer::new_session_id;
use crate::item_lifecycle::{ItemRun, ItemState};
use crate::lane_pool::{Holder, LanePool};
use crate::orchestrator::guard::PortGuard;
use crate::orchestrator::live::{HeldLane, LiveHandle};
use crate::orchestrator::session::branch_name;
use crate::orchestrator::steps::{Running, StepDone, StepRequest, state_name};
use crate::orchestrator::{Action, ProjectRuns, apply, tick_payload};
use crate::store::{RunRecord, Store};
use crate::telemetry::events::parse_iso_ms;
use crate::telemetry::{Event, EventLog, Telemetry, event, now_ms, watchdog};
use crate::text::now_iso8601;
use crate::view::ItemCommands;

pub use super::mode::{MODE_VAR, OrchestratorMode};

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
    /// The engine that takes over from `engine` if it is at its usage limit when the code gates send findings
    /// back (engines/03 section 3.5): the session starts fresh on it. `None` when `engine` isn't limited or
    /// nothing else can take it.
    pub takeover: Option<EngineId>,
    /// The chore this item is (a chore task's local item): its prompt and kind, for a fresh session's brief.
    pub chore: Option<crate::chore_items::ChoreWork>,
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
    /// Whether a fresh Claude implementer start may begin now: no when `sonnet`, the model a claim with no tier runs on, has a cap and no slot free (07.1). `pending` counts the Claude implementer starts already made this pass, which haven't taken their slot yet.
    fn implementer_may_start(&self, _pending: u32) -> bool {
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
    fn advance(&self, request: StepRequest) -> Option<BoxFuture<'static, StepDone>>;
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
    /// The chore items running here (`chore_items`): no lease, no store item, kept in the state file.
    pub(super) chores: BTreeMap<String, crate::chore_items::ChoreWork>,
    /// This computer takes the store's tasks (it has a session runner and reports `health`).
    pub(super) takes_tasks: bool,
    /// What this computer holds of the base branch: what the fetch saw and the results of the store's `main` looks (`main_facts.rs`).
    pub(super) main_look: crate::main_look::MainLook,
    /// The base branch has `.agent-ci/`, as the last fetch showed it (`agentCiOf`): laid on `project` (`ProjectConfig::agent_ci`).
    pub(super) agent_ci: bool,
    /// Leases given back here that the store hasn't taken yet: item id to (lease token, the give-back's
    /// note), kept in the state file and sent again, `report` then `release`, on later passes (the TS
    /// agent's `unreleased`).
    pub(super) unreleased: BTreeMap<String, (String, String)>,
    /// When each `unreleased` entry was first listed (epoch seconds), kept in the state file; a release that
    /// keeps failing is dropped after [`super::settle::STUCK_RELEASE_SECS`].
    pub(super) unreleased_since: BTreeMap<String, i64>,
    /// The test's clock, as `Machine::clock` is.
    #[cfg(test)]
    pub(super) clock: Option<i64>,
    /// Items a person skipped whose `skip` the store hasn't taken yet; sent once their release has landed, and
    /// again on later passes until it answers (the TS agent's `unreleased.skip`). Kept in memory only: a restart forgets it and the store still lists the item for a person to skip again.
    pub(super) unskipped: std::collections::BTreeSet<String>,
    /// What the store was last told of each leased item's progress (`flow.rs`).
    pub(super) told: super::flow::Told,
    /// Playtest answers for the store's try-out queue not yet taken (id to passed and notes); see `playtest_owed.rs`. In memory only.
    pub(super) unanswered: BTreeMap<String, (bool, Option<String>)>,
    /// The `playtests` value last seen and whether a `release_playtests` is owed; see `playtest_owed.rs`. In memory only.
    pub(super) playtests_seen: Option<String>,
    pub(super) release_owed: bool,
    /// Leases let go while their item waits, not yet taken by the store: item id to (lease token, what it
    /// waits for, the item's state). Kept in the state file and sent again on later passes.
    pub(super) unparked: BTreeMap<String, (String, String, String)>,
    /// Items parked while they wait on the host or a person, kept in the state file with their records (`parking.rs`): item id to
    /// (what it waits for, the state it was parked in). The run stays here, the lease is the store's until the wait ends.
    pub(super) parked: BTreeMap<String, (String, String)>,
    /// The store's merge turn for an item (`item_merge.rs`).
    pub(super) turns: super::item_merge::Turns,
    /// Completions the store hasn't taken yet, by item id, kept in the state file and sent again on later passes (the TS
    /// agent's `ItemRecord.outbox`); see `outbox.rs`.
    pub(super) outbox: BTreeMap<String, crate::backlog::complete::Pending>,
    /// When each stuck `outbox` entry last raised its `error` event (epoch seconds), so one comes per day, not
    /// per pass. Memory only: a restart may say it once more.
    pub(super) outbox_noted: BTreeMap<String, i64>,
    /// What each leased item is doing (state, lane, session id, branch, slice), kept in the state file so a
    /// restart takes it back as it was. Only items in `leases` have one: it goes when the lease does.
    pub(super) records: BTreeMap<String, RunRecord>,
    /// Each leased item's decision facts, read from its store item and sent back as they change
    /// (`decisions.rs`). An item whose facts couldn't be read has none.
    pub(super) decisions: BTreeMap<String, crate::backlog::decisions::sync::ItemDecisions>,
    /// Items taken back mid-session by the first pass, whose sessions are resumed on the next schedule.
    pub(super) to_resume: Vec<String>,
    /// The computer's data folder, where session transcripts are kept (none in a loop built without one).
    pub(super) data_dir: Option<PathBuf>,
    /// Whether the saved `state.usage` has been checked once against the weekly cutoff.
    pub(super) week_checked: bool,
    /// Items whose session the watchdog killed, by when it is resumed.
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
    pub(super) live: LiveHandle,
    /// GitHub's numeric id of `project.github_repo`, asked of `gh` until it answers and then kept.
    pub(super) repo: Option<u64>,
    /// The steps running for this project's items.
    pub(super) running: Running,
    /// The steps in `running`, by item, for their `step_end`.
    pub(super) open_steps: std::collections::HashMap<String, super::step_events::OpenStep>,
    /// What the fetch saw that the store was told last (`report`).
    pub(super) seen: report::Seen,
    /// The watchdog's debounce state (docs/specs/agent-telemetry.md section 4): how long each rule's
    /// finding has held, across minutes.
    pub(super) telemetry: Telemetry,
    /// When this loop began, i.e. this run of the agent (epoch ms); the watchdog's `duplicate_step` ignores earlier starts.
    run_started_ms: i64,
    /// The run loop's state (`runs.rs`), used only by [`ProjectLoop::run_pass`].
    pub(super) run_loop: super::RunLoop,
    /// The pool's tasks and the ends owed to the store (`pool.rs`).
    pub(super) pool: super::pool::Pool,
    /// The jobs the store sent as tasks (`job_tasks.rs`).
    pub(super) jobs: super::job_tasks::Jobs,
    /// The tag list's generation when this project last asked, so a re-read ends a wait (`tags-reread-on-save`).
    pub(super) tags_seen: u64,
    /// A session this size or smaller still resumes across a restart; a bigger one starts fresh instead
    /// (06.2-implementer.md "A session that's grown starts fresh", `MachineConfig::max_resume_context_tokens`).
    pub(super) max_resume_context_tokens: u32,
    /// Continues waiting for a free lane (`item_commands.rs`).
    pub(super) continuing: Vec<super::item_commands::PendingContinue>,
    /// Failed playtest answers waiting for a lane: the item and the message to resume the implementer with.
    pub(super) playtest_fails: Vec<super::item_commands::PendingContinue>,
    /// A person's answers to an item's questions waiting for a lane (`answers.rs`). In memory only.
    pub(super) answering: Vec<super::answers::PendingAnswer>,
    /// Items sent back from a state with no lane, waiting for one to start their implementer on (`lane_resumes.rs`).
    pub(super) lane_resumes: Vec<String>,
    /// Start overs waiting for a free lane (`item_commands.rs`).
    pub(super) starting_over: Vec<super::item_commands::PendingContinue>,
    /// The lane each item last held here, so a Continue goes back to its worktree when it's free.
    pub(super) last_lane: BTreeMap<String, u32>,
    /// The games running in lanes (`play { lane }`), by lane, with the item each lane was held for.
    pub(super) playing: BTreeMap<u32, (String, Box<dyn LanePlay>)>,
    /// Lanes whose game was stopped for a restart, with when: played again once the lane is given back (`lane_play.rs`).
    pub(super) restarting: BTreeMap<u32, std::time::Instant>,
    /// A build played from the play checkout, and the one waiting for it (`build_play.rs`).
    pub(super) build_play: super::build_play::BuildPlay,
    /// The spec draft running now, if any (`drafts.rs`): its ids only, in memory.
    pub(super) draft: Option<super::drafts::RunningDraft>,
    /// Store failures of the draft and ask tasks, written to the event log by [`Self::drain_failures`].
    pub(super) failures: crate::backlog::failures::Failures,
    /// Approve and Merge presses being carried out, and the items of drafts merged since the store last took headers.
    pub(super) landing: super::land::Landing,
    /// Final draft reports the store hasn't taken, in a file of the project's own (`draft_owed.rs`).
    pub(super) drafts_owed: super::draft_owed::Kept,
    /// The leases held on plans that wait for a person, and the approved ones the next land pass carries out (`held_drafts.rs`).
    pub(super) held: super::held_drafts::Held,
    /// Items a plan asks a person to check first, by id, with why (`plan_checking.json` in the project's folder): kept from the plan's
    /// report until the Items headers that add them are sent.
    pub(super) plan_checking: crate::owed_file::OwedFile<String>,
    /// The items each plan adds, by plan id, until the store holds them once it merges (`plan_items.rs`).
    pub(super) plan_items: super::plan_items::Kept,
    /// The ones kept from before the start that are yet to be sent again, and the tasks sending them.
    pub(super) owed_resume: Vec<String>,
    pub(super) owed_sends: Vec<tokio::task::JoinHandle<()>>,
    /// The wait between sends of those.
    pub(super) owed_wait: std::time::Duration,
    /// The stopped items a triager is looking into or about to (`claudeOnIt`), in memory only.
    pub(super) triaging: std::collections::BTreeSet<String>,
    /// Items whose entry in `continuing` or `starting_over` is Claude's decision, not a person's (06.9 bullet 17).
    pub(super) claude_pending: std::collections::BTreeSet<String>,
    /// The chat's asks, waiting and running (`asks.rs`).
    pub(super) asks: super::asks::Asks,
    /// Items taken over from another computer, and the rows asking for it (`take_overs.rs`).
    pub(super) take_overs: super::take_overs::Pulling,
    /// When items held here with no lease are next claimed back (`own_items.rs`).
    pub(super) own_items: super::own_items::Look,
    pub(super) finishing: super::finish_steps::Finishing,
    /// A person's Merge into main on an item (`merge_item.rs`).
    pub(super) merging: super::merge_item::Merging,
    /// A person's Open PR to main (`main_pr.rs`).
    pub(super) opening: super::main_pr::Opening,
    /// A person's Sync now (`sync_now.rs`).
    pub(super) syncing: super::sync_now::Syncing,
    /// The expert sessions (`experts.rs`).
    pub(super) experts: super::experts::Experts,
    /// The CI watcher's pass (`ci_pass.rs`).
    pub(super) ci_watch: super::ci_pass::State,
    /// The Fix button's rows (`fix_requests.rs`).
    pub(super) fixes: super::fix_requests::FixRows,
    /// The site's Add spec and Build this rows.
    pub(super) wanted: super::draft_wanted::WantedRows,
    /// How many items the backlog held when this project was last read; `None` before a read has gone through, so
    /// nothing that needs a read backlog (the CI watcher) runs on an unread one.
    pub(super) backlog_items: Option<usize>,
    /// The branches of every item and piece the store listed when this project was last read, whoever holds them (in
    /// `runs` mode none is held here): the CI watcher leaves their pull requests alone.
    pub(super) listed_branches: std::collections::HashSet<String>,
    /// The project as config.json has it; `project` is this with the allowed commands over it (`command_gate.rs`).
    pub(super) file_project: ProjectConfig,
    /// The project's settings values as the last good `settings_get` had them (null before one): the issue import reads
    /// its `issues` setting from them.
    pub(super) settings_values: Value,
    /// The settings the store holds a default for, as the last good `settings_get` had them (`None`: none yet, or it sent none): what
    /// the project runs on is only these (`store_effective.rs`), so without them nothing starts.
    pub(super) effective: Option<crate::store_effective::StoreEffective>,
    /// What the store is told of the settings this computer runs (`settings_report.rs`).
    pub(super) settings_report: super::settings_report::SettingsReport,
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
        let file_project = project.clone();
        crate::program_hold::restore(&project.id_or_derived(), store.program_holds());
        let revisions = store.revise_owed();
        let ship_owed = store.ship_owed();
        let drafts_owed = super::draft_owed::open(store.dir());
        let plan_checking = crate::owed_file::OwedFile::open(store.dir().join("plan_checking.json"));
        Self {
            owed_resume: drafts_owed.all().into_keys().collect(),
            owed_sends: Vec::new(),
            owed_wait: super::draft_owed::RETRY,
            drafts_owed,
            held: super::held_drafts::Held::open(store.dir()),
            plan_checking,
            plan_items: super::plan_items::open(store.dir()),
            run_loop: super::RunLoop::restoring(&id, store.run_ends_owed(), crate::telemetry::now_secs()),
            pool: super::pool::Pool::restoring(&store),
            settings_values: Value::Null,
            effective: crate::store_effective::StoreEffective::before_the_first_answer(),
            jobs: Default::default(),
            tags_seen: 0,
            id,
            project,
            leases: store.leases(),
            chores: store.chores(),
            takes_tasks: false,
            main_look: Default::default(),
            agent_ci: false,
            unreleased: store.unreleased(),
            unreleased_since: store.unreleased_since(crate::telemetry::now_ms() / 1000),
            #[cfg(test)]
            clock: None,
            unskipped: Default::default(),
            told: Default::default(),
            unanswered: Default::default(),
            playtests_seen: None,
            release_owed: false,
            unparked: store.unparked(),
            parked: store.parked(),
            turns: Default::default(),
            outbox: store.complete_outbox(),
            outbox_noted: BTreeMap::new(),
            records: store.runs(),
            decisions: BTreeMap::new(),
            to_resume: Vec::new(),
            data_dir: None,
            week_checked: false,
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
            live,
            repo: None,
            running: Running::default(),
            open_steps: Default::default(),
            seen: report::Seen::default(),
            telemetry: Telemetry::new(),
            run_started_ms: now_ms(),
            max_resume_context_tokens,
            continuing: Vec::new(),
            playtest_fails: Vec::new(),
            answering: Vec::new(),
            lane_resumes: Vec::new(),
            starting_over: Vec::new(),
            last_lane: BTreeMap::new(),
            playing: BTreeMap::new(),
            restarting: BTreeMap::new(),
            build_play: Default::default(),
            draft: None,
            failures: Default::default(),
            landing: super::land::Landing::with_revisions(revisions),
            triaging: Default::default(),
            claude_pending: Default::default(),
            asks: Default::default(),
            take_overs: Default::default(),
            own_items: Default::default(),
            finishing: Default::default(),
            merging: Default::default(),
            opening: Default::default(),
            ci_watch: Default::default(),
            fixes: Default::default(),
            wanted: Default::default(),
            syncing: Default::default(),
            experts: super::experts::Experts::with_owed(ship_owed),
            backlog_items: None,
            listed_branches: Default::default(),
            file_project,
            settings_report: Default::default(),
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

    /// Takes this pass's `settings_get` (`None`: not asked; an error changes nothing, so the allowed commands keep
    /// running) into the project's allowed commands, then lays them over `file_project` as `project`.
    pub(super) fn take_allowed_commands(
        &mut self,
        commands: &ItemCommands,
        settings: Option<Result<crate::backlog::settings::SettingsAnswer, BacklogError>>,
    ) {
        let gates = commands.gates();
        if let Some(dir) = &self.data_dir {
            gates.open(dir, &self.id);
        }
        match settings {
            Some(Ok(answer)) => {
                let settings = answer.record;
                self.settings_values = settings.as_ref().map_or(Value::Null, |s| s.values.clone());
                // The defaults and numbers come from `effective`, whether or not a record has been saved. An answer with none (an older
                // store) says nothing new: what was read stays, and before any was read there is none, so nothing starts.
                if let Some(effective) = answer.effective {
                    self.effective = Some(effective);
                }
                self.settings_report.applied = settings.as_ref().map(|s| s.version);
                if let Err(e) = gates.consider(&self.id, settings.as_ref(), &self.file_project) {
                    tracing::warn!("{}: saving the allowed commands: {e:#}", self.id);
                }
            }
            Some(Err(e)) => self.store_error("settings_get", &e),
            None => {}
        }
        self.project = commands.allowed_config(&self.id, &self.file_project);
        // Checks the store has that nothing approved here runs: the checking step waits for the approval.
        self.project.checks_unapproved =
            self.project.checks.is_empty() && self.settings_values["checks"].as_array().is_some_and(|c| !c.is_empty());
        self.lay_effective();
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
        let sessions = self.live.sessions().snapshot().len();
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
        self.check_watchdog(paused, cant_start, &actions, pool);
        Step::Ticked { would }
    }

    /// Why no new work can start on this computer right now, for the watchdog, by the same rules `claim` uses:
    /// a claim pause, or no lane whose engine (after fall-over) may start.
    pub(super) fn start_blocked(&self, shared: &Shared<'_>, pool: &LanePool) -> Option<&'static str> {
        if shared.own_failure.is_some() {
            return Some("claim paused");
        }
        let gate = StartGate::new(self, pool, shared);
        let lanes = pool.rules().lanes;
        (lanes > 0 && !(1..=lanes).any(|k| gate.lane_may_start(self, shared, k))).then_some("usage limit")
    }

    /// This minute's watchdog rules (docs/specs/agent-telemetry.md section 4): finds what's quietly gone
    /// wrong against the state this tick already has (`actions`, `pool`) and times it against findings
    /// from minutes before.
    fn check_watchdog(&mut self, paused: bool, cant_start: Option<&str>, actions: &[Action], pool: &LanePool) {
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
            run_started_ms: self.run_started_ms,
            restart: watchdog::RestartView::of(crate::restart::state()),
            lease_idle_minutes: self.project.tuning.lease_idle_minutes,
            idle_lanes_minutes: self.project.tuning.idle_lanes_minutes,
            no_progress_minutes: self.project.tuning.no_progress_minutes,
        });
        self.telemetry.minute(&self.id, found, now_ms, &mut self.events);
        self.live.set_anomalies(&self.id, self.telemetry.open_anomalies());
    }

    /// Writes the held and the unreleased leases, and the completions owed, to the state file.
    pub(super) fn save_leases(&mut self) {
        self.store.set_leases(&self.leases);
        self.store.set_unreleased(&self.unreleased);
        self.unreleased_since.retain(|id, _| self.unreleased.contains_key(id));
        self.store.set_unreleased_since(&self.unreleased_since);
        self.store.set_unparked(&self.unparked);
        self.store.set_parked(&self.parked);
        self.store.set_complete_outbox(&self.outbox);
        self.records.retain(|id, _| {
            self.leases.contains_key(id) || self.chores.contains_key(id) || self.parked.contains_key(id)
        });
        self.store.set_chores(&self.chores);
        self.store.set_runs(&self.records);
        let holds = crate::program_hold::snapshot(&self.project.id_or_derived(), |item| {
            self.leases.contains_key(item) || self.chores.contains_key(item) || self.parked.contains_key(item)
        });
        self.store.set_program_holds(&holds);
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
        self.forget_progress(&item.id);
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

    /// Writes the item's run into its record: its state now, `lane`, its branch and slice, `session` and
    /// `context_tokens` (the implementer session's id and its last known size). Does nothing for an item with
    /// no lease. Call `save_leases` after.
    /// How many times a person started the item over: 0 until then. The store counts attempts from 1 (an absent
    /// `attempt` is the first), as the TS agent does, so the store's value is one more than this. 0 too while the
    /// item's facts haven't been read: whatever names a branch from it waits for them first.
    pub(super) fn attempt_of(&self, item_id: &str) -> u32 {
        self.decisions_of(item_id)
            .and_then(|d| d.attempt)
            .map_or(0, |n| n.saturating_sub(1))
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
        if !self.holds(item_id) {
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
            ..self.records.get(item_id).cloned().unwrap_or_default()
        };
        self.records.insert(item_id.to_string(), record);
    }

    /// Refreshes the saved run of an item whose state just moved. A run that was parked or given back has no
    /// lease or record now; the rest are saved as they stand.
    pub(super) fn refresh_record(&mut self, pool: &LanePool, item_id: &str) {
        if self.records.contains_key(item_id) && self.holds(item_id) {
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
        if !self.ensure_decisions(item_id, shared).await {
            let why = "the session didn't start: the item's facts haven't been read from the shared backlog yet";
            self.hand_back(pool, item_id, "queued", why, shared).await;
            return false;
        }
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
        // A send-back from a step (checks, review, CI) rides the resume; a fresh session's prompt carries it.
        let sent_back = self.records.get_mut(item_id).and_then(|r| r.flow.follow_up.take());
        let follow_up = if resume.is_some() {
            sent_back.or(follow_up)
        } else {
            follow_up
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
            None => StartGate::new(self, pool, shared).engine_now(self, shared, lane),
        };
        // A resumed session keeps its last known size; a fresh or picking-up one starts with none of its own yet.
        let context_tokens = resume
            .is_some()
            .then(|| self.records.get(item_id).and_then(|r| r.impl_context_tokens))
            .flatten();
        self.remember(item_id, Some(lane), Some((session_id.clone(), engine)), context_tokens);
        self.save_leases();
        self.report_progress(shared.links, item_id).await;
        let takeover = self.takeover_unless_local(pool, shared, (item_id, &session_id, engine));
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
            takeover,
            chore: self.chores.get(item_id).cloned(),
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
                // Every start takes a slot: a "do next" item is first in the store's order, not outside the slots.
                true
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
        let gate = StartGate::new(self, pool, shared);
        let mut started = 0;
        let mut todo = actions.to_vec();
        // Starts the model cap held off a Claude lane are placed again on the lanes it leaves (a Cursor lane).
        for round in 0..2 {
            let mut capped = 0;
            for action in todo.iter().filter(|a| **a != Action::AskForWork) {
                if let Action::StartImplementer { lane, .. } = action {
                    if shared.stopping || gate.holds(self, shared, *lane) {
                        continue;
                    }
                    if self.runs_claude(shared, &gate, *lane) && self.model_full(starter) {
                        capped += 1;
                        continue;
                    }
                }
                if !apply(&self.id, action, &mut self.runs, pool, &self.project, &mut self.events) {
                    continue;
                }
                if let Action::StartImplementer { item_id, lane } = action {
                    let on_claude = self.runs_claude(shared, &gate, *lane);
                    let ok = self.launch(pool, item_id, *lane, Launch::Fresh, starter, shared).await;
                    started += u32::from(ok);
                    self.claude_starts += u32::from(ok && on_claude);
                }
            }
            if round == 1 || capped == 0 {
                break;
            }
            todo = self
                .decide_gated(pool, shared, &gate, true)
                .into_iter()
                .filter(|a| matches!(a, Action::StartImplementer { .. }))
                .take(capped)
                .collect();
        }
        started
    }
}
