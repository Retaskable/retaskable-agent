//! The loop that owns the computer's orchestrator state (one shared lane pool, each project's state) and calls `orchestrator::decide` on a clock
//! (`Pool.tick` in Retaskable.com's `agent/src/pool.ts`), behind a safety switch,
//! `RETASKABLE_ORCHESTRATOR`. It wakes every `pollIntervalSec` (the TS agent's 10 second `step()`); once a
//! minute that wake is a full pass (`minute()`): a `git fetch` of each live project's repo, a read of the
//! project's items from the shared backlog, a decision (`decide` is pure) and a `tick` event saying what it
//! would do or did. In `dry` mode, or with no [`SessionStarter`], that's all: it never starts a session,
//! takes a lane, asks the store for work or writes to it. In `claim` mode with a starter, every wake also
//! takes in the sessions and steps that ended (`settle`: freeing, parking or releasing their lanes and
//! leases), and, unless paused, advances every non-terminal item one step (one at a time per item, see
//! `steps`) and, while `concurrency` session slots and a lane are free, starts queued items and asks the
//! store for more (`claim_next`). A store that can't be reached, or answers something unreadable, is a
//! `store_error` event and a skipped tick, never an empty backlog (docs/specs/agent/02-principles.md: fail closed).
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use super::guard::PortGuard;
use super::live::{HeldLane, LiveHandle, now_iso8601};
use super::round_robin::{Candidate, plan};
use super::steps::{Running, StepRequest};
use super::{Action, ProjectRuns, apply, decide, tick_payload};
use crate::backlog::{BacklogError, ClaimNextAgent, ClaimNextAnswer, Client, Item};
use crate::config::ProjectConfig;
use crate::env::Env;
use crate::item_lifecycle::{ItemRun, ItemState};
use crate::lane_pool::{Holder, LanePool, PoolRules};
use crate::lanes;
use crate::link::TokenStore;
use crate::store::{RunRecord, Store};
use crate::telemetry::{Event, EventLog, event};
use crate::view::PauseState;

/// The variable that switches the orchestrator on.
pub const MODE_VAR: &str = "RETASKABLE_ORCHESTRATOR";

/// How often the loop reads every project's backlog and writes its `tick` events: the TS agent's once a
/// minute (`MINUTE_MS` in `agent/src/pool.ts`).
pub const EVERY: Duration = Duration::from_secs(60);

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
}

