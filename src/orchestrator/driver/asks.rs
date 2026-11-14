//! The ask step of a project's loop (docs/specs/agent/06.6-ask-claude.md, "Code questions"): the `ask` intent rows
//! that are code questions, pulled from the store by the intent puller (`intents/mod.rs`) and answered here, one
//! `AskHost` session each. A question has a person waiting, so it doesn't queue behind `concurrency` but counts toward
//! it while it runs. It isn't taken while this computer or the project is paused, over the daily budget, at the
//! engine's usage limit, signed out or while the agent is stopping: the row stays open and is taken once that passes
//! (the store times it out after 5 minutes unclaimed). A store that can't be reached takes nothing: nothing is
//! answered locally.
use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;

use super::Shared;
use super::project::ProjectLoop;
use super::start_gate::StartGate;
use crate::ask::code_answer::question_of;
use crate::ask::pictures;
use crate::ask::prompt::StateLane;
use crate::backlog::Client;
use crate::backlog::intents::IntentRow;
use crate::config::ProjectConfig;
use crate::engines::Role;
use crate::intents::{Handler, IntentStore, Outcome, Puller, StoreLink};
use crate::lane_pool::{Holder, LanePool};
use crate::store::RunRecord;

/// The store as the ask reads it.
#[derive(Clone)]
pub struct AskStore {
    pub client: Arc<Client>,
    pub repo: u64,
    pub token: String,
    /// Where a failed write is kept for the loop to log.
    pub failures: crate::backlog::failures::Failures,
}

/// What an assistant session needs besides the store: the agent's own run state, for the prompt.
pub struct AskJob {
    pub project: ProjectConfig,
    /// The intent row's id.
    pub id: String,
    pub question: String,
    /// The "Attached images" block made from the row's pictures, empty when there are none.
    pub images_block: String,
    pub store: AskStore,
    /// The lanes this project holds.
    pub lanes: Vec<StateLane>,
    /// The runs this computer holds, by item id.
    pub runs: BTreeMap<String, RunRecord>,
    /// The engine to start on when it isn't the assistant's chosen one.
    pub engine: Option<crate::engines::EngineId>,
    /// The sessions running now: this one is listed while it runs.
    pub sessions: crate::orchestrator::live::sessions::Sessions,
}

/// The parts of an ask that touch the store, the machine and the engine. Faked in tests.
pub trait AskHost: Send + Sync {
    /// Whether the store says new asks must wait: the project is paused, or today's spend is over the daily budget.
    /// An `Err` is the store not answering.
    fn blocked(&self, store: &AskStore) -> BoxFuture<'static, Result<bool>>;
    /// Reads the store for the prompt and runs the assistant session in the base checkout, read-only. `Done` carries
    /// the row's `{ answer }`; a session that fails is `Failed` with its plain reason; one cut by a restart is
    /// `Abandoned`.
    fn run(&self, job: AskJob) -> BoxFuture<'static, Outcome>;
}

/// What the ask handler knows when the puller takes a row.
pub(super) struct AskCx {
    project: ProjectConfig,
    store: AskStore,
    lanes: Vec<StateLane>,
    runs: BTreeMap<String, RunRecord>,
    engine: Option<crate::engines::EngineId>,
    sessions: crate::orchestrator::live::sessions::Sessions,
}

/// The `ask` kind: a code question, answered by a read-only session with the pictures it came with.
struct AskHandler {
    host: Arc<dyn AskHost>,
    /// Where pictures are saved.
    pictures: std::path::PathBuf,
}

