//! The loop that owns the computer's orchestrator state (one shared lane pool, each project's state) and calls `orchestrator::decide` on a clock
//! (`Pool.tick` in Retaskable.com's `agent/src/pool.ts`), behind a safety switch,
//! `RETASKABLE_ORCHESTRATOR`. Today only the **dry** loop exists: each minute (the TS agent's `minute()`
//! writes the `tick` event and its fetch of the store's items on that cadence; its 10 second poll drives
//! `step()`, which has no counterpart here yet) it reads the project's items from the shared backlog,
//! decides what a tick would do (`decide` is pure, and nothing is applied), and writes a `tick` event saying so.
//! It never starts a session, takes a lane, asks the store for work or writes to it. A store that
//! can't be reached, or answers something unreadable, is a `store_error` event and a skipped tick, never
//! an empty backlog (docs/specs/agent/02-principles.md: fail closed).
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::FutureExt;
use serde_json::{Value, json};
use tokio::time::MissedTickBehavior;

use super::guard::PortGuard;
use super::live::{HeldLane, LiveHandle, now_iso8601};
use super::round_robin::{Candidate, plan};
use super::{Action, ProjectRuns, apply, decide, tick_payload};
use crate::backlog::{BacklogError, ClaimNextAgent, ClaimNextAnswer, Client, Item, ItemStatus};
use crate::config::ProjectConfig;
use crate::env::Env;
use crate::item_lifecycle::ItemRun;
use crate::lane_pool::{Holder, LanePool, PoolRules};
use crate::link::TokenStore;
use crate::store::Store;
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
}

/// Runs the implementer session for an item that took a lane. Built in a later piece; until one is given to
/// the [`Machine`], `claim` mode claims nothing, so no claimed item is left with nothing working on it.
pub trait SessionStarter: Send + Sync {
    /// Called once the item is `implementing` and its lane held. Must not block: start the session and return.
    fn start(&self, request: StartRequest);
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
            store,
            runs: ProjectRuns::new(),
            adopted: false,
            mode: live.snapshot().mode,
            guard: None,
            events,
            client: Arc::new(client),
            live,
            repo: None,
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

    /// Writes the held leases to the state file.
    fn save_leases(&mut self) {
        self.store.set_leases(&self.leases);
        if let Err(e) = self.store.save() {
            tracing::warn!("{}: saving the held leases failed: {e:#}", self.id);
        }
    }

    /// Takes a lease into `runs` as a queued item, with the `lease` event the TS agent's store writes.
    fn take_lease(&mut self, item: &Item, token: String) {
        let mut run = ItemRun::new();
        run.slice = item.slice;
        self.runs.insert(item.id.clone(), run);
        self.leases.insert(item.id.clone(), token);
        self.save_leases();
        self.events.add_in(
            &self.id,
            Event::Lease(event::Lease {
                item: Some(item.id.clone()),
                change: Some("taken".into()),
                state: Some("queued".into()),
                ..Default::default()
            }),
        );
    }

    /// On the first pass, takes back the items this computer still holds a lease on (the TS agent keeps the
    /// token in its state file, so a restart carries on with them). One the store no longer shows as handed
    /// out (taken by another agent, finished, gone) is dropped; the rest start again as queued.
    fn readopt(&mut self, items: &[Item]) {
        if std::mem::replace(&mut self.adopted, true) || self.leases.is_empty() {
            return;
        }
        for (id, token) in std::mem::take(&mut self.leases) {
            match items.iter().find(|i| i.id == id) {
                Some(item) if matches!(item.status, ItemStatus::Claimed | ItemStatus::Building) => {
                    tracing::info!("{}: still holds {id}, carrying on", self.id);
                    self.take_lease(item, token);
                }
                _ => tracing::info!("{}: no longer holds {id}, dropping its lease", self.id),
            }
        }
        self.save_leases();
    }

    /// Applies the starts decided to the real runs and pool, and calls the starter for each that took its lane.
    fn apply_starts(&mut self, actions: &[Action], pool: &mut LanePool, starter: &dyn SessionStarter) {
        for action in actions.iter().filter(|a| **a != Action::AskForWork) {
            if !apply(&self.id, action, &mut self.runs, pool, &self.project, &mut self.events) {
                continue;
            }
            if let Action::StartImplementer { item_id, lane } = action {
                starter.start(StartRequest {
                    project: self.id.clone(),
                    item_id: item_id.clone(),
                    lane: *lane,
                    token: self.leases.get(item_id).cloned().unwrap_or_default(),
                });
            }
        }
    }