impl OrchestratorMode {
    /// The mode `RETASKABLE_ORCHESTRATOR` names (`off`, `dry` or `claim`, any case), and a warning
    /// for a value that is none of them, which counts as `off`.
    pub fn from_env(env: &Env) -> (Self, Option<String>) {
        let Some(raw) = env.get(MODE_VAR) else {
            return (Self::Off, None);
        };
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => (Self::Off, None),
            "dry" => (Self::Dry, None),
            "claim" => (Self::Claim, None),
            other => (
                Self::Off,
                Some(format!(
                    "{MODE_VAR}={other:?} isn't off, dry or claim, so the orchestrator stays off."
                )),
            ),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Dry => "dry",
            Self::Claim => "claim",
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

/// The mode the loops really run in. `claim` with another agent on this computer runs dry instead,
/// logged loudly, so two agents never claim from one backlog.
pub async fn effective_mode(mode: OrchestratorMode, guard: &PortGuard) -> OrchestratorMode {
    if mode != OrchestratorMode::Claim {
        return mode;
    }
    match guard.refusal().await {
        Some(why) => {
            tracing::error!("orchestrator: claim refused, running dry instead: {why}");
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
    /// Whether to resume `session_id` (a restart cut it off) rather than start it.
    pub resume: bool,
    /// Where the session reports how it ended; the loop reads it on its next wake.
    pub outcomes: OutcomeSender,
}

mod report;
mod restart;
mod settle;
pub use settle::{OutcomeSender, SessionOutcome, SessionResult};

/// Runs the sessions and steps for the work the loop schedules. Built in a later piece (implementer-session and
/// the review stages); until one is given to the [`Machine`], `claim` mode claims nothing, so no claimed item is
/// left with nothing working on it.
pub trait SessionStarter: Send + Sync {
    /// Called once the item is `implementing` and its lane held. Must not block: start the session and return;
    /// it reports its end through `request.outcomes`. An `Err` (the session was refused or couldn't start)
    /// gives the lane and the claim straight back.
    fn start(&self, request: StartRequest) -> Result<(), String>;
    /// Sessions running now, on every project: the pseudocode's `activeSessions`, counted against `concurrency`.
    fn active(&self) -> u32;
    /// The item's next step for its state (`steps::has_step`, except `implementing`, whose step is its session),
    /// run as its own task: the run as the step left it. `None` when no step is built for that state yet: the item
    /// waits where it is, and the loop asks again on its next wake.
    /// The loop asks again only once it ends. A step that leaves the item in a state that runs nothing in a lane
    /// (`steps::needs_lane`) has pushed its work first: the loop then frees the lane.
    fn advance(&self, request: StepRequest) -> Option<BoxFuture<'static, ItemRun>>;
}

/// One project's orchestrator state, kept for as long as the agent runs. The lanes are not here: they
/// are the computer's, held by the [`Machine`].
pub struct ProjectLoop {
    id: String,
    project: ProjectConfig,
    /// Loaded so a project whose state file can't be read doesn't loop; only `claim` mode writes it (the leases).
    store: Store,
    runs: ProjectRuns,
    /// Lease token of each item this computer holds, by id; kept in the state file so a restart still holds them.
    leases: BTreeMap<String, String>,
    /// Leases given back here that the store hasn't taken yet (item id to token), kept in the state file
    /// and sent again on later passes (the TS agent's `unreleased`).
    unreleased: BTreeMap<String, String>,
    /// Leases let go while their item waits, not yet taken by the store: item id to (lease token, what it
    /// waits for, the item's state). Kept in the state file and sent again on later passes.
    unparked: BTreeMap<String, (String, String, String)>,
    /// What each leased item is doing (state, lane, session id, branch, slice), kept in the state file so a
    /// restart takes it back as it was. Only items in `leases` have one: it goes when the lease does.
    records: BTreeMap<String, RunRecord>,
    /// Items taken back mid-session by the first pass, whose sessions are resumed on the next schedule.
    to_resume: Vec<String>,
    /// The item the store handed out for each lease held, by id, for the session that starts on it.
    items: BTreeMap<String, Item>,
    /// Where this project's sessions report their end; replaced with the machine's by [`Machine::add`].
    outcomes: OutcomeSender,
    /// Whether the held leases were matched against the backlog yet (once, on the first pass).
    adopted: bool,
    mode: OrchestratorMode,
    /// Present in `claim` mode: asked again before each claim.
    guard: Option<PortGuard>,
    events: EventLog,
    client: Arc<Client>,
    live: LiveHandle,
    /// GitHub's numeric id of `project.github_repo`, asked of `gh` until it answers and then kept.
    repo: Option<u64>,
    /// The steps running for this project's items.
    running: Running,
    /// What the fetch saw that the store was told last (`report`).
    seen: report::Seen,
}

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
            running: Running::default(),
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
    fn record(
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

    /// Applies the starts decided to the real runs and pool, and calls the starter for each that took its
    /// lane. A start the starter refuses gives the lane and the claim straight back. How many sessions started
    /// that take a session slot.
    async fn apply_starts(
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

    /// Asks the store for work while a lane is free and a session slot is left (`slots`, used up as work
    /// starts), and starts what it hands out. The tick's `claim`, the last answer: the item id, `none` or
    /// `failed`; `None` when it wasn't asked (no slot or lane, the guard, or no link).
    async fn claim(
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
    fn cant_start(&self, message: String) {
        self.live.failed(&self.id, now_iso8601(), message);
    }

    fn store_error(&mut self, op: &str, e: &BacklogError) {
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

fn describe(action: &Action) -> String {
    match action {
        Action::StartImplementer { item_id, lane } => format!("start_implementer {item_id} in lane {lane}"),
        Action::NeedsHuman { item_id } => format!("needs_human {item_id}"),
        Action::AskForWork => "ask_for_work".into(),
    }
}

/// GitHub's numeric repo id (`gh api repos/<repo> --jq .id`, `repoId` in `agent/src/gh.ts`), which names the
/// project's backlog in the store.
async fn repo_id(github_repo: &str) -> Option<u64> {
    let out = tokio::process::Command::new("gh")
        .args(["api", &format!("repos/{github_repo}"), "--jq", ".id"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .ok()
        .filter(|&n| n > 0)
}

/// Why a project's items weren't read this pass.
enum Fail {
    /// Before the store could be asked (no repo id, no link): the page hears of it, no `store_error` event.
    Cant(String),
    Store(BacklogError),
}

/// A project's backlog read: the repo id it learned and the items or why not.
struct Read {
    repo: Option<u64>,
    items: Result<Vec<Item>, Fail>,
}

/// The inputs every pass shares.
pub struct Links {
    pub backlog_url: String,
    pub keychain: Arc<dyn TokenStore>,
    pub paused: PauseState,
    /// This computer's name and the host of the retaskable.com it links to, as `claim_next` is told them.
    pub machine: String,
    pub environment: String,
}

/// Reads one project's items. Owns what it needs, so it runs as its own task: a panic in it is that
/// project's failed read and nothing else's.
async fn read(
    client: Arc<Client>,
    id: String,
    github_repo: String,
    cached: Option<u64>,
    backlog_url: String,
    keychain: Arc<dyn TokenStore>,
) -> Read {
    let repo = match cached {
        Some(repo) => Some(repo),
        None => repo_id(&github_repo).await,
    };
    let Some(repo_id) = repo else {
        tracing::warn!("{id}: dry tick skipped: gh couldn't say the id of {github_repo}");
        let items = Err(Fail::Cant(format!("gh couldn't say the id of {github_repo}")));
        return Read { repo, items };
    };
    let token = match keychain.get(&backlog_url) {
        Ok(token) => token,
        Err(e) => {
            tracing::warn!("{id}: dry tick skipped: reading the backlog link: {e:#}");
            let items = Err(Fail::Cant(format!("reading the backlog link: {e:#}")));
            return Read { repo, items };
        }
    };
    let items = match token {
        None => Err(Fail::Store(BacklogError::BadAnswer {
            status: 401,
            code: Some("unauthorized".into()),
            message: "this computer holds no link to the backlog store".into(),
        })),
        Some(token) => client.list(repo_id, Some(&token)).await.map_err(Fail::Store),
    };
    Read { repo, items }
}

/// The computer's orchestrator: one shared lane pool and every project's state. Each pass reads every
/// project's backlog (in parallel, each in its own task), then the projects take turns deciding starts
/// on the shared lanes (`round_robin::plan`), as the TS agent's `Pool.tick` does.
pub struct Machine {
    pool: LanePool,
    projects: Vec<ProjectLoop>,
    /// Index in `projects` of the project served last: the next start goes to the one after it.
    last_served: Option<usize>,
    links: Links,
    /// Runs the sessions for what is claimed. With none, `claim` mode runs dry.
    starter: Option<Arc<dyn SessionStarter>>,
    /// How many sessions may run at once, across every project (`concurrency`).
    concurrency: u32,
    /// Sessions report their end here; the receiver is read on each wake.
    outcomes: mpsc::UnboundedReceiver<SessionOutcome>,
    outcome_sender: OutcomeSender,
}

impl Machine {
    pub fn new(lane_count: u32, concurrency: u32, links: Links) -> Self {
        let (sender, outcomes) = mpsc::unbounded_channel();
        Self {
            pool: LanePool::new(PoolRules {
                lanes: lane_count,
                ..Default::default()
            }),
            projects: Vec::new(),
            last_served: None,
            links,
            starter: None,
            concurrency,
            outcomes,
            outcome_sender: OutcomeSender(sender),
        }
    }

    /// The sender every started session gets (inside its [`StartRequest`]).
    pub fn outcome_sender(&self) -> OutcomeSender {
        self.outcome_sender.clone()
    }

    /// Whether a project's loop really acts: `claim` mode with a starter.
    fn live(&self, lp: &ProjectLoop) -> bool {
        self.starter.is_some() && lp.mode == OrchestratorMode::Claim
    }

    fn paused(&self) -> Vec<bool> {
        (self.projects.iter())
            .map(|lp| self.links.paused.reason(&lp.id).is_some())
            .collect()
    }

    /// The poll between the minute's passes (`pollIntervalSec`): the live projects take in their ended steps,
    /// advance their items and start what the free lanes and session slots allow, without reading the backlog
    /// or writing a `tick`. Does nothing with no starter. A panic costs this poll only.
    pub async fn poll(&mut self) {
        let ready: Vec<bool> = self.projects.iter().map(|lp| self.live(lp)).collect();
        if !ready.contains(&true) {
            return;
        }
        let paused = self.paused();
        if let Err(panic) = AssertUnwindSafe(self.schedule(&ready, &paused)).catch_unwind().await {
            tracing::error!("orchestrator: a poll panicked, skipping it: {}", panic_message(&*panic));
        }
    }

    /// The minute's `git fetch` of every live project's repo, paused or not (section 7: the fetch runs while
    /// paused too), each in parallel, and then what it saw told to the store (`report`). A failed fetch is
    /// logged and an `error` event; the loop goes on.
    pub async fn fetch(&mut self) {
        let jobs: Vec<_> = (self.projects.iter())
            .filter(|lp| self.live(lp))
            .map(|lp| {
                let repo = std::path::PathBuf::from(&lp.project.repo_path);
                (
                    lp.id.clone(),
                    tokio::task::spawn_blocking(move || lanes::fetch_latest(&repo)),
                )
            })
            .collect();
        for (id, job) in jobs {
            let Some(lp) = self.projects.iter_mut().find(|lp| lp.id == id) else {
                continue;
            };
            let failed = match job.await {
                Ok(Ok(_)) => {
                    lp.report(&self.links).await;
                    continue;
                }
                Ok(Err(e)) => format!("{e:#}"),
                Err(e) => e.to_string(),
            };
            tracing::warn!("{id}: git fetch failed: {failed}");
            let error = event::Error {
                r#where: Some("fetch".into()),
                error: Some(failed),
                ..Default::default()
            };
            lp.events.add_in(&id, Event::Error(error));
        }
    }

    /// Lets `claim` mode claim and start work.
    pub fn with_starter(mut self, starter: Arc<dyn SessionStarter>) -> Self {
        self.starter = Some(starter);
        self
    }

    pub fn add(&mut self, mut project: ProjectLoop) {
        project.outcomes = self.outcome_sender.clone();
        self.projects.push(project);
    }

    /// One pass over every project, in the order added. A project whose read fails (or panics) is skipped
    /// with its own `store_error` or live-state failure; the others go on.
    pub async fn pass(&mut self) -> Vec<Step> {
        let jobs: Vec<_> = self
            .projects
            .iter()
            .map(|lp| {
                tokio::spawn(read(
                    lp.client.clone(),
                    lp.id.clone(),
                    lp.project.github_repo.clone(),
                    lp.repo,
                    self.links.backlog_url.clone(),
                    self.links.keychain.clone(),
                ))
            })
            .collect();
        let mut read_items = Vec::new();
        for (lp, job) in self.projects.iter_mut().zip(jobs) {
            let done = job.await.unwrap_or_else(|e| {
                tracing::error!("{}: the backlog read failed: {e}", lp.id);
                Read {
                    repo: lp.repo,
                    items: Err(Fail::Cant(format!("the backlog read failed: {e}"))),
                }
            });
            lp.repo = done.repo;
            read_items.push(match done.items {
                Ok(items) => Some(items),
                Err(Fail::Cant(message)) => {
                    lp.cant_start(message);
                    None
                }
                Err(Fail::Store(e)) => {
                    lp.store_error("list", &e);
                    None
                }
            });
        }
        let paused = self.paused();
        let n = self.projects.len();
        // A panic deciding or recording costs this pass only: it is logged and the next pass runs.
        match AssertUnwindSafe(self.decide_and_record(read_items, paused))
            .catch_unwind()
            .await
        {
            Ok(steps) => steps,
            Err(panic) => {
                tracing::error!("orchestrator: a pass panicked, skipping it: {}", panic_message(&*panic));
                vec![Step::Skipped; n]
            }
        }
    }

    /// The pass after the reads: takes back held leases, schedules, then records each project's pass.
    async fn decide_and_record(&mut self, read_items: Vec<Option<Vec<Item>>>, paused: Vec<bool>) -> Vec<Step> {
        for (i, items) in read_items.iter().enumerate() {
            if let Some(items) = items
                && self.live(&self.projects[i])
            {
                self.projects[i].readopt(items, &mut self.pool);
            }
        }
        if self.starter.is_some() {
            self.flush_all_owed().await;
        }
        let ready: Vec<bool> = read_items.iter().map(Option::is_some).collect();
        let scheduled = self.schedule(&ready, &paused).await;
        let mut steps = Vec::new();
        for (((lp, items), paused), (actions, live)) in
            (self.projects.iter_mut().zip(read_items).zip(paused)).zip(scheduled)
        {
            match items {
                Some(items) => steps.push(lp.record(items, paused, actions, &self.pool, live)),
                None => steps.push(Step::Skipped),
            }
        }
        steps
    }

    /// One step of the loop for the `ready` projects (read this pass, or live on a poll). The sessions that ended
    /// are settled, and the live projects take in their ended steps (paused or not) and, unless paused, advance
    /// their items. Then starts are planned round robin within the
    /// session slots left (`concurrency` less the starter's `active`), and the live ones apply them and ask the
    /// store for the lanes and slots still free. Each project's actions, and for a live one its claim answer.
    async fn schedule(&mut self, ready: &[bool], paused: &[bool]) -> Vec<(Vec<Action>, Option<Option<String>>)> {
        let starter = self.starter.clone();
        let live: Vec<bool> = (self.projects.iter().zip(ready))
            .map(|(lp, &ready)| ready && self.live(lp))
            .collect();
        if let Some(starter) = starter.as_deref() {
            self.settle_outcomes().await;
            for ((lp, &live), &paused) in self.projects.iter_mut().zip(&live).zip(paused) {
                if live {
                    lp.resume_sessions(&mut self.pool, starter, &self.links).await;
                    lp.take_finished(&mut self.pool, &self.links).await;
                    if !paused {
                        lp.advance(&self.pool, starter);
                    }
                }
            }
        }
        let mut slots = (self.concurrency).saturating_sub(starter.as_ref().map_or(0, |s| s.active()));
        let mut candidates: Vec<Option<Candidate>> = (self.projects.iter().zip(ready).zip(paused))
            .map(|((lp, &ready), &paused)| {
                ready.then(|| Candidate {
                    id: &lp.id,
                    paused,
                    runs: lp.runs.clone(),
                    cfg: &lp.project,
                    rush: (lp.items.values())
                        .filter(|i| i.do_next)
                        .map(|i| i.id.clone())
                        .collect(),
                })
            })
            .collect();
        let (plans, last) = plan(&self.pool, self.last_served, slots, &mut candidates);
        drop(candidates);
        self.last_served = last;
        let mut scheduled = Vec::new();
        for ((lp, actions), live) in self.projects.iter_mut().zip(plans).zip(live) {
            let mut answer = None;
            if let Some(starter) = starter.as_deref().filter(|_| live) {
                slots = slots.saturating_sub(lp.apply_starts(&actions, &mut self.pool, starter, &self.links).await);
                answer = Some(if actions.contains(&Action::AskForWork) {
                    lp.claim(&mut self.pool, &mut slots, &self.links, starter).await
                } else {
                    None
                });
            }
            scheduled.push((actions, answer));
        }
        scheduled
    }
}

/// What a caught panic said.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    (panic.downcast_ref::<&str>().map(|s| s.to_string()))
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

/// What `spawn_all` starts the orchestrator from.
pub struct SpawnArgs<'a> {
    pub dir: &'a Path,
    pub projects: &'a [ProjectConfig],
    pub lane_count: u32,
    /// `concurrency`: how many sessions may run at once, across every project.
    pub concurrency: u32,
    /// `pollIntervalSec`: how often the loop steps between the minute's passes.
    pub poll_every: Duration,
    pub backlog_url: &'a str,
    pub keychain: &'a Arc<dyn TokenStore>,
    pub paused: &'a PauseState,
    pub live: &'a LiveHandle,
    pub guard: Option<&'a PortGuard>,
    pub machine: String,
    pub environment: String,
    /// Runs sessions for claimed work. None in production until the implementer session is built.
    pub starter: Option<Arc<dyn SessionStarter>>,
}

/// Starts the computer's one orchestrator task when the mode isn't `Off`. A project that can't loop (no
/// GitHub repo, unreadable state) is logged and left out; the others go on.
pub fn spawn_all(args: SpawnArgs) {
    if args.live.snapshot().mode == OrchestratorMode::Off {
        return;
    }
    let mut machine = Machine::new(
        args.lane_count,
        args.concurrency,
        Links {
            backlog_url: args.backlog_url.to_string(),
            keychain: args.keychain.clone(),
            paused: args.paused.clone(),
            machine: args.machine,
            environment: args.environment,
        },
    );
    if let Some(starter) = args.starter {
        machine = machine.with_starter(starter);
    } else if args.live.snapshot().mode == OrchestratorMode::Claim {
        tracing::warn!("claim mode: no session runner yet, running dry");
    }
    for project in args.projects {
        let id = project.id_or_derived();
        if project.github_repo.is_empty() {
            tracing::warn!("{id}: orchestrator skipped: the project has no githubRepo, so no backlog repo id");
            continue;
        }
        let client = Client::new(args.backlog_url);
        match ProjectLoop::open(args.dir, project, client, args.live.clone(), args.guard.cloned()) {
            Ok(lp) => machine.add(lp),
            Err(e) => tracing::warn!("{id}: orchestrator skipped: {e:#}"),
        }
    }
    if !machine.projects.is_empty() {
        tokio::spawn(run(machine, args.poll_every));
    }
}

/// The poll's period: at least a second. The file's `pollIntervalSec` is used as written (only its environment
/// variable is range-checked), and a hand-edited `0` would panic `tokio::time::interval` and end the loop.
fn poll_period(poll_every: Duration) -> Duration {
    poll_every.max(Duration::from_secs(1))
}

/// The loop (section 7's pseudocode): every `poll_every` a poll, and about once a minute (at the first poll
/// too) the fetch and the pass that reads every backlog and writes the `tick`s in its place.
async fn run(mut machine: Machine, poll_every: Duration) {
    let mut every = tokio::time::interval(poll_period(poll_every));
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut minute_at: Option<tokio::time::Instant> = None;
    loop {
        let now = every.tick().await;
        if minute_at.is_none_or(|at| now.duration_since(at) >= EVERY) {
            minute_at = Some(now);
            machine.fetch().await;
            machine.pass().await;
        } else {
            machine.poll().await;
        }
    }
}

#[cfg(test)]
mod tests;
