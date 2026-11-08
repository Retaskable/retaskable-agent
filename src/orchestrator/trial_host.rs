//! The trial plan's session (docs/specs/first-run-check.md section 3.3, "In this Rust agent, no drafts module, no
//! store draft"): one fresh implementer session on a lane, with the fixed README instruction standing in for an
//! item's `remaining` text, and its PR if it opens one. Modelled on [`super::piece_runs`] and
//! [`super::draft_host`], but nothing is retried: any end but `pr_opened` is [`TrialEnd::Dropped`].
//! The session's process tree registers with `process_tree`, so a restart or Quit cuts it.
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;
use serde_json::Value;

use super::session::quiet_timeout;
use crate::backlog::{Item, ItemStatus};
use crate::config::{CursorModels, Engine, EngineChoiceConfig, ModelsConfig, ProjectConfig};
use crate::engines::claude::sandbox_enabled;
use crate::engines::engine::CursorOptions;
use crate::engines::{Role, SessionStart, TranscriptSink, engine_for, engine_of, place, run_session};
use crate::env::Env;
use crate::implementer::{Interrupted, Session, Start, publish, run_implementer, session_spec};
use crate::tiers::{implementer_model, implementer_tier};
use crate::trial_plan::{TRIAL_ID, TRIAL_TEXT};
use crate::{lanes, paths};

/// The trial's pull request title.
pub const TRIAL_TITLE: &str = "Say hello to this repo";

const TEMPLATE: &str = include_str!("../../prompts/implementer.md");

/// What the trial's session needs: the project, a free lane, the trial's branch and session id, and the project's
/// latest `state.usage` reading (for the model tier).
pub struct TrialJob {
    pub project: ProjectConfig,
    pub lane: u32,
    pub branch: String,
    pub session_id: String,
    pub usage: Option<Value>,
    /// The `engine-placement` flag: Off, only a Claude trial runs.
    pub placement: bool,
}

/// How the trial ended. `Dropped` carries why; the trial isn't retried.
#[derive(Debug, PartialEq, Eq)]
pub enum TrialEnd {
    Opened { pr_url: String },
    Dropped { reason: String },
}

/// The parts of the trial that touch the machine: git, the code host and the engine. Faked in tests.
pub trait TrialHost: Send + Sync {
    /// Prepares the lane and runs the session. An `Err` is this computer's own trouble (the lane, or a restart or
    /// Quit cutting the session: it downcasts to [`Interrupted`]); anything the trial itself did wrong is `Dropped`.
    fn run(&self, job: TrialJob) -> BoxFuture<'static, Result<TrialEnd>>;
}

pub struct ClaudeTrial {
    choice: EngineChoiceConfig,
    data_dir: PathBuf,
    env: Env,
    models: ModelsConfig,
    cursor_models: CursorModels,
}

impl ClaudeTrial {
    pub fn new(
        data_dir: PathBuf,
        choice: EngineChoiceConfig,
        env: Env,
        models: ModelsConfig,
        cursor_models: CursorModels,
    ) -> Arc<Self> {
        Arc::new(Self {
            choice,
            data_dir,
            env,
            models,
            cursor_models,
        })
    }
}

fn dropped(reason: impl Into<String>) -> Result<TrialEnd> {
    Ok(TrialEnd::Dropped { reason: reason.into() })
}

/// The synthetic item the prompt is built from: [`TRIAL_TEXT`] as its `remaining`, and no spec files.
fn trial_item() -> Item {
    Item {
        id: TRIAL_ID.into(),
        title: TRIAL_TITLE.into(),
        status: ItemStatus::Building,
        slice: 1,
        remaining: Some(TRIAL_TEXT.into()),
        ..Item::default()
    }
}

impl TrialHost for ClaudeTrial {
    fn run(&self, job: TrialJob) -> BoxFuture<'static, Result<TrialEnd>> {
        let (choice, data_dir, env, models, cursor_models) = (
            self.choice.clone(),
            self.data_dir.clone(),
            self.env.clone(),
            self.models,
            self.cursor_models.clone(),
        );
        Box::pin(async move {
            let project = &job.project;
            let chosen = engine_for(&choice, project.engines.as_ref(), Role::Implementer, None);
            if chosen != Engine::Claude && !job.placement {
                return dropped(format!("{chosen} sessions don't run yet"));
            }
            let placed = match place(chosen, Role::Implementer, &env, &cursor_models) {
                Ok(placed) => placed,
                Err(why) => return dropped(why),
            };
            let (repo, worktrees, lane, branch, base) = (
                PathBuf::from(&project.repo_path),
                PathBuf::from(&project.worktrees_dir),
                job.lane,
                job.branch.clone(),
                project.base_branch.clone(),
            );
            let cwd =
                tokio::task::spawn_blocking(move || lanes::prepare(&repo, &worktrees, lane, &branch, &base)).await??;
            let item = trial_item();
            let now = crate::telemetry::now_ms() / 1000;
            let model = implementer_model(implementer_tier(job.usage.as_ref(), now, &models), &item).to_string();
            let mut spec = session_spec(
                &Session {
                    template: TEMPLATE,
                    project_type: project.project_type,
                    item: &item,
                    branch: &job.branch,
                    base_branch: &project.base_branch,
                    github_repo: &project.github_repo,
                    checks: &project.checks,
                    code_gates: project.code_gates,
                    where_to_start: "",
                    notes: None,
                    items: &[],
                    cwd: &cwd,
                    model,
                    budget_usd: project.budget.drafter_usd,
                    max_turns: None,
                    engine: Some(placed.engine),
                    cursor: placed.cursor.clone().map(|c| CursorOptions {
                        cli: (!project.github_repo.is_empty()).then(|| "gh".to_string()),
                        ..c
                    }),
                    local: None,
                    sandbox: sandbox_enabled(project.sandbox, cfg!(windows)),
                },
                Start::Fresh,
            )?;
            spec.start = SessionStart::Fresh(job.session_id.clone());
            let sink = TranscriptSink {
                path: paths::run_dir(&data_dir, &project.id_or_derived(), TRIAL_ID)
                    .join(format!("{}.jsonl", job.session_id)),
                lane: None,
            };
            let engine = engine_of(Some(placed.engine))?;
            let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
            let id = job.session_id.clone();
            let (answer, _) = run_implementer(spec, |spec| async move {
                run_session(engine.as_ref(), &placed.bin, &spec, &id, Some(&sink), quiet).await
            })
            .await;
            let result = match answer {
                Ok(r) => r,
                Err(e) if e.downcast_ref::<Interrupted>().is_some() => return Err(e),
                Err(e) => return dropped(format!("{e:#}")),
            };
            if result.status != "pr_opened" {
                return dropped(format!("the session ended {}: {}", result.status, result.summary));
            }
            let gh = env
                .get("RETASKABLE_GH")
                .map_or_else(|| PathBuf::from("gh"), PathBuf::from);
            let (base, branch) = (project.base_branch.clone(), job.branch.clone());
            let body = result
                .change_body
                .as_deref()
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .map_or(result.summary, str::to_string);
            let published = tokio::task::spawn_blocking(move || {
                publish::publish(crate::git_bin::program(), &gh, &cwd, &branch, &base, TRIAL_TITLE, &body)
            })
            .await?;
            match published {
                Ok(p) => Ok(TrialEnd::Opened { pr_url: p.url }),
                Err(e) => dropped(format!("the work is committed but couldn't be published: {e:#}")),
            }
        })
    }
}

#[cfg(all(test, unix))]
#[path = "trial_host_tests.rs"]
mod tests;
