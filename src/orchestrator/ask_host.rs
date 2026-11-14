//! The real [`AskHost`]: reads the store for the assistant's prompt (`facts_get`, the items and the drafts) and the
//! agent's run state, runs the assistant session through the project's chosen engine in its base checkout, read-only,
//! and turns the finished session into the intent row's result (`ask::code_answer::answer_from`). The session's
//! process tree registers with `process_tree`, so a restart or Quit cuts it.
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;

use super::driver::{AskHost, AskJob, AskStore};
use super::session::watch_for;
use crate::ask::code_answer::answer_from;
use crate::ask::prompt::{AssistantInput, AudienceLine, StateDraft, StateItem, StateSummary};
use crate::ask::session::{Session, session_spec};
use crate::config::{CursorModels, Engine, EngineChoiceConfig};
use crate::engines::cursor::with_project;
use crate::engines::{Role, TranscriptSink, engine_for, engine_of, place, run_session, start_engine};
use crate::env::Env;
use crate::implementer::session::new_session_id;
use crate::intents::Outcome;
use crate::paths;
use crate::tiers::{SONNET, floor};
use crate::view::AgentView;

const TEMPLATE: &str = include_str!("../../prompts/assistant.md");

pub struct ClaudeAsks {
    data_dir: PathBuf,
    choice: EngineChoiceConfig,
    env: Env,
    cursor_models: CursorModels,
    /// The machine's `budget.dailyUsd`; 0 is no cap.
    daily_usd: f64,
    view: Arc<AgentView>,
    /// The computer's local model, for the tier ladder.
    local: crate::local_model::LocalModel,
}

impl ClaudeAsks {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        data_dir: PathBuf,
        choice: EngineChoiceConfig,
        env: Env,
        cursor_models: CursorModels,
        daily_usd: f64,
        view: Arc<AgentView>,
        local: crate::local_model::LocalModel,
    ) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            choice,
            env,
            cursor_models,
            daily_usd,
            view,
            local,
        })
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

    fn run(&self, job: AskJob) -> BoxFuture<'static, Outcome> {
        let (data_dir, choice, env, cursor_models, daily_usd, view, local) = (
            self.data_dir.clone(),
            self.choice.clone(),
            self.env.clone(),
            self.cursor_models.clone(),
            self.daily_usd,
            self.view.clone(),
            self.local.clone(),
        );
        Box::pin(async move {
            match ask(job, data_dir, choice, env, cursor_models, daily_usd, view, local).await {
                Ok(outcome) => outcome,
                // A store that can't be read leaves the row to fail: nothing is answered without it.
                Err(e) => Outcome::Failed(crate::text::clip(&format!("Couldn't read the project: {e:#}"), 280)),
            }
        })
    }
}

/// One code question from its store reads to the row's outcome. A session that can't start or run is a plain
/// reason the row fails with; an `Err` is the store not answering.
#[allow(clippy::too_many_arguments)]
async fn ask(
    job: AskJob,
    data_dir: PathBuf,
    choice: EngineChoiceConfig,
    env: Env,
    cursor_models: CursorModels,
    daily_usd: f64,
    view: Arc<AgentView>,
    local: crate::local_model::LocalModel,
) -> Result<Outcome> {
    let AskJob {
        project,
        id,
        question,
        images_block,
        store,
        lanes,
        runs,
        engine,
        sessions,
    } = job;
    let (client, repo, token) = (&store.client, store.repo, Some(store.token.as_str()));
    // Every read is the store's: one that fails leaves the ask waiting, nothing is answered without it.
    let audiences = client.audiences(repo, token).await?;
    let items = client.list(repo, token).await?;
    let drafts = client.drafts(repo, token).await?;
    let facts = client.project_facts(repo, token).await?;

    let audiences: Vec<AudienceLine> = (audiences.into_iter())
        .map(|a| AudienceLine {
            id: a.id,
            name: a.name,
            who: a.short,
        })
        .collect();
    let state = StateSummary {
        about: None,
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
        paused_reason: view.pause_reason(&project.id_or_derived()),
        spent_today_usd: facts.spent_today_usd,
        daily_usd,
    };
    let question = if images_block.is_empty() {
        question
    } else {
        format!("{question}\n\n{images_block}")
    };
    let sorry = |e: anyhow::Error| Ok(Outcome::Failed(crate::text::clip(&format!("{e:#}"), 280)));
    // The local model places as Claude Code with no local target, like `place` says.
    let chosen = match start_engine(
        engine,
        engine_for(&choice, project.engines.as_ref(), Role::Assistant, None),
    ) {
        Engine::Local => Engine::Claude,
        other => other,
    };
    let placed = match place(chosen, Role::Assistant, &env, &cursor_models) {
        Ok(placed) => placed,
        Err(why) => return Ok(Outcome::Failed(crate::text::clip(&why.to_string(), 280))),
    };
    let repo_path = PathBuf::from(&project.repo_path);
    let up = local.ladder_for(chosen == Engine::Claude);
    let tier = floor(SONNET, sessions.claude_at_limit(), up);
    // Held until the session ends.
    let claimed = local.claim(tier).await;
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
            asks: &[],
            reply_to: None,
            question: &question,
        },
        engine: chosen,
        tier,
        budget: &project.budget,
        cwd: &repo_path,
        local: claimed.as_ref().map(|(_, target)| target.clone()),
        cursor: placed.cursor.clone().map(|c| with_project(&project, c)),
    }) {
        Ok(spec) => spec,
        Err(e) => return sorry(e),
    };
    let project_id = project.id_or_derived();
    let sink = TranscriptSink {
        path: paths::run_dir(&data_dir, &project_id, &id).join("assistant.jsonl"),
        lane: None,
    };
    let engine = match engine_of(spec.engine) {
        Ok(engine) => engine,
        Err(e) => return sorry(e),
    };
    let quiet = watch_for(&project, spec.engine.unwrap_or_default());
    let listed = sessions.register_spec(&project_id, &id, None, Role::Assistant, &spec);
    let r = listed
        .scope(run_session(
            engine.as_ref(),
            &placed.bin,
            &spec,
            &new_session_id(),
            Some(&sink),
            quiet,
        ))
        .await;
    let r = match r {
        Ok(r) => r,
        Err(e) => return sorry(e),
    };
    if r.subtype == "interrupted" {
        // Cut by a restart: no word to the store, which opens the row again once the claim is stale.
        return Ok(Outcome::Abandoned);
    }
    Ok(match answer_from(&r, project.budget.ask_usd) {
        Ok(result) => Outcome::Done(result),
        Err(why) => Outcome::Failed(why),
    })
}
