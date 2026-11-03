//! The real [`DraftHost`]: a draft's worktree through `draft_worktree`, the drafter session through
//! `claude -p` (Claude only, like the implementer's starter), and the PR through `publish`.
//! The session's process tree registers with `process_tree`, so a restart or Quit cuts it.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures_util::future::BoxFuture;

use super::driver::{DraftHost, DraftJob};
use super::session::{exists, quiet_timeout};
use crate::config::{Engine, EngineChoiceConfig, ModelsConfig, ProjectConfig};
use crate::drafts::session::{Session, Start, parse_result, session_spec};
use crate::drafts::{DrafterResult, titles_text};
use crate::engines::resolve::resolve_claude_on_path;
use crate::engines::{Role, TranscriptSink, engine_for, engine_of, run_session};
use crate::env::Env;
use crate::implementer::{Interrupted, publish};
use crate::tiers::implementer_tier;
use crate::{draft_worktree, paths};

const TEMPLATE: &str = include_str!("../../prompts/drafter.md");

pub struct ClaudeDrafts {
    data_dir: PathBuf,
    choice: EngineChoiceConfig,
    env: Env,
    models: ModelsConfig,
}

impl ClaudeDrafts {
    pub fn new(data_dir: PathBuf, choice: EngineChoiceConfig, env: Env, models: ModelsConfig) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            choice,
            env,
            models,
        })
    }
}

impl DraftHost for ClaudeDrafts {
    fn prepare(&self, project: &ProjectConfig, id: &str, branch: &str) -> Result<PathBuf> {
        draft_worktree::create(
            Path::new(&project.repo_path),
            Path::new(&project.worktrees_dir),
            id,
            branch,
            &project.base_branch,
        )
    }

    fn run(&self, job: DraftJob) -> BoxFuture<'static, Result<DrafterResult>> {
        let (data_dir, choice, env, models) = (
            self.data_dir.clone(),
            self.choice.clone(),
            self.env.clone(),
            self.models,
        );
        Box::pin(async move {
            let project = &job.project;
            if engine_for(&choice, project.engines.as_ref(), Role::Drafter, None) != Engine::Claude {
                return Err(anyhow!("only Claude sessions run in the Rust agent yet"));
            }
            let bin = resolve_claude_on_path(&env)?;
            if !exists(&bin, &env) {
                return Err(anyhow!("the claude binary wasn't found ({})", bin.display()));
            }
            let draft = &job.draft;
            let themes = draft.intake.as_ref().map(|i| i.themes.clone()).unwrap_or_default();
            let titles = titles_text(&draft.titles, &themes);
            let tier = implementer_tier(job.usage.as_ref(), crate::telemetry::now_ms() / 1000, &models);
            let spec = session_spec(
                &Session {
                    template: TEMPLATE,
                    request: draft.request.as_deref().unwrap_or(""),
                    titles: &titles,
                    branch: &job.branch,
                    base_branch: &project.base_branch,
                    engine: Engine::Claude,
                    tier,
                    budget: &project.budget,
                    cwd: &job.dir,
                    local: None,
                },
                if job.resume {
                    Start::Resume {
                        session_id: job.session_id.clone(),
                        follow_up: &job.follow_up,
                        decisions: &[],
                    }
                } else {
                    Start::Fresh
                },
            )?;
            let sink = TranscriptSink {
                path: paths::run_dir(&data_dir, &project.id_or_derived(), &draft.id).join("drafter.jsonl"),
                lane: None,
            };
            let engine = engine_of(spec.engine)?;
            let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
            let r = run_session(engine.as_ref(), &bin, &spec, &job.session_id, Some(&sink), quiet).await?;
            if r.subtype == "interrupted" {
                return Err(anyhow::Error::new(Interrupted(
                    "the drafter was cut by a restart".into(),
                )));
            }
            parse_result(r, project.budget.drafter_usd)
        })
    }

    fn publish(
        &self,
        project: &ProjectConfig,
        dir: &Path,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<publish::Published> {
        let gh = self
            .env
            .get("RETASKABLE_GH")
            .map_or_else(|| PathBuf::from("gh"), PathBuf::from);
        publish::publish(
            crate::git_bin::program(),
            &gh,
            dir,
            branch,
            &project.base_branch,
            title,
            body,
        )
    }

    fn remove(&self, project: &ProjectConfig, id: &str) {
        if let Err(e) = draft_worktree::remove(Path::new(&project.repo_path), Path::new(&project.worktrees_dir), id) {
            tracing::warn!("{id}: removing the draft's worktree: {e:#}");
        }
    }
}
