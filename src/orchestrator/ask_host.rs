//! The real [`AskHost`]: the assistant session through `claude -p` (Claude only, like the drafter's host), read-only,
//! in the project's base checkout. The session's process tree registers with `process_tree`, so a restart or Quit
//! cuts it.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures_util::future::BoxFuture;

use super::driver::{AskHost, AskJob};
use super::session::{exists, quiet_timeout};
use crate::ask::model::AssistantResult;
use crate::ask::prompt::AssistantInput;
use crate::ask::session::{Session, parse_result, session_spec};
use crate::config::{Engine, EngineChoiceConfig, ModelsConfig};
use crate::engines::resolve::resolve_claude_on_path;
use crate::engines::{Role, TranscriptSink, engine_for, engine_of, run_session};
use crate::env::Env;
use crate::implementer::{Interrupted, new_session_id};
use crate::paths;
use crate::tiers::implementer_tier;

const TEMPLATE: &str = include_str!("../../prompts/assistant.md");

pub struct ClaudeAsks {
    data_dir: PathBuf,
    choice: EngineChoiceConfig,
    env: Env,
    models: ModelsConfig,
}

impl ClaudeAsks {
    pub fn new(data_dir: PathBuf, choice: EngineChoiceConfig, env: Env, models: ModelsConfig) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            choice,
            env,
            models,
        })
    }
}

impl AskHost for ClaudeAsks {
    fn run(&self, job: AskJob) -> BoxFuture<'static, Result<AssistantResult>> {
        let (data_dir, choice, env, models) = (
            self.data_dir.clone(),
            self.choice.clone(),
            self.env.clone(),
            self.models,
        );
        Box::pin(async move {
            let project = &job.project;
            if engine_for(&choice, project.engines.as_ref(), Role::Assistant, None) != Engine::Claude {
                return Err(anyhow!("only Claude sessions run in the Rust agent yet"));
            }
            let bin = resolve_claude_on_path(&env)?;
            if !exists(&bin, &env) {
                return Err(anyhow!("the claude binary wasn't found ({})", bin.display()));
            }
            let tier = implementer_tier(job.usage.as_ref(), crate::telemetry::now_ms() / 1000, &models);
            let spec = session_spec(&Session {
                template: TEMPLATE,
                input: AssistantInput {
                    project_type: project.project_type,
                    base_branch: &project.base_branch,
                    github_repo: &project.github_repo,
                    pr: "PR",
                    change: "pull request",
                    state: &job.state,
                    audiences: &job.audiences,
                    asks: &job.asks,
                    reply_to: job.reply_to.as_deref(),
                    question: &job.question,
                },
                engine: Engine::Claude,
                tier,
                budget: &project.budget,
                cwd: Path::new(&project.repo_path),
                local: None,
            })?;
            let sink = TranscriptSink {
                path: paths::run_dir(&data_dir, &project.id_or_derived(), &job.ask_id).join("assistant.jsonl"),
                lane: None,
            };
            let engine = engine_of(spec.engine)?;
            let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
            let r = run_session(engine.as_ref(), &bin, &spec, &new_session_id(), Some(&sink), quiet).await?;
            if r.subtype == "interrupted" {
                return Err(anyhow::Error::new(Interrupted(
                    "the assistant was cut by a restart".into(),
                )));
            }
            Ok(parse_result(r, project.budget.ask_usd))
        })
    }

    /// 0 until this agent keeps a day's spend, so the daily budget never holds an ask here yet.
    fn spent_today_usd(&self) -> f64 {
        0.0
    }
}
