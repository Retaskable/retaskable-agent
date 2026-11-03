//! The ask step of a project's loop (docs/specs/agent/06.6-ask-claude.md, Interactive sessions): a chat message
//! the connector queued as an `ask` command is answered by one read-only assistant session, run through the
//! [`AskHost`], whose answer and each action's result go back to the store with `ask_report`. An ask has a human
//! waiting, so it never queues behind `concurrency`, but it counts toward it while it runs. It doesn't start while
//! paused (this computer's, or the project's on the store), over the daily budget or at the engine's usage limit:
//! the ask is reported waiting and answered once that lifts. A store that can't be read or asked leaves the ask
//! waiting in memory and answers nothing locally (fail closed). Behind the `ask-claude` release flag: Off is today's
//! behaviour, an ask is never answered.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio::task::JoinHandle;

use super::Shared;
use super::project::{ProjectLoop, signed_out};
use crate::ask::model::AssistantResult;
use crate::ask::perform::{Perform, perform};
use crate::ask::prompt::{AudienceLine, Exchange, StateDraft, StateItem, StateLane, StateSummary};
use crate::backlog::asks::{Ask, AskReport};
use crate::backlog::{BacklogError, Client, Item};
use crate::config::ProjectConfig;
use crate::engines::{Role, engine_for, program_of};
use crate::implementer::Interrupted;
use crate::lane_pool::{Holder, LanePool};
use crate::view::{AgentView, AskCommand};

/// What the store's chat shows an ask that can't start yet.
pub const WAITING: &str = "Paused: I'll answer when someone resumes";

/// What an assistant session needs: the prompt's inputs, read from the store and this agent's run state.
pub struct AskJob {
    pub project: ProjectConfig,
    pub ask_id: String,
    /// The message, with its "Attached images…" block after it when it has pictures.
    pub question: String,
    /// The ask this one replies to: its thread replaces the last five exchanges.
    pub reply_to: Option<String>,
    pub state: StateSummary,
    pub audiences: Vec<AudienceLine>,
    /// Every earlier ask in the chat, oldest first, this one left out.
    pub asks: Vec<Exchange>,
    /// The project's latest `state.usage` reading, for the model tier.
    pub usage: Option<Value>,
}

/// The parts of an ask that touch the machine: the engine. Faked in tests.
pub trait AskHost: Send + Sync {
    /// Runs the assistant in the project's base checkout. An error that downcasts to [`Interrupted`] is a restart or
    /// Quit cutting it; any other is told to the person as the answer.
    fn run(&self, job: AskJob) -> BoxFuture<'static, anyhow::Result<AssistantResult>>;
    /// What this computer spent today, in US dollars, against `budget.dailyUsd`.
    fn spent_today_usd(&self) -> f64;
}

/// What the ask step runs with: the host, the view the assistant's actions run their commands against, and the
/// computer's `budget.dailyUsd` (0 for none).
#[derive(Clone)]
pub struct AskRig {
    pub host: Arc<dyn AskHost>,
    pub view: Arc<AgentView>,
    pub daily_usd: f64,
}

/// A project's asks: the ones waiting to start, the ones already reported waiting, and the sessions running.
#[derive(Default)]
pub(super) struct Asking {
    pending: Vec<AskCommand>,
    reported: BTreeSet<String>,
    running: BTreeMap<String, JoinHandle<()>>,
}

impl Asking {
    pub(super) fn running(&mut self) -> u32 {
        self.running.retain(|_, t| !t.is_finished());
        self.running.len() as u32
    }
}

/// What the store told the pass before any ask starts.
struct Read {
    asks: Vec<Ask>,
    audiences: Vec<AudienceLine>,
    items: Vec<Item>,
    drafts: Vec<StateDraft>,
}

impl ProjectLoop {
    /// Starts every ask that can, and takes the sessions running here off `slots`. Asks start whatever `slots` holds.
    pub(super) async fn ask_pass(
        &mut self,
        slots: &mut u32,
        rig: &AskRig,
        pool: &LanePool,
        paused: bool,
        shared: &Shared<'_>,
    ) {
        let queued = rig.view.item_commands.take_asks(&self.id);
        if self.flags.on("ask-claude").await {
            self.asking.pending.extend(queued);
            self.serve_asks(rig, pool, paused, shared).await;
        } else {
            self.asking.pending.clear();
        }
        *slots = slots.saturating_sub(self.asking.running());
    }

    async fn serve_asks(&mut self, rig: &AskRig, pool: &LanePool, paused: bool, shared: &Shared<'_>) {
        if self.asking.pending.is_empty() || shared.stopping || signed_out(shared) {
            return;
        }
        let Some(repo) = self.repo else { return };
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        let engine = program_of(engine_for(
            &shared.links.engine_choice,
            self.project.engines.as_ref(),
            Role::Assistant,
            None,
        ));
        let spent = rig.host.spent_today_usd();
        let hold = paused
            || shared.engine_limits.is_limited(engine, Instant::now())
            || (rig.daily_usd > 0.0 && spent >= rig.daily_usd);
        let hold = match hold {
            true => true,
            false => match self.client.project_paused(repo, Some(&token)).await {
                Ok(paused) => paused,
                Err(e) => return self.store_error("pause_get", &e),
            },
        };
        if hold {
            return self.report_waiting(repo, &token).await;
        }
        let read = match read_for_asks(self.client.clone(), repo, &token).await {
            Ok(read) => read,
            Err((op, e)) => return self.store_error(op, &e),
        };
        for ask in std::mem::take(&mut self.asking.pending) {
            self.asking.reported.remove(&ask.ask_id);
            let job = self.ask_job(&ask, &read, pool, spent, rig.daily_usd);
            let ctx = Answer {
                client: self.client.clone(),
                repo,
                token: token.clone(),
                host: rig.host.clone(),
                view: rig.view.clone(),
                project: self.id.clone(),
                images: ask.images,
                items: read.items.clone(),
            };
            tracing::info!("{}: answering ask {}", self.id, ask.ask_id);
            let id = ask.ask_id;
            self.asking
                .running
                .insert(id.clone(), tokio::spawn(answer(ctx, id, job)));
        }
    }

