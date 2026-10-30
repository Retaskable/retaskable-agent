//! The loop that owns one project's orchestrator state and calls `orchestrator::tick` on a clock
//! (`Pool.tick` in Retaskable.com's `agent/src/pool.ts`), behind a safety switch,
//! `RETASKABLE_ORCHESTRATOR`. Today only the **dry** loop exists: each minute (the TS agent's `minute()`
//! writes the `tick` event and its fetch of the store's items on that cadence; its 10 second poll drives
//! `step()`, which has no counterpart here yet) it reads the project's items from the shared backlog,
//! decides what a tick would do (`decide` is pure, and nothing is applied), and writes a `tick` event saying so.
//! It never starts a session, takes a lane, asks the store for work or writes to it. A store that
//! can't be reached, or answers something unreadable, is a `store_error` event and a skipped tick, never
//! an empty backlog (docs/specs/agent/02-principles.md: fail closed).
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::time::MissedTickBehavior;

use super::guard::PortGuard;
use super::live::{HeldLane, LiveHandle, now_iso8601};
use super::{Action, ProjectRuns, decide, tick_payload};
use crate::backlog::{BacklogError, Client};
use crate::config::ProjectConfig;
use crate::env::Env;
use crate::lane_pool::{Holder, LanePool, PoolRules};
use crate::link::TokenStore;
use crate::store::Store;
use crate::telemetry::{Event, EventLog, event};
use crate::view::PauseState;

/// The variable that switches the orchestrator on.
pub const MODE_VAR: &str = "RETASKABLE_ORCHESTRATOR";

/// How often a project's loop reads the backlog and writes its `tick` event: the TS agent's once a
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
    /// Will claim and run work. Not built yet: behaves exactly like `Dry`, and only while the port guard
    /// (`guard.rs`) sees no other agent on this computer.
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
            Self::Claim => tracing::warn!("orchestrator: claim isn't implemented yet, so it runs dry: it never claims"),
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

/// One project's orchestrator state, kept for as long as the agent runs.
pub struct ProjectLoop {
    id: String,
    project: ProjectConfig,
    /// Loaded so a project whose state file can't be read doesn't loop; nothing in the dry loop writes it.
    _store: Store,
    runs: ProjectRuns,
    pool: LanePool,
    mode: OrchestratorMode,
    /// Present in `claim` mode: asked again before each claim.
    guard: Option<PortGuard>,
    events: EventLog,
    client: Client,
    live: LiveHandle,
}

impl ProjectLoop {
    pub fn new(
        id: String,
        project: ProjectConfig,
        store: Store,
        lane_count: u32,
        events: EventLog,
        client: Client,
        live: LiveHandle,
    ) -> Self {
        live.register(&id);
        Self {
            id,
            project,
            _store: store,
            runs: ProjectRuns::new(),
            pool: LanePool::new(PoolRules {
                lanes: lane_count,
                ..Default::default()
            }),
            mode: live.snapshot().mode,
            guard: None,
            events,
            client,
            live,
        }
    }

    /// Loads the project's state and opens its event log under `<data dir>/projects/<id>`.
    pub fn open(
        dir: &Path,
        lane_count: u32,
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
        Ok(Self::new(id, project.clone(), store, lane_count, events, client, live).with_guard(guard))
    }

    /// Asks the port guard before each claim (set in `claim` mode).
    pub fn with_guard(mut self, guard: Option<PortGuard>) -> Self {
        self.guard = guard;
        self
    }

    pub fn events(&self) -> &EventLog {
        &self.events
    }

