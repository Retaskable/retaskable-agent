//! The ask step of a project's loop (docs/specs/agent/06.6-ask-claude.md, "Interactive sessions"): a person's chat
//! message (the `ask` command, queued by `commands`) is answered by an assistant session. Someone is waiting, so an
//! ask doesn't queue behind `concurrency`, but it counts toward it while it runs. It doesn't start while paused (this
//! computer's, or the project's, read from the store), over the daily budget or at the usage limit: it is reported
//! waiting, "Paused: I'll answer when someone resumes", and starts once none holds. The store is read first for the
//! prompt (`asks_list`, audiences, items, drafts); a read that fails leaves the ask waiting and answers nothing
//! (fail closed). The session sits behind [`AskHost`]; its actions run through `ask::perform::perform`, and the
//! answer and each action's result go back with `ask_report`. Behind the `ask-claude` flag: Off takes no ask.
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio::task::JoinHandle;

use super::Shared;
use super::project::ProjectLoop;
use crate::ask::model::AssistantResult;
use crate::ask::perform::{Perform, perform};
use crate::ask::prompt::{AssistantInput, AudienceLine, Exchange, StateDraft, StateItem, StateLane, StateSummary};
use crate::backlog::asks::AskReport;
use crate::backlog::{Client, Item};
use crate::config::ProjectConfig;
use crate::engines::EngineId;
use crate::lane_pool::{Holder, LanePool};
use crate::view::{AgentView, Ask};

/// What an ask is told while it can't start.
pub const WAITING: &str = "Paused: I'll answer when someone resumes";

/// What the assistant session needs, all read already: the session itself touches nothing else of the store.
#[derive(Clone)]
pub struct AskJob {
    pub project: ProjectConfig,
    pub question: String,
    pub reply_to: Option<String>,
    /// Every earlier ask, oldest first, not including this one.
    pub asks: Vec<Exchange>,
    pub audiences: Vec<AudienceLine>,
    pub state: StateSummary,
    /// The project's latest `state.usage` reading, for the model tier.
    pub usage: Option<Value>,
}

impl AskJob {
    /// The prompt's inputs; for a reply the prompt shows the thread it continues instead of the last five.
    pub fn input(&self) -> AssistantInput<'_> {
        AssistantInput {
            project_type: self.project.project_type,
            base_branch: &self.project.base_branch,
            github_repo: &self.project.github_repo,
            pr: "PR",
            change: "pull request",
            state: &self.state,
            audiences: &self.audiences,
            asks: &self.asks,
            reply_to: self.reply_to.as_deref(),
            question: &self.question,
        }
    }
}

/// What this computer has spent today and its cap (`budget.dailyUsd`, 0 for none).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Spend {
    pub today_usd: f64,
    pub daily_usd: f64,
}

/// The part of an ask that touches the machine: the assistant session. Faked in tests.
pub trait AskHost: Send + Sync {
    /// Runs the assistant read-only in the project's base checkout.
    fn run(&self, job: AskJob) -> BoxFuture<'static, Result<AssistantResult>>;
    fn spend(&self) -> Spend;
}

/// The asks of one project: those waiting to start (and whether each was told so) and the sessions running.
#[derive(Default)]
pub(super) struct Asking {
    waiting: Vec<(Ask, bool)>,
    answering: Vec<JoinHandle<()>>,
}

impl ProjectLoop {
    /// How many ask sessions run here now; finished ones are forgotten. They count toward `concurrency`.
    pub(super) fn answering(&mut self) -> u32 {
        self.asking.answering.retain(|t| !t.is_finished());
        self.asking.answering.len() as u32
    }

    /// Takes in the asks the connector queued and starts every waiting one that may. `paused` is this computer's pause.
    pub(super) async fn ask_pass(
        &mut self,
        pool: &LanePool,
        paused: bool,
        host: &Arc<dyn AskHost>,
        view: &AgentView,
        shared: &Shared<'_>,
    ) {
        let on = self.flags.on("ask-claude").await;
        shared.links.item_commands.set_ask_on(&self.id, on);
        let queued = shared.links.item_commands.take_asks(&self.id);
        if !on {
            return; // Off is today's behaviour: no assistant, so the asks are not taken.
        }
        self.asking.waiting.extend(queued.into_iter().map(|a| (a, false)));
        if self.asking.waiting.is_empty() || shared.stopping {
            return;
        }
        let (Some(repo), Ok(Some(token))) = (self.repo, shared.links.keychain.get(&shared.links.backlog_url)) else {
            return;
        };
        let spend = host.spend();
        let at_limit = shared.engine_limits.is_limited(EngineId::Claude, Instant::now());
        let mut held = paused || at_limit || (spend.daily_usd > 0.0 && spend.today_usd >= spend.daily_usd);
        if !held {
            match self.client.project_paused(repo, Some(&token)).await {
                Ok(p) => held = p,
                Err(e) => return self.store_error("pause_get", &e),
            }
        }
        if held {
            return self.tell_waiting(repo, &token).await;
        }
        let read = match read_for_asks(&self.client, repo, &token).await {
            Ok(read) => read,
            Err(e) => return self.store_error("asks_list", &e),
        };
        for (ask, _) in std::mem::take(&mut self.asking.waiting) {
            let job = self.ask_job(&ask, &read, pool, spend);
            let (client, host, items, view) = (self.client.clone(), host.clone(), read.items.clone(), view.clone());
            let (project, token) = (self.id.clone(), token.clone());
            self.asking.answering.push(tokio::spawn(async move {
                let result = host.run(job).await.unwrap_or_else(|e| AssistantResult {
                    answer: format!("Sorry, I couldn't answer that: {e:#}"),
                    actions: vec![],
                    dropped: vec![],
                });
                let cx = Perform {
                    view: &view,
                    project: &project,
                    client: &client,
                    repo_id: repo,
                    token: Some(&token),
                    ask_id: &ask.id,
                    images_block: &ask.images_block,
                    items: &items,
                };
                let actions = perform(&result.actions, &result.dropped, &cx).await;
                let report = AskReport {
                    status: Some("done".into()),
                    answer: Some(result.answer),
                    actions: Some(actions),
                    ..Default::default()
                };
                if let Err(e) = client.ask_report(repo, Some(&token), &ask.id, &report).await {
                    tracing::warn!("{project}: an answer didn't reach the store: {e}");
                    // Retry once so a blip doesn't leave the ask waiting forever.
                    if let Err(e) = client.ask_report(repo, Some(&token), &ask.id, &report).await {
                        tracing::error!("{project}: an answer was lost, the ask stays waiting: {e}");
                    }
                }
            }));
        }
    }

