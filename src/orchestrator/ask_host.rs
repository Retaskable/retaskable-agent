//! The real [`AskHost`]: the assistant session through `claude -p` (Claude only, like the drafter's host), read-only
//! in the project's base checkout. It keeps the day's spend of the sessions it ran, for the daily budget.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use futures_util::future::BoxFuture;

use super::driver::{AskHost, AskJob, Spend};
use super::session::{exists, quiet_timeout};
use crate::ask::model::AssistantResult;
use crate::ask::session::{Session, TEMPLATE, parse_result, session_spec};
use crate::config::{Engine, EngineChoiceConfig, ModelsConfig};
use crate::engines::resolve::resolve_claude_on_path;
use crate::engines::{Role, TranscriptSink, engine_for, engine_of, run_session};
use crate::env::Env;
use crate::implementer::new_session_id;
use crate::paths;
use crate::tiers::implementer_tier;

pub struct ClaudeAsks {
    data_dir: PathBuf,
    choice: EngineChoiceConfig,
    env: Env,
    models: ModelsConfig,
    daily_usd: f64,
    /// The UTC day (days since 1970) and what the sessions run on it cost.
    spent: Arc<Mutex<(i64, f64)>>,
}

impl ClaudeAsks {
    pub fn new(
        data_dir: PathBuf,
        choice: EngineChoiceConfig,
        env: Env,
        models: ModelsConfig,
        daily_usd: f64,
    ) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            choice,
            env,
            models,
            daily_usd,
            spent: Default::default(),
        })
    }
}

fn today() -> i64 {
    crate::telemetry::now_ms() / 86_400_000
}

impl AskHost for ClaudeAsks {
    fn run(&self, job: AskJob) -> BoxFuture<'static, Result<AssistantResult>> {
        let (data_dir, choice, env, models) = (
            self.data_dir.clone(),
            self.choice.clone(),
            self.env.clone(),
            self.models,
        );
        let spent = self.spent.clone();
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
                input: job.input(),
                engine: Engine::Claude,
                tier,
                budget: &project.budget,
                cwd: Path::new(&project.repo_path),
                local: None,
            })?;
            let session_id = new_session_id();
            let sink = TranscriptSink {
                path: paths::run_dir(&data_dir, &project.id_or_derived(), "ask").join(format!("{session_id}.jsonl")),
                lane: None,
            };
            let engine = engine_of(spec.engine)?;
            let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
            let r = run_session(engine.as_ref(), &bin, &spec, &session_id, Some(&sink), quiet).await?;
            if let Some(cost) = r.cost_usd {
                let mut spent = spent.lock().unwrap_or_else(|e| e.into_inner());
                if spent.0 != today() {
                    *spent = (today(), 0.0);
                }
                spent.1 += cost;
            }
            Ok(parse_result(r, project.budget.ask_usd))
        })
    }

    fn spend(&self) -> Spend {
        let spent = self.spent.lock().unwrap_or_else(|e| e.into_inner());
        Spend {
            today_usd: if spent.0 == today() { spent.1 } else { 0.0 },
            daily_usd: self.daily_usd,
        }
    }
}