    /// Asks the store for work (at most once per pass) and starts what it hands out. The answer for the
    /// `claim` event: the item id, `none` or `failed`; `None` when it wasn't asked (the guard, or no link).
    async fn claim(&mut self, pool: &mut LanePool, links: &Links, starter: &dyn SessionStarter) -> Option<String> {
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
        let answer = match self.client.claim_next(repo, Some(&token), &agent).await {
            Err(e) => {
                self.store_error("claim_next", &e);
                "failed".to_string()
            }
            Ok(ClaimNextAnswer::Nothing) => "none".to_string(),
            Ok(ClaimNextAnswer::Item { item, token }) => {
                self.take_lease(&item, token);
                let actions = decide(&self.id, false, &self.runs, pool, &self.project);
                self.apply_starts(&actions, pool, starter);
                item.id.clone()
            }
        };
        self.events.add_in(
            &self.id,
            Event::Claim(event::Claim {
                answer: Some(json!(answer)),
                ..Default::default()
            }),
        );
        Some(answer)
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
}

impl Machine {
    pub fn new(lane_count: u32, links: Links) -> Self {
        Self {
            pool: LanePool::new(PoolRules {
                lanes: lane_count,
                ..Default::default()
            }),
            projects: Vec::new(),
            last_served: None,
            links,
            starter: None,
        }
    }

    /// Lets `claim` mode claim and start work.
    pub fn with_starter(mut self, starter: Arc<dyn SessionStarter>) -> Self {
        self.starter = Some(starter);
        self
    }

    pub fn add(&mut self, project: ProjectLoop) {
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
        let paused: Vec<bool> = self
            .projects
            .iter()
            .map(|lp| self.links.paused.reason(&lp.id).is_some())
            .collect();
        let n = self.projects.len();
        // A panic deciding or recording costs this pass only: it is logged and the next pass runs.
        match AssertUnwindSafe(self.decide_and_record(read_items, paused))
            .catch_unwind()
            .await
        {
            Ok(steps) => steps,
            Err(panic) => {
                let why = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                tracing::error!("orchestrator: a pass panicked, skipping it: {why}");
                vec![Step::Skipped; n]
            }
        }
    }

    /// The pass after the reads: plans the starts, and in `claim` mode with a starter applies them, asks
    /// for work and starts what comes back, then records each project's pass.
    async fn decide_and_record(&mut self, read_items: Vec<Option<Vec<Item>>>, paused: Vec<bool>) -> Vec<Step> {
        let starter = self.starter.clone();
        for (lp, items) in self.projects.iter_mut().zip(&read_items) {
            if let (Some(items), Some(_)) = (items, &starter)
                && lp.mode == OrchestratorMode::Claim
            {
                lp.readopt(items);
            }
        }
        let mut candidates: Vec<Option<Candidate>> = (self.projects.iter().zip(&read_items).zip(&paused))
            .map(|((lp, items), &paused)| {
                items.as_ref().map(|_| Candidate {
                    id: &lp.id,
                    paused,
                    runs: lp.runs.clone(),
                    cfg: &lp.project,
                })
            })
            .collect();
        let (plans, last) = plan(&self.pool, self.last_served, &mut candidates);
        drop(candidates);
        self.last_served = last;
        let mut steps = Vec::new();
        for (((lp, items), paused), actions) in self.projects.iter_mut().zip(read_items).zip(paused).zip(plans) {
            let Some(items) = items else {
                steps.push(Step::Skipped);
                continue;
            };
            let mut live = None;
            if let Some(starter) = starter.as_deref()
                && lp.mode == OrchestratorMode::Claim
            {
                lp.apply_starts(&actions, &mut self.pool, starter);
                live = Some(if actions.contains(&Action::AskForWork) {
                    lp.claim(&mut self.pool, &self.links, starter).await
                } else {
                    None
                });
            }
            steps.push(lp.record(items, paused, actions, &self.pool, live));
        }
        steps
    }
}

/// What `spawn_all` starts the orchestrator from.
pub struct SpawnArgs<'a> {
    pub dir: &'a Path,
    pub projects: &'a [ProjectConfig],
    pub lane_count: u32,
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
        tokio::spawn(run(machine));
    }
}

