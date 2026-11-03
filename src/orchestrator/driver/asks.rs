//! The ask step of a project's loop (docs/specs/agent/06.6-ask-claude.md, "Interactive sessions"): the chat's
//! `ask` commands queued by `commands::ask` are answered here, one `AskHost` session each. An ask has a person
//! waiting, so it doesn't queue behind `concurrency` but counts toward it while it runs. It doesn't start while
//! this computer or the project is paused, over the daily budget, at the engine's usage limit, signed out or while
//! the agent is stopping: it is reported waiting ("Paused: I'll answer when someone resumes") and answered once
//! that passes. The store keeps the conversation; the loop keeps only the asks waiting for a session and the tasks
//! running. A store that can't be reached fails closed: the ask goes back in the queue, nothing is answered locally.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio::task::JoinHandle;

use super::Shared;
use super::project::{ProjectLoop, signed_out};
use crate::ask::prompt::StateLane;
use crate::backlog::Client;
use crate::backlog::asks::AskReport;
use crate::config::ProjectConfig;
use crate::engines::{Role, engine_for, program_of};
use crate::lane_pool::{Holder, LanePool};
use crate::store::RunRecord;
use crate::view::AskCommand;

/// What an ask's `ask_report` says while it waits.
pub const WAITING: &str = "Paused: I'll answer when someone resumes";

/// The store as the ask reads and writes it.
#[derive(Clone)]
pub struct AskStore {
    pub client: Arc<Client>,
    pub repo: u64,
    pub token: String,
}

/// What an assistant session needs besides the store: the agent's own run state, for the prompt.
pub struct AskJob {
    pub project: ProjectConfig,
    pub ask: AskCommand,
    pub store: AskStore,
    /// The project's latest `state.usage` reading, for the session's model tier.
    pub usage: Option<Value>,
    /// The lanes this project holds.
    pub lanes: Vec<StateLane>,
    /// The runs this computer holds, by item id.
    pub runs: BTreeMap<String, RunRecord>,
}

/// The answer and what each action it asked for came to (`perform`'s results).
#[derive(Debug, Clone, PartialEq)]
pub struct AskReply {
    pub answer: String,
    pub actions: Vec<Value>,
}

/// The parts of an ask that touch the store, the machine and the engine. Faked in tests.
pub trait AskHost: Send + Sync {
    /// Whether the store says new asks must wait: the project is paused, or today's spend is over the daily budget.
    /// An `Err` is the store not answering.
    fn blocked(&self, store: &AskStore) -> BoxFuture<'static, Result<bool>>;
    /// Reads the store for the prompt, runs the assistant session in the base checkout and carries out its actions.
    /// A session that fails is an answer saying so; an `Err` is the store not answering, which leaves the ask waiting.
    fn run(&self, job: AskJob) -> BoxFuture<'static, Result<AskReply>>;
}

/// A project's asks: those waiting for a session, which of them the store was told so, and those running.
#[derive(Default)]
pub(super) struct Asks {
    waiting: Vec<AskCommand>,
    reported: BTreeSet<String>,
    running: Vec<RunningAsk>,
}

/// An ask whose session is running.
pub(super) struct RunningAsk {
    task: JoinHandle<()>,
}

impl ProjectLoop {
    /// How many asks are running here now; finished ones are forgotten.
    pub(super) fn asking(&mut self) -> u32 {
        self.asks.running.retain(|a| !a.task.is_finished());
        self.asks.running.len() as u32
    }

    /// Takes in the asks queued for this project and starts each one that may run, or reports it waiting. Behind the
    /// `ask-claude` flag: Off is recorded for the command, which refuses `ask`.
    pub(super) async fn ask_pass(
        &mut self,
        paused: bool,
        pool: &LanePool,
        slots: &mut u32,
        host: &Arc<dyn AskHost>,
        shared: &Shared<'_>,
    ) {
        let on = self.flags.on("ask-claude").await;
        shared.links.item_commands.set_ask_on(&self.id, on);
        let queued = shared.links.item_commands.take_asks(&self.id);
        if !on {
            return; // Off is today's behaviour: `ask` is refused as a command this agent doesn't take.
        }
        self.asks.waiting.extend(queued);
        if self.asks.waiting.is_empty() || shared.stopping {
            return;
        }
        let Some(repo) = self.repo else { return };
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        let store = || AskStore {
            client: self.client.clone(),
            repo,
            token: token.clone(),
        };
        let engine = program_of(engine_for(
            &shared.links.engine_choice,
            self.project.engines.as_ref(),
            Role::Assistant,
            None,
        ));
        let mut hold =
            paused || signed_out(shared) || shared.engine_limits.is_limited(engine, std::time::Instant::now());
        if !hold {
            hold = match host.blocked(&store()).await {
                Ok(blocked) => blocked,
                Err(e) => return tracing::warn!("{}: the ask gate couldn't read the store: {e:#}", self.id),
            };
        }
        let waiting = std::mem::take(&mut self.asks.waiting);
        if hold {
            for ask in &waiting {
                if !self.asks.reported.contains(&ask.ask_id) {
                    let report = AskReport {
                        status: Some("waiting".into()),
                        waiting: Some(WAITING.into()),
                        ..Default::default()
                    };
                    if send(&store(), &ask.ask_id, &report).await {
                        self.asks.reported.insert(ask.ask_id.clone());
                    }
                }
            }
            self.asks.waiting = waiting;
            return;
        }
        for ask in waiting {
            self.asks.reported.remove(&ask.ask_id);
            *slots = slots.saturating_sub(1);
            let job = AskJob {
                project: self.project.clone(),
                ask,
                store: store(),
                usage: self.store.usage(),
                lanes: self.ask_lanes(pool),
                runs: self.records.clone(),
            };
            let (host, queue) = (host.clone(), shared.links.item_commands.clone());
            tracing::info!("{}: answering ask {}", self.id, job.ask.ask_id);
            self.asks.running.push(RunningAsk {
                task: tokio::spawn(answer(host, job, queue)),
            });
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

/// One ask from start to report. A store that fails before the session puts the ask back in the queue; once the
/// session has run and its actions are done the reply is kept and only the report is retried, never the session.
async fn answer(host: Arc<dyn AskHost>, job: AskJob, queue: crate::view::ItemCommands) {
    let (store, ask) = (job.store.clone(), job.ask.clone());
    let reply = match host.run(job).await {
        Ok(reply) => reply,
        Err(e) => {
            tracing::warn!("ask {}: the store failed, it stays waiting: {e:#}", ask.ask_id);
            return queue.push_ask(ask);
        }
    };
    let done = AskReport {
        status: Some("answered".into()),
        answer: Some(reply.answer),
        actions: Some(reply.actions),
        ..Default::default()
    };
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        if send(&store, &ask.ask_id, &done).await {
            return;
        }
    }
    tracing::error!(
        "ask {}: the answer never reached the store; it isn't run again",
        ask.ask_id
    );
}

async fn send(store: &AskStore, id: &str, report: &AskReport) -> bool {
    match store
        .client
        .ask_report(store.repo, Some(&store.token), id, report)
        .await
    {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("ask {id}: the report didn't reach the store: {e}");
            false
        }
    }
}