impl Handler<AskCx> for AskHandler {
    fn kind(&self) -> &'static str {
        "ask"
    }

    fn takes(&self, row: &IntentRow) -> bool {
        question_of(&row.kind, &row.args).is_some()
    }

    fn run(&self, row: IntentRow, cx: &AskCx) -> BoxFuture<'static, Outcome> {
        let host = self.host.clone();
        let dir = self.pictures.clone();
        let job = AskJob {
            project: cx.project.clone(),
            id: row.id.clone(),
            question: question_of(&row.kind, &row.args).unwrap_or_default().to_string(),
            images_block: String::new(),
            store: cx.store.clone(),
            lanes: cx.lanes.clone(),
            runs: cx.runs.clone(),
            engine: cx.engine,
            sessions: cx.sessions.clone(),
        };
        async move {
            let ids = pictures::ids_of(&row.args);
            let store = &job.store;
            let saved = match pictures::fetch(&store.client, store.repo, Some(&store.token), &ids, &dir).await {
                Ok(saved) => saved,
                Err(why) => return Outcome::Failed(why),
            };
            // `saved` lives until the session is done: the pictures are removed after the run.
            let outcome = host
                .run(AskJob {
                    images_block: saved.block(),
                    ..job
                })
                .await;
            drop(saved);
            outcome
        }
        .boxed()
    }
}

/// How often the store is asked for new questions (docs/specs/intent-rows.md section 3.3).
const POLL_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// A project's asks: the puller, built the first time a pass has a host.
pub(super) struct Asks {
    puller: Option<Puller<AskCx>>,
    polled: Option<std::time::Instant>,
    /// How long after one look at the store the next may be.
    pub(super) poll_every: std::time::Duration,
}

impl Default for Asks {
    fn default() -> Self {
        Self {
            puller: None,
            polled: None,
            poll_every: POLL_EVERY,
        }
    }
}

impl ProjectLoop {
    /// How many asks are running here now; finished ones are forgotten.
    pub(super) fn asking(&mut self) -> u32 {
        self.drain_failures();
        self.asks.puller.as_mut().map_or(0, |p| p.active())
    }

    /// Takes the code questions the store lists for this project and starts each one that may run.
    pub(super) async fn ask_pass(
        &mut self,
        paused: bool,
        pool: &LanePool,
        slots: &mut u32,
        host: &Arc<dyn AskHost>,
        shared: &Shared<'_>,
    ) {
        if shared.stopping {
            return;
        }
        let Some(repo) = self.repo else { return };
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        let failures = self.failures.clone();
        let store = AskStore {
            client: self.client.clone(),
            repo,
            token: token.clone(),
            failures: failures.clone(),
        };
        let gate = StartGate::for_lanes(self, pool.rules().lanes, shared);
        let (hold, start_on) = match gate.role_start(self, shared, Role::Assistant, true) {
            Some(engine) => (paused, engine),
            None => (true, None),
        };
        if hold {
            return;
        }
        if self.asks.polled.is_some_and(|at| at.elapsed() < self.asks.poll_every) {
            return;
        }
        self.asks.polled = Some(std::time::Instant::now());
        match host.blocked(&store).await {
            Ok(false) => {}
            Ok(true) => return,
            Err(e) => return tracing::warn!("{}: the ask gate couldn't read the store: {e:#}", self.id),
        }
        let link: Arc<dyn IntentStore> = Arc::new(StoreLink {
            client: self.client.clone(),
            repo,
            token,
            machine: shared.links.machine.clone(),
            failures,
        });
        let cx = AskCx {
            project: self.project.clone(),
            store,
            lanes: self.ask_lanes(pool),
            runs: self.records.clone(),
            engine: start_on,
            sessions: self.live.sessions().clone(),
        };
        let puller = self.asks.puller.get_or_insert_with(|| {
            Puller::default().register(Arc::new(AskHandler {
                host: host.clone(),
                pictures: pictures::folder(),
            }))
        });
        let started = puller.pass(&link, || cx).await;
        *slots = slots.saturating_sub(started);
        if started > 0 {
            tracing::info!("{}: answering {started} code question(s)", self.id);
        }
    }

    fn ask_lanes(&self, pool: &LanePool) -> Vec<StateLane> {
        (1..=pool.rules().lanes)
            .filter_map(|lane| match pool.holder(lane)? {
                Holder::Item { project, item } | Holder::Play { project, item } if *project == self.id => {
                    Some(StateLane {
                        lane,
                        item_id: Some(item.clone()),
                        running: self.records.get(item).is_some_and(|r| r.state == "implementing"),
                    })
                }
                _ => None,
            })
            .collect()
    }
}