    /// One pass: read the items, then decide. Without a `token` the store isn't asked at all.
    pub async fn step(&mut self, repo_id: u64, token: Option<&str>, paused: bool) -> Step {
        let items = match token {
            None => Err(BacklogError::BadAnswer {
                status: 401,
                code: Some("unauthorized".into()),
                message: "this computer holds no link to the backlog store".into(),
            }),
            Some(_) => self.client.list(repo_id, token).await,
        };
        let items = match items {
            Ok(items) => items,
            Err(e) => return self.store_error("list", &e),
        };
        let mut states: BTreeMap<String, u32> = BTreeMap::new();
        for item in &items {
            if let Ok(Value::String(s)) = serde_json::to_value(item.status) {
                *states.entry(s).or_default() += 1;
            }
        }
        // Decide on the real state; the dry loop applies nothing, so the real runs, pool and log see
        // only the `tick` event below. The pool is locked just for this read.
        let (actions, held, payload) = {
            let pool = &self.pool;
            let actions = decide(&self.id, paused, &self.runs, pool, &self.project);
            let held: Vec<HeldLane> = pool
                .held_lanes()
                .filter_map(|lane| match pool.holder(lane)? {
                    Holder::Item { item, .. } | Holder::Play { item, .. } => Some(HeldLane {
                        lane,
                        title: items.iter().find(|i| &i.id == item).map(|i| i.title.clone()),
                        item_id: item.clone(),
                    }),
                })
                .collect();
            (actions, held, tick_payload(paused, pool))
        };
        // A queued item past its slice limit is no start and no ask: the dry tick never listed it.
        let actions: Vec<Action> = actions
            .into_iter()
            .filter(|a| !matches!(a, Action::NeedsHuman { .. }))
            .collect();
        let would: Vec<String> = actions.iter().map(describe).collect();
        if self.mode == OrchestratorMode::Claim && actions.contains(&Action::AskForWork) {
            // Asked before every claim, so a TS agent started since is noticed at once. Claiming isn't
            // built, so a pass that gets this far only says so.
            // The guard is cloned out so no borrow of the loop is held across the await.
            let refusal = match self.guard.clone() {
                Some(guard) => guard.refusal().await,
                None => None,
            };
            match refusal {
                Some(why) => tracing::error!("{}: not claiming: {why}", self.id),
                None => tracing::warn!("{}: claim isn't built yet: not claiming", self.id),
            }
        }
        let decision = match actions.first() {
            None if paused => "paused: would do nothing".to_string(),
            None => "would do nothing".to_string(),
            Some(Action::AskForWork) => "would ask for work".to_string(),
            Some(Action::StartImplementer { item_id, lane }) => format!("would start {item_id} on lane {lane}"),
            Some(Action::NeedsHuman { .. }) => unreachable!("filtered out above"),
        };
        self.live.ticked(&self.id, paused, now_iso8601(), held, decision);
        let mut payload = payload;
        payload.states = Some(json!(states));
        payload.claim = Some(json!("not asked"));
        payload.extra.insert("dry".into(), json!(would));
        self.events.add_in(&self.id, Event::Tick(payload));
        if !would.is_empty() {
            tracing::debug!("{}: dry tick would: {}", self.id, would.join(", "));
        }
        Step::Ticked { would }
    }

    /// A pass that couldn't even ask the store (no repo id, no link): the page hears of it too.
    fn cant_start(&self, message: String) {
        self.live.failed(&self.id, now_iso8601(), message);
    }

