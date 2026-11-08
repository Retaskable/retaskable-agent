//! The real [`AskHost`]: reads the store for the assistant's prompt (`asks_list`, `facts_get`, the items and the
//! drafts) and the agent's run state, runs the assistant session through the engine's CLI in the project's base checkout
//! (on the assistant's chosen engine with `engine-placement` On, else Claude), carries out its actions with `ask::perform` and hands back the answer
//! and each action's result for the loop to send with `ask_report`. The session's process tree registers with
//! `process_tree`, so a restart or Quit cuts it.
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;

use super::driver::{AskHost, AskJob, AskReply, AskStore};
use super::session::quiet_timeout;
use crate::ask::perform::{Perform, perform};
use crate::ask::prompt::{AssistantInput, AudienceLine, Exchange, StateDraft, StateItem, StateSummary};
use crate::ask::session::{Session, parse_result, session_spec};
use crate::backlog::asks::Ask;
use crate::config::{CursorModels, Engine, EngineChoiceConfig, ModelsConfig};
use crate::engines::{Role, TranscriptSink, engine_for, engine_of, place, run_session};
use crate::env::Env;
use crate::implementer::Interrupted;
use crate::implementer::session::new_session_id;
use crate::paths;
use crate::tiers::implementer_tier;
use crate::view::AgentView;

const TEMPLATE: &str = include_str!("../../prompts/assistant.md");

pub struct ClaudeAsks {
    data_dir: PathBuf,
    choice: EngineChoiceConfig,
    env: Env,
    models: ModelsConfig,
    cursor_models: CursorModels,
    /// The machine's `budget.dailyUsd`; 0 is no cap.
    daily_usd: f64,
    view: Arc<AgentView>,
}

impl ClaudeAsks {
    pub fn new(
        data_dir: PathBuf,
        choice: EngineChoiceConfig,
        env: Env,
        models: ModelsConfig,
        cursor_models: CursorModels,
        daily_usd: f64,
        view: Arc<AgentView>,
    ) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            choice,
            env,
            models,
            cursor_models,
            daily_usd,
            view,
        })
    }
}

fn exchange(a: &Ask) -> Exchange {
    Exchange {
        id: a.id.clone(),
        text: a.text.clone(),
        answer: a.answer.clone(),
        reply_to: a.reply_to.clone(),
        from_retaskable: a.from.as_deref() == Some("retaskable"),
    }
}