    /// Reports each ask not yet told that it waits.
    async fn tell_waiting(&mut self, repo: u64, token: &str) {
        for (ask, told) in &mut self.asking.waiting {
            if *told {
                continue;
            }
            let report = AskReport {
                status: Some("waiting".into()),
                waiting: Some(WAITING.into()),
                ..Default::default()
            };
            match self.client.ask_report(repo, Some(token), &ask.id, &report).await {
                Ok(()) => *told = true,
                Err(e) => tracing::warn!("{}: an ask's waiting message didn't reach the store: {e}", self.id),
            }
        }
    }

    /// The prompt's inputs for `ask`: the store's reads and this loop's own run state.
    fn ask_job(&self, ask: &Ask, read: &Read, pool: &LanePool, spend: Spend) -> AskJob {
        let status = |item: &Item| {
            serde_json::to_value(item.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
        };
        let items = read.items.iter().map(|i| {
            let record = self.records.get(&i.id);
            StateItem {
                id: i.id.clone(),
                title: i.title.clone(),
                state: record.map_or_else(|| status(i).unwrap_or_default(), |r| r.state.clone()),
                backlog_status: status(i).unwrap_or_default(),
                lane: pool.lane_of(&self.id, &i.id),
                change_url: i.pr_url.clone(),
                needs_person: record.and_then(|r| r.needs_human_reason.clone()),
            }
        });
        let lanes = (1..=pool.limit()).map(|lane| {
            let item = match pool.holder(lane) {
                Some(Holder::Item { project, item }) if *project == self.id => Some(item.clone()),
                _ => None,
            };
            let running = item
                .as_ref()
                .is_some_and(|i| self.records.get(i).is_some_and(|r| r.state == "implementing"));
            StateLane {
                lane,
                item_id: item,
                running,
            }
        });
        let drafts = read.drafts.iter().map(|d| StateDraft {
            id: d.id.clone(),
            title: d.titles.join(", "),
            state: if d.pr_url.is_some() {
                "waiting for approval"
            } else {
                "being written"
            }
            .into(),
        });
        let exchanges = read.asks.iter().filter(|a| a.id != ask.id).map(|a| Exchange {
            id: a.id.clone(),
            text: a.text.clone(),
            answer: a.answer.clone(),
            reply_to: a.reply_to.clone(),
            from_retaskable: a.from.as_deref() == Some("retaskable"),
        });
        AskJob {
            project: self.project.clone(),
            question: ask.text.clone(),
            reply_to: ask.reply_to.clone(),
            asks: exchanges.collect(),
            audiences: (read.audiences.iter())
                .map(|a| AudienceLine {
                    id: a.id.clone(),
                    name: a.name.clone(),
                    who: a.short.clone(),
                })
                .collect(),
            state: StateSummary {
                about: ask.about.clone(),
                items: items.collect(),
                lanes: lanes.collect(),
                drafts: drafts.collect(),
                status: "running".into(),
                paused_reason: None,
                spent_today_usd: spend.today_usd,
                daily_usd: spend.daily_usd,
            },
            usage: self.store.usage(),
        }
    }
}

async fn read_for_asks(client: &Client, repo: u64, token: &str) -> Result<Read, crate::backlog::BacklogError> {
    let token = Some(token);
    Ok(Read {
        asks: client.asks_list(repo, token).await?,
        audiences: client.audiences(repo, token).await?,
        items: client.list(repo, token).await?,
        drafts: client.drafts(repo, token).await?,
    })
}

struct Read {
    asks: Vec<crate::backlog::asks::Ask>,
    audiences: Vec<crate::backlog::asks::Audience>,
    items: Vec<Item>,
    drafts: Vec<crate::backlog::Draft>,
}