    fn store_error(&mut self, op: &str, e: &BacklogError) -> Step {
        tracing::warn!("{}: dry tick skipped: {e}", self.id);
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
        Step::Skipped
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

/// Starts one task per project when the mode isn't `Off`. A project that can't loop (no GitHub repo,
/// unreadable state) is logged and left out; the others go on. Each task is its own, so one failing
/// never stops another.
#[allow(clippy::too_many_arguments)]
pub fn spawn_all(
    dir: &Path,
    projects: &[ProjectConfig],
    lane_count: u32,
    backlog_url: &str,
    keychain: &Arc<dyn TokenStore>,
    paused: &PauseState,
    live: &LiveHandle,
    guard: Option<&PortGuard>,
) {
    if live.snapshot().mode == OrchestratorMode::Off {
        return;
    }
    for project in projects {
        let id = project.id_or_derived();
        if project.github_repo.is_empty() {
            tracing::warn!("{id}: orchestrator skipped: the project has no githubRepo, so no backlog repo id");
            continue;
        }
        let client = Client::new(backlog_url);
        let lp = match ProjectLoop::open(dir, lane_count, project, client, live.clone(), guard.cloned()) {
            Ok(lp) => lp,
            Err(e) => {
                tracing::warn!("{id}: orchestrator skipped: {e:#}");
                continue;
            }
        };
        tokio::spawn(run(
            lp,
            project.github_repo.clone(),
            backlog_url.to_string(),
            keychain.clone(),
            paused.clone(),
        ));
    }
}

async fn run(
    mut lp: ProjectLoop,
    github_repo: String,
    backlog_url: String,
    keychain: Arc<dyn TokenStore>,
    paused: PauseState,
) {
    let mut every = tokio::time::interval(EVERY);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut repo: Option<u64> = None;
    loop {
        every.tick().await;
        if repo.is_none() {
            repo = repo_id(&github_repo).await;
        }
        let Some(repo_id) = repo else {
            tracing::warn!("{}: dry tick skipped: gh couldn't say the id of {github_repo}", lp.id);
            lp.cant_start(format!("gh couldn't say the id of {github_repo}"));
            continue;
        };
        let token = match keychain.get(&backlog_url) {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!("{}: dry tick skipped: reading the backlog link: {e:#}", lp.id);
                lp.cant_start(format!("reading the backlog link: {e:#}"));
                continue;
            }
        };
        let is_paused = paused.reason(&lp.id).is_some();
        lp.step(repo_id, token.as_deref(), is_paused).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::post};
    use std::sync::Mutex;

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

    fn project_loop(base: &str) -> (ProjectLoop, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::load(dir.path()).unwrap();
        let lp = ProjectLoop::new(
            "proj".into(),
            ProjectConfig::default(),
            store,
            2,
            EventLog::with_now(|| 0),
            Client::new(base),
            LiveHandle::new(OrchestratorMode::Dry),
        );
        (lp, dir)
    }

    #[tokio::test]
    async fn one_dry_tick_writes_a_tick_event_and_asks_the_store_only_to_list() {
        let (base, asked) = stub().await;
        let (mut lp, _dir) = project_loop(&base);
        let step = lp.step(7, Some("tok"), false).await;
        assert_eq!(
            step,
            Step::Ticked {
                would: vec!["ask_for_work".into()]
            }
        );
        assert_eq!(
            *asked.lock().unwrap(),
            vec!["list".to_string()],
            "no claim_next was sent"
        );
        assert_eq!(
            lp.events().recent().iter().map(|e| e.to_value()).collect::<Vec<_>>(),
            vec![json!({
                "at": "1970-01-01T00:00:00.000Z",
                "kind": "tick",
                "project": "proj",
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
        assert!(lp.runs.is_empty());
        assert_eq!(lp.pool.held_lanes().count(), 0, "a dry tick holds no lane");
    }

    #[tokio::test]
    async fn the_live_state_follows_each_pass() {
        let (base, _asked) = stub().await;
        let (mut lp, _dir) = project_loop(&base);
        let proj = |lp: &ProjectLoop| lp.live.snapshot().projects["proj"].clone();
        assert_eq!(proj(&lp), Default::default(), "registered before its first pass");

        lp.step(7, Some("tok"), false).await;
        let live = proj(&lp);
        assert!(live.last_tick.is_some());
        assert_eq!(live.decision.as_deref(), Some("would ask for work"));
        assert!(live.held.is_empty() && !live.paused && live.store_error.is_none());

        lp.step(7, None, true).await;
        let failed = proj(&lp);
        assert!(failed.store_error.unwrap().message.contains("no link"));
        assert_eq!(
            failed.last_tick, live.last_tick,
            "a failed pass leaves the last tick as it was"
        );

        lp.step(7, Some("tok"), true).await;
        let live = proj(&lp);
        assert!(live.paused && live.store_error.is_none());
        assert_eq!(live.decision.as_deref(), Some("paused: would do nothing"));
    }

    #[tokio::test]
    async fn a_paused_project_ticks_paused_and_would_do_nothing() {
        let (base, _asked) = stub().await;
        let (mut lp, _dir) = project_loop(&base);
        assert_eq!(lp.step(7, Some("tok"), true).await, Step::Ticked { would: vec![] });
        assert_eq!(lp.events().recent()[0].to_value()["status"], "paused");
    }

    #[tokio::test]
    async fn an_unreachable_backlog_is_a_store_error_and_no_tick() {
        // Port 1 on loopback refuses connections.
        let (mut lp, _dir) = project_loop("http://127.0.0.1:1");
        assert_eq!(lp.step(7, Some("tok"), false).await, Step::Skipped);
        let kinds: Vec<_> = lp.events().recent().iter().map(|e| e.kind()).collect();
        assert_eq!(kinds, vec!["store_error"]);
        assert_eq!(lp.events().recent()[0].to_value()["op"], "list");
    }

    #[tokio::test]
    async fn no_token_means_the_store_isnt_asked_and_the_tick_is_skipped() {
        let (base, asked) = stub().await;
        let (mut lp, _dir) = project_loop(&base);
        assert_eq!(lp.step(7, None, false).await, Step::Skipped);
        assert!(asked.lock().unwrap().is_empty());
        assert_eq!(lp.events().recent()[0].kind(), "store_error");
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
            let (lp, _dir) = project_loop(&base);
            let port = if blocked { ts_agent_port().await } else { closed_port() };
            let mut lp = lp.with_guard(Some(PortGuard::new(closed_port(), [port])));
            lp.mode = OrchestratorMode::Claim;
            assert_eq!(
                lp.step(7, Some("tok"), false).await,
                Step::Ticked {
                    would: vec!["ask_for_work".into()]
                }
            );
            assert_eq!(*asked.lock().unwrap(), vec!["list".to_string()]);
        }
    }
}