fn status_word(status: crate::backlog::ItemStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

impl AskHost for ClaudeAsks {
    fn blocked(&self, store: &AskStore) -> BoxFuture<'static, Result<bool>> {
        let (client, repo, token, daily) = (store.client.clone(), store.repo, store.token.clone(), self.daily_usd);
        Box::pin(async move {
            let facts = client.project_facts(repo, Some(&token)).await?;
            Ok(facts.paused || (daily > 0.0 && facts.spent_today_usd >= daily))
        })
    }

    fn run(&self, job: AskJob) -> BoxFuture<'static, Result<AskReply>> {
        let (data_dir, choice, env, models, cursor_models, daily_usd, view) = (
            self.data_dir.clone(),
            self.choice.clone(),
            self.env.clone(),
            self.models,
            self.cursor_models.clone(),
            self.daily_usd,
            self.view.clone(),
        );
        Box::pin(async move {
            let AskJob {
                project,
                ask,
                store,
                usage,
                lanes,
                runs,
                events,
                placement,
            } = job;
            let (client, repo, token) = (&store.client, store.repo, Some(store.token.as_str()));
            // Every read is the store's: one that fails leaves the ask waiting, nothing is answered without it.
            let asks = client.asks_list(repo, token).await?;
            let audiences = client.audiences(repo, token).await?;
            let items = client.list(repo, token).await?;
            let drafts = client.drafts(repo, token).await?;
            let facts = client.project_facts(repo, token).await?;

            let history: Vec<Exchange> = asks.iter().filter(|a| a.id != ask.ask_id).map(exchange).collect();
            let audiences: Vec<AudienceLine> = (audiences.into_iter())
                .map(|a| AudienceLine {
                    id: a.id,
                    name: a.name,
                    who: a.short,
                })
                .collect();
            let state = StateSummary {
                about: ask.about.clone(),
                items: (items.iter())
                    .map(|i| {
                        let run = runs.get(&i.id);
                        StateItem {
                            id: i.id.clone(),
                            title: i.title.clone(),
                            state: run.map_or_else(|| status_word(i.status), |r| r.state.clone()),
                            backlog_status: status_word(i.status),
                            lane: run.and_then(|r| r.lane),
                            change_url: i.pr_url.clone(),
                            needs_person: run.and_then(|r| r.needs_human_reason.clone()),
                        }
                    })
                    .collect(),
                lanes,
                drafts: (drafts.iter())
                    .map(|d| StateDraft {
                        id: d.id.clone(),
                        title: d.titles.first().cloned().unwrap_or_default(),
                        state: if d.pr_url.is_some() {
                            "awaiting approval"
                        } else {
                            "being written"
                        }
                        .into(),
                    })
                    .collect(),
                status: "running".into(),
                paused_reason: None,
                spent_today_usd: facts.spent_today_usd,
                daily_usd,
            };
            let question = if ask.images_block.is_empty() {
                ask.text.clone()
            } else {
                format!("{}\n\n{}", ask.text, ask.images_block)
            };
            let reply = |answer: String| AskReply {
                answer,
                actions: vec![],
            };
            let sorry = |e: anyhow::Error| reply(format!("Sorry, I couldn't answer that: {e:#}"));
            // With `engine-placement` Off, only Claude answers, as before.
            let chosen = if placement {
                engine_for(&choice, project.engines.as_ref(), Role::Assistant, None)
            } else {
                Engine::Claude
            };
            let placed = match place(chosen, Role::Assistant, &env, &cursor_models) {
                Ok(placed) => placed,
                Err(e) => return Ok(sorry(anyhow::Error::msg(e))),
            };
            let repo_path = PathBuf::from(&project.repo_path);
            let tier = implementer_tier(usage.as_ref(), crate::telemetry::now_ms() / 1000, &models);
            let spec = match session_spec(&Session {
                template: TEMPLATE,
                input: AssistantInput {
                    project_type: project.project_type,
                    base_branch: &project.base_branch,
                    github_repo: &project.github_repo,
                    pr: "PR",
                    change: "pull request",
                    state: &state,
                    audiences: &audiences,
                    asks: &history,
                    reply_to: ask.reply_to.as_deref(),
                    question: &question,
                },
                engine: chosen,
                tier,
                budget: &project.budget,
                cwd: &repo_path,
                local: None,
                cursor: placed.cursor.clone(),
            }) {
                Ok(spec) => spec,
                Err(e) => return Ok(sorry(e)),
            };
            let project_id = project.id_or_derived();
            let sink = TranscriptSink {
                path: paths::run_dir(&data_dir, &project_id, &ask.ask_id).join("assistant.jsonl"),
                lane: None,
            };
            let engine = match engine_of(spec.engine) {
                Ok(engine) => engine,
                Err(e) => return Ok(sorry(e)),
            };
            let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
            let follower = follow(sink.path.clone(), engine_of(spec.engine)?, events);
            let r = run_session(
                engine.as_ref(),
                &placed.bin,
                &spec,
                &new_session_id(),
                Some(&sink),
                quiet,
            )
            .await;
            follower.abort();
            let r = match r {
                Ok(r) => r,
                Err(e) => return Ok(sorry(e)),
            };
            if r.subtype == "interrupted" {
                return Err(anyhow::Error::new(Interrupted(
                    "the assistant was cut by a restart".into(),
                )));
            }
            let result = parse_result(r, project.budget.ask_usd);
            let results = perform(
                &result.actions,
                &result.dropped,
                &Perform {
                    view: &view,
                    project: &project_id,
                    client,
                    repo_id: repo,
                    token,
                    ask_id: &ask.ask_id,
                    images_block: &ask.images_block,
                    items: &items,
                },
            )
            .await;
            Ok(AskReply {
                answer: result.answer,
                actions: results,
            })
        })
    }
}

/// Tails the session's transcript, sending each line's summarized events (`Engine::summarize`) as they are written,
/// for the loop to report as progress. Stops when aborted.
fn follow(
    path: PathBuf,
    engine: Box<dyn crate::engines::Engine>,
    events: tokio::sync::mpsc::UnboundedSender<crate::engines::SessionEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut read = 0usize;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let text = String::from_utf8_lossy(&bytes);
            let Some(end) = text.rfind('\n') else { continue };
            if end < read {
                continue;
            }
            for line in text[read..end].lines() {
                if let Ok(ev) = serde_json::from_str::<serde_json::Value>(line) {
                    for event in engine.summarize(&ev, crate::telemetry::now_ms()) {
                        let _ = events.send(event);
                    }
                }
            }
            read = end + 1;
        }
    })
}