async fn run(mut machine: Machine) {
    let mut every = tokio::time::interval(EVERY);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        machine.pass().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item_lifecycle::ItemRun;
    use crate::test_support::item_lifecycle_cfg;
    use axum::{Json, Router, routing::post};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn the_mode_defaults_to_off_and_reads_the_three_names() {
        let mode = |v: Option<&str>| match v {
            Some(v) => OrchestratorMode::from_env(&Env::from_pairs([(MODE_VAR, v)])),
            None => OrchestratorMode::from_env(&Env::empty()),
        };
        assert_eq!(mode(None), (OrchestratorMode::Off, None));
        assert_eq!(mode(Some("off")), (OrchestratorMode::Off, None));
        assert_eq!(mode(Some("dry")), (OrchestratorMode::Dry, None));
        assert_eq!(mode(Some(" Claim ")), (OrchestratorMode::Claim, None));
    }

    #[test]
    fn an_unrecognised_mode_is_off_with_a_warning() {
        let (mode, warning) = OrchestratorMode::from_env(&Env::from_pairs([(MODE_VAR, "yes")]));
        assert_eq!(mode, OrchestratorMode::Off);
        assert!(warning.unwrap().contains("stays off"));
    }

    /// A fake store that remembers each op it was asked, answering `list` with one todo item.
    async fn stub() -> (String, Arc<Mutex<Vec<String>>>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        let app = Router::new().route(
            "/v1/repos/{id}/ops",
            post(move |Json(body): Json<Value>| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(body["op"].as_str().unwrap_or("").to_string());
                    Json(json!({ "value": [{ "id": "a", "title": "A", "status": "todo", "rank": 0 }] }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), asked)
    }

    /// The link token the keychain answers with (`None` is no link), and whether the next read panics.
    struct Keys {
        token: Mutex<Option<String>>,
        panic_once: AtomicBool,
    }

    impl TokenStore for Keys {
        fn get(&self, _url: &str) -> Result<Option<String>> {
            if self.panic_once.swap(false, Ordering::SeqCst) {
                panic!("a read that goes wrong");
            }
            Ok(self.token.lock().unwrap().clone())
        }
        fn set(&self, _url: &str, _token: &str) -> Result<()> {
            Ok(())
        }
        fn delete(&self, _url: &str) -> Result<()> {
            Ok(())
        }
    }

    struct Rig {
        machine: Machine,
        keys: Arc<Keys>,
        live: LiveHandle,
        _dir: tempfile::TempDir,
    }

    impl Rig {
        fn lp(&mut self, i: usize) -> &mut ProjectLoop {
            &mut self.machine.projects[i]
        }

        fn pause(&self, project: &str, on: bool) {
            self.machine.links.paused.set(project, on.then(|| "test".to_string()));
        }

        /// Puts every project in `claim` mode with a fake session starter.
        fn go_live(&mut self) -> Arc<Fake> {
            let fake = Arc::new(Fake::default());
            self.machine.starter = Some(fake.clone());
            for lp in &mut self.machine.projects {
                lp.mode = OrchestratorMode::Claim;
            }
            fake
        }

        /// Gives a project `ids` as queued items, to be started.
        fn queue(&mut self, i: usize, ids: &[&str]) {
            for id in ids {
                self.lp(i).runs.insert(id.to_string(), ItemRun::new());
            }
        }
    }

    /// One project per base URL, named `p0`, `p1`, ..., on a computer with `lanes` lanes.
    fn rig(lanes: u32, bases: &[&str]) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let keys = Arc::new(Keys {
            token: Mutex::new(Some("tok".into())),
            panic_once: AtomicBool::new(false),
        });
        let live = LiveHandle::new(OrchestratorMode::Dry);
        let links = Links {
            backlog_url: "https://backlog.example".into(),
            keychain: keys.clone(),
            paused: PauseState::default(),
            machine: "my-pc".into(),
            environment: "retaskable.com".into(),
        };
        let mut machine = Machine::new(lanes, links);
        for (i, base) in bases.iter().enumerate() {
            let store = Store::load(&dir.path().join(i.to_string())).unwrap();
            let mut lp = ProjectLoop::new(
                format!("p{i}"),
                item_lifecycle_cfg(),
                store,
                EventLog::with_now(|| 0),
                Client::new(*base),
                live.clone(),
            );
            lp.repo = Some(7);
            machine.add(lp);
        }
        Rig {
            machine,
            keys,
            live,
            _dir: dir,
        }
    }

    fn would(step: &Step) -> Vec<&str> {
        match step {
            Step::Ticked { would } => would.iter().map(String::as_str).collect(),
            Step::Skipped => panic!("skipped"),
        }
    }

    #[tokio::test]
    async fn one_dry_tick_writes_a_tick_event_and_asks_the_store_only_to_list() {
        let (base, asked) = stub().await;
        let mut rig = rig(2, &[&base]);
        let steps = rig.machine.pass().await;
        assert_eq!(
            steps,
            vec![Step::Ticked {
                would: vec!["ask_for_work".into()]
            }]
        );
        assert_eq!(
            *asked.lock().unwrap(),
            vec!["list".to_string()],
            "no claim_next was sent"
        );
        assert_eq!(
            rig.lp(0)
                .events()
                .recent()
                .iter()
                .map(|e| e.to_value())
                .collect::<Vec<_>>(),
            vec![json!({
                "at": "1970-01-01T00:00:00.000Z",
                "kind": "tick",
                "project": "p0",
                "status": "running",
                "lanes": 2,
                "busy": [],
                "free": 2,
                "sessions": 0,
                "states": { "todo": 1 },
                "claim": "not asked",
                "dry": ["ask_for_work"]
            })]
        );
        assert_eq!(rig.lp(0).runs.values().count(), 0);
        assert_eq!(rig.machine.pool.held_lanes().count(), 0, "a dry tick holds no lane");
    }

    #[tokio::test]
    async fn the_live_state_follows_each_pass() {
        let (base, _asked) = stub().await;
        let mut rig = rig(2, &[&base]);
        let proj = |rig: &Rig| rig.live.snapshot().projects["p0"].clone();
        assert_eq!(proj(&rig), Default::default(), "registered before its first pass");

        rig.machine.pass().await;
        let live = proj(&rig);
        assert!(live.last_tick.is_some());
        assert_eq!(live.decision.as_deref(), Some("would ask for work"));
        assert!(live.held.is_empty() && !live.paused && live.store_error.is_none());

        *rig.keys.token.lock().unwrap() = None;
        rig.pause("p0", true);
        rig.machine.pass().await;
        let failed = proj(&rig);
        assert!(failed.store_error.unwrap().message.contains("no link"));
        assert_eq!(
            failed.last_tick, live.last_tick,
            "a failed pass leaves the last tick as it was"
        );

        *rig.keys.token.lock().unwrap() = Some("tok".into());
        rig.machine.pass().await;
        let live = proj(&rig);
        assert!(live.paused && live.store_error.is_none());
        assert_eq!(live.decision.as_deref(), Some("paused: would do nothing"));
    }

    #[tokio::test]
    async fn a_paused_project_ticks_paused_and_would_do_nothing() {
        let (base, _asked) = stub().await;
        let mut rig = rig(2, &[&base]);
        rig.pause("p0", true);
        assert_eq!(rig.machine.pass().await, vec![Step::Ticked { would: vec![] }]);
        assert_eq!(rig.lp(0).events().recent()[0].to_value()["status"], "paused");
    }

    #[tokio::test]
    async fn an_unreachable_backlog_is_a_store_error_and_no_tick() {
        // Port 1 on loopback refuses connections.
        let mut rig = rig(2, &["http://127.0.0.1:1"]);
        assert_eq!(rig.machine.pass().await, vec![Step::Skipped]);
        let kinds: Vec<_> = rig.lp(0).events().recent().iter().map(|e| e.kind()).collect();
        assert_eq!(kinds, vec!["store_error"]);
        assert_eq!(rig.lp(0).events().recent()[0].to_value()["op"], "list");
    }

    #[tokio::test]
    async fn no_token_means_the_store_isnt_asked_and_the_tick_is_skipped() {
        let (base, asked) = stub().await;
        let mut rig = rig(2, &[&base]);
        *rig.keys.token.lock().unwrap() = None;
        assert_eq!(rig.machine.pass().await, vec![Step::Skipped]);
        assert!(asked.lock().unwrap().is_empty());
        assert_eq!(rig.lp(0).events().recent()[0].kind(), "store_error");
    }

    #[tokio::test]
    async fn two_projects_on_one_lane_take_turns_starting() {
        let (base, _asked) = stub().await;
        let mut rig = rig(1, &[&base, &base]);
        rig.queue(0, &["x"]);
        rig.queue(1, &["y"]);
        let first = rig.machine.pass().await;
        assert_eq!(would(&first[0]), ["start_implementer x in lane 1"]);
        assert!(would(&first[1]).is_empty(), "the one lane went to p0");
        let second = rig.machine.pass().await;
        assert_eq!(would(&second[1]), ["start_implementer y in lane 1"]);
        assert!(would(&second[0]).is_empty(), "p1 is served first after p0");
        assert_eq!(rig.machine.pool.held_lanes().count(), 0, "nothing was applied");
    }

    #[tokio::test]
    async fn three_projects_with_work_and_two_lanes_alternate() {
        let (base, _asked) = stub().await;
        let mut rig = rig(2, &[&base, &base, &base]);
        for i in 0..3 {
            rig.queue(i, &["i1", "i2"]);
        }
        let first = rig.machine.pass().await;
        assert_eq!(would(&first[0]), ["start_implementer i1 in lane 1"]);
        assert_eq!(would(&first[1]), ["start_implementer i1 in lane 2"]);
        assert!(would(&first[2]).is_empty());
        let second = rig.machine.pass().await;
        assert_eq!(would(&second[2]), ["start_implementer i1 in lane 1"]);
        assert_eq!(would(&second[0]), ["start_implementer i1 in lane 2"]);
        assert!(would(&second[1]).is_empty());
    }

    #[tokio::test]
    async fn one_projects_unreachable_backlog_leaves_the_others_ticking() {
        let (base, _asked) = stub().await;
        let mut rig = rig(2, &["http://127.0.0.1:1", &base, &base]);
        let steps = rig.machine.pass().await;
        assert_eq!(steps[0], Step::Skipped);
        assert_eq!(would(&steps[1]), ["ask_for_work"]);
        assert_eq!(would(&steps[2]), ["ask_for_work"]);
        let projects = rig.live.snapshot().projects;
        assert!(projects["p0"].store_error.is_some() && projects["p0"].last_tick.is_none());
        assert!(projects["p1"].store_error.is_none() && projects["p2"].last_tick.is_some());
        assert_eq!(rig.lp(0).events().recent()[0].kind(), "store_error");
    }

    #[tokio::test]
    async fn a_panicking_read_fails_only_its_project() {
        let (base, _asked) = stub().await;
        let mut rig = rig(2, &[&base, &base]);
        rig.keys.panic_once.store(true, Ordering::SeqCst);
        let steps = rig.machine.pass().await;
        assert_eq!(steps.iter().filter(|s| **s == Step::Skipped).count(), 1);
        assert_eq!(steps.iter().filter(|s| matches!(s, Step::Ticked { .. })).count(), 1);
        let failed = rig
            .live
            .snapshot()
            .projects
            .into_values()
            .filter(|p| p.store_error.is_some());
        assert!(
            failed
                .map(|p| p.store_error.unwrap().message)
                .all(|m| m.contains("read failed"))
        );
        assert!(
            matches!(rig.machine.pass().await[..], [Step::Ticked { .. }, Step::Ticked { .. }]),
            "the next pass reads both"
        );
    }

    #[tokio::test]
    async fn held_lanes_show_in_live_state_under_the_project_that_holds_them() {
        let (base, _asked) = stub().await;
        let mut rig = rig(3, &[&base, &base]);
        let held = |project: &str, item: &str| Holder::Item {
            project: project.into(),
            item: item.into(),
        };
        assert!(rig.machine.pool.take(1, held("p0", "a")));
        assert!(rig.machine.pool.take(3, held("p1", "b")));
        rig.machine.pass().await;
        let projects = rig.live.snapshot().projects;
        let lane = |lane, item: &str, title: Option<&str>| HeldLane {
            lane,
            item_id: item.into(),
            title: title.map(Into::into),
        };
        assert_eq!(projects["p0"].held, vec![lane(1, "a", Some("A"))]);
        assert_eq!(projects["p1"].held, vec![lane(3, "b", None)]);
        let busy = rig.lp(1).events().recent()[0].to_value()["busy"].clone();
        assert_eq!(busy, json!([1, 3]), "the tick event shows the computer's lanes");
    }

    async fn ts_agent_port() -> u16 {
        let app = Router::new().route(
            "/api/health",
            axum::routing::get(|| async { Json(json!({ "ok": true, "version": "1" })) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        port
    }

    fn closed_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[tokio::test]
    async fn claim_runs_dry_while_another_agent_answers_and_stays_claim_otherwise() {
        let guard = PortGuard::new(closed_port(), [ts_agent_port().await]);
        assert_eq!(
            effective_mode(OrchestratorMode::Claim, &guard).await,
            OrchestratorMode::Dry
        );
        let quiet = PortGuard::new(closed_port(), [closed_port()]);
        assert_eq!(
            effective_mode(OrchestratorMode::Claim, &quiet).await,
            OrchestratorMode::Claim
        );
        assert_eq!(
            effective_mode(OrchestratorMode::Dry, &guard).await,
            OrchestratorMode::Dry
        );
    }

    #[tokio::test]
    async fn a_claim_mode_tick_still_only_lists_with_or_without_the_guard_blocking() {
        for blocked in [true, false] {
            let (base, asked) = stub().await;
            let mut rig = rig(2, &[&base]);
            let port = if blocked { ts_agent_port().await } else { closed_port() };
            let lp = rig.lp(0);
            lp.guard = Some(PortGuard::new(closed_port(), [port]));
            lp.mode = OrchestratorMode::Claim;
            assert_eq!(
                rig.machine.pass().await,
                vec![Step::Ticked {
                    would: vec!["ask_for_work".into()]
                }]
            );
            assert_eq!(*asked.lock().unwrap(), vec!["list".to_string()]);
        }
    }

    /// A session starter that records what it was asked to start, and can panic once.
    #[derive(Default)]
    struct Fake {
        started: Mutex<Vec<StartRequest>>,
        panic_once: AtomicBool,
    }

    impl SessionStarter for Fake {
        fn start(&self, request: StartRequest) {
            self.started.lock().unwrap().push(request);
            if self.panic_once.swap(false, Ordering::SeqCst) {
                panic!("a starter that goes wrong");
            }
        }
    }

    type Asked = Arc<Mutex<Vec<(String, Value)>>>;

    /// A store that answers `list` with `listed` and `claim_next` with `claim` (a value) or, for `None`, an error.
    async fn claim_stub(listed: Value, claim: Option<Value>) -> (String, Asked) {
        use axum::http::StatusCode;
        let asked: Asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        let app = Router::new().route(
            "/v1/repos/{id}/ops",
            post(move |Json(body): Json<Value>| {
                let (seen, listed, claim) = (seen.clone(), listed.clone(), claim.clone());
                async move {
                    let op = body["op"].as_str().unwrap_or("").to_string();
                    seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                    match (op.as_str(), claim) {
                        ("claim_next", None) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" }))),
                        ("claim_next", Some(v)) => (StatusCode::OK, Json(json!({ "value": v }))),
                        _ => (StatusCode::OK, Json(json!({ "value": listed }))),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), asked)
    }

    fn item(id: &str, status: &str) -> Value {
        json!({ "id": id, "title": id.to_uppercase(), "status": status, "rank": 0 })
    }

    fn ops(asked: &Asked) -> Vec<String> {
        asked.lock().unwrap().iter().map(|(op, _)| op.clone()).collect()
    }

    fn kinds(rig: &mut Rig) -> Vec<String> {
        rig.lp(0).events().recent().iter().map(|e| e.kind()).collect()
    }

    fn handed_out() -> Option<Value> {
        Some(json!({ "item": item("a", "claimed"), "token": "lease-1" }))
    }

    #[tokio::test]
    async fn claim_mode_claims_once_queues_then_starts_and_holds_the_lane() {
        let (base, asked) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
        let mut rig = rig(2, &[&base]);
        let fake = rig.go_live();
        let steps = rig.machine.pass().await;
        assert_eq!(would(&steps[0]), ["ask_for_work"]);
        assert_eq!(
            ops(&asked),
            ["list", "claim_next"],
            "one claim for the project, though two lanes were free"
        );
        let agent = asked.lock().unwrap()[1].1["agent"].clone();
        assert_eq!(
            agent,
            json!({ "machine": "my-pc", "environment": "retaskable.com", "baseBranch": "" })
        );
        assert_eq!(
            *fake.started.lock().unwrap(),
            vec![StartRequest {
                project: "p0".into(),
                item_id: "a".into(),
                lane: 1,
                token: "lease-1".into()
            }]
        );
        assert_eq!(
            rig.lp(0).runs["a"].state,
            crate::item_lifecycle::ItemState::Implementing
        );
        assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
        let held = rig.live.snapshot().projects["p0"].held.clone();
        assert_eq!(held.len(), 1);
        assert_eq!(
            (held[0].lane, held[0].item_id.as_str(), held[0].title.as_deref()),
            (1, "a", Some("A"))
        );
        assert_eq!(kinds(&mut rig), ["lease", "lane_taken", "claim", "tick"]);
        assert_eq!(rig.lp(0).events().recent()[2].to_value()["answer"], "a");
        assert_eq!(rig.lp(0).events().recent()[3].to_value()["claim"], "a");
        assert_eq!(
            rig.lp(0).store.leases()["a"],
            "lease-1",
            "the lease is kept for a restart"
        );
    }

    #[tokio::test]
    async fn claim_mode_without_a_starter_sends_no_claim() {
        let (base, asked) = claim_stub(json!([]), handed_out()).await;
        let mut rig = rig(2, &[&base]);
        rig.lp(0).mode = OrchestratorMode::Claim;
        rig.machine.pass().await;
        assert_eq!(ops(&asked), ["list"]);
        assert_eq!(rig.machine.pool.held_lanes().count(), 0);
    }

    #[tokio::test]
    async fn a_guard_block_means_no_claim_next() {
        let (base, asked) = claim_stub(json!([]), handed_out()).await;
        let mut rig = rig(2, &[&base]);
        let fake = rig.go_live();
        rig.lp(0).guard = Some(PortGuard::new(closed_port(), [ts_agent_port().await]));
        assert!(matches!(rig.machine.pass().await[..], [Step::Ticked { .. }]));
        assert_eq!(ops(&asked), ["list"]);
        assert!(fake.started.lock().unwrap().is_empty() && rig.lp(0).runs.is_empty());
    }

    #[tokio::test]
    async fn a_store_error_on_claim_is_logged_and_never_read_as_nothing() {
        let (base, asked) = claim_stub(json!([]), None).await;
        let mut rig = rig(2, &[&base]);
        let fake = rig.go_live();
        rig.machine.pass().await;
        assert_eq!(ops(&asked), ["list", "claim_next"]);
        assert_eq!(kinds(&mut rig), ["store_error", "claim", "tick"]);
        assert_eq!(rig.lp(0).events().recent()[0].to_value()["op"], "claim_next");
        assert_eq!(rig.lp(0).events().recent()[1].to_value()["answer"], "failed");
        assert!(rig.lp(0).runs.is_empty() && rig.machine.pool.held_lanes().count() == 0);
        assert!(fake.started.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_restart_takes_back_the_items_it_still_holds_and_drops_the_rest() {
        let listed = json!([item("a", "building"), item("b", "on_main")]);
        let (base, asked) = claim_stub(listed, Some(Value::Null)).await;
        let mut rig = rig(2, &[&base]);
        let fake = rig.go_live();
        let held = [("a", "old"), ("b", "older"), ("gone", "x")];
        rig.lp(0).leases = held.iter().map(|(i, t)| (i.to_string(), t.to_string())).collect();
        rig.machine.pass().await;
        let started = fake.started.lock().unwrap().clone();
        assert_eq!(
            (started.len(), started[0].item_id.as_str(), started[0].token.as_str()),
            (1, "a", "old")
        );
        assert_eq!(rig.lp(0).store.leases().keys().collect::<Vec<_>>(), ["a"]);
        assert_eq!(ops(&asked), ["list", "claim_next"], "the other lane may still claim");
        assert_eq!(rig.lp(0).events().recent().last().unwrap().to_value()["claim"], "none");
    }

    #[tokio::test]
    async fn a_panic_in_the_pass_is_caught_and_the_next_pass_runs() {
        let (base, asked) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
        let mut rig = rig(2, &[&base]);
        let fake = rig.go_live();
        fake.panic_once.store(true, Ordering::SeqCst);
        assert_eq!(rig.machine.pass().await, vec![Step::Skipped]);
        assert!(matches!(rig.machine.pass().await[..], [Step::Ticked { .. }]));
        assert_eq!(ops(&asked).iter().filter(|o| *o == "list").count(), 2);
    }
}