    /// Tells the store each ask not yet reported that it waits. One the store doesn't take is told again next pass.
    async fn report_waiting(&mut self, repo: u64, token: &str) {
        let report = AskReport {
            waiting: Some(WAITING.into()),
            ..Default::default()
        };
        for id in self.asking.pending.iter().map(|a| a.ask_id.clone()).collect::<Vec<_>>() {
            if self.asking.reported.contains(&id) {
                continue;
            }
            match self.client.ask_report(repo, Some(token), &id, &report).await {
                Ok(()) => {
                    self.asking.reported.insert(id);
                }
                Err(e) => return self.store_error("ask_report", &e),
            }
        }
    }

    fn ask_job(&self, ask: &AskCommand, read: &Read, pool: &LanePool, spent: f64, daily: f64) -> AskJob {
        let question = match ask.images.is_empty() {
            true => ask.text.clone(),
            false => format!("{}\n\n{}", ask.text, ask.images),
        };
        let items = (read.items.iter())
            .map(|i| StateItem {
                id: i.id.clone(),
                title: i.title.clone(),
                state: (self.records.get(&i.id)).map_or_else(|| status_word(&i.status), |r| r.state.clone()),
                backlog_status: status_word(&i.status),
                lane: pool.lane_of(&self.id, &i.id),
                change_url: i.pr_url.clone(),
                needs_person: self.records.get(&i.id).and_then(|r| r.needs_human_reason.clone()),
            })
            .collect();
        let lanes = (1..=pool.rules().lanes)
            .map(|lane| StateLane {
                lane,
                item_id: match pool.holder(lane) {
                    Some(Holder::Item { project, item } | Holder::Play { project, item }) if *project == self.id => {
                        Some(item.clone())
                    }
                    _ => None,
                },
                running: self.playing.contains_key(&lane),
            })
            .collect();
        AskJob {
            project: self.project.clone(),
            ask_id: ask.ask_id.clone(),
            question,
            reply_to: ask.reply_to.clone(),
            state: StateSummary {
                about: ask.about.clone(),
                items,
                lanes,
                drafts: read.drafts.clone(),
                status: "running".into(),
                paused_reason: None,
                spent_today_usd: spent,
                daily_usd: daily,
            },
            audiences: read.audiences.clone(),
            asks: (read.asks.iter())
                .filter(|a| a.id != ask.ask_id)
                .map(|a| Exchange {
                    id: a.id.clone(),
                    text: a.text.clone(),
                    answer: a.answer.clone(),
                    reply_to: a.reply_to.clone(),
                    from_retaskable: a.from.as_deref() == Some("retaskable"),
                })
                .collect(),
            usage: self.store.usage(),
        }
    }
}

/// What the prompt's state and history are made from, read from the store: any failure is the whole read's.
async fn read_for_asks(client: Arc<Client>, repo: u64, token: &str) -> Result<Read, (&'static str, BacklogError)> {
    let token = Some(token);
    let asks = client.asks_list(repo, token).await.map_err(|e| ("asks_list", e))?;
    let audiences = client.audiences(repo, token).await.map_err(|e| ("facts_get", e))?;
    let items = client.list(repo, token).await.map_err(|e| ("list", e))?;
    let drafts = client.drafts(repo, token).await.map_err(|e| ("drafts", e))?;
    Ok(Read {
        asks,
        audiences: (audiences.into_iter())
            .map(|a| AudienceLine {
                id: a.id,
                name: a.name,
                who: a.short,
            })
            .collect(),
        items,
        drafts: (drafts.into_iter())
            .map(|d| StateDraft {
                state: if d.pr_url.is_some() {
                    "awaiting approval"
                } else {
                    "being written"
                }
                .into(),
                title: d.titles.join(", "),
                id: d.id,
            })
            .collect(),
    })
}

fn status_word(status: &crate::backlog::ItemStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// What one answering task owns.
struct Answer {
    client: Arc<Client>,
    repo: u64,
    token: String,
    host: Arc<dyn AskHost>,
    view: Arc<AgentView>,
    project: String,
    images: String,
    items: Vec<Item>,
}

/// One ask from session to report. A report the store doesn't take is only logged: the actions already ran.
async fn answer(ctx: Answer, id: String, job: AskJob) {
    let result = match ctx.host.run(job).await {
        Ok(result) => result,
        // Cut by a restart or Quit: the ask is the store's, and is asked again.
        Err(e) if e.downcast_ref::<Interrupted>().is_some() => return,
        Err(e) => AssistantResult {
            answer: format!("Sorry, I couldn't answer that: {e:#}."),
            actions: vec![],
            dropped: vec![],
        },
    };
    let results = perform(
        &result.actions,
        &result.dropped,
        &Perform {
            view: &ctx.view,
            project: &ctx.project,
            client: &ctx.client,
            repo_id: ctx.repo,
            token: Some(&ctx.token),
            ask_id: &id,
            images_block: &ctx.images,
            items: &ctx.items,
        },
    )
    .await;
    let report = AskReport {
        status: Some("done".into()),
        answer: Some(result.answer),
        actions: Some(results),
        ..Default::default()
    };
    if let Err(e) = ctx.client.ask_report(ctx.repo, Some(&ctx.token), &id, &report).await {
        tracing::warn!("an ask's answer didn't reach the store: {e}");
    }
}
