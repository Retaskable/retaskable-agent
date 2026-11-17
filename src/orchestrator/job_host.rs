//! The real [`JobHost`]: the read-only sessions the store's jobs run (docs/specs/pools.md section 3f; `triageTask.ts`,
//! `vetChore.ts` and `releaseNotes.ts` in the TypeScript agent). Triage runs on the triager's engine at the chore tier
//! (the top tier for a check) with `budget.reviewerUsd`; vetting and release notes on the writer's engine at the
//! scouts' tier (haiku, or the local model while it is free) with `budget.scoutUsd` and `budget.releaseNotesUsd`.
//! Each runs in the base checkout when there is one, never in a lane, listed on the Agents tab while it runs. The
//! other tools (Lighthouse, the probes, the browser) are [`JobHost`]'s own defaults.
use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::cursor_guard::watch_for;
use super::live::sessions::Sessions;
use crate::ci_judge::judge_cwd;
use crate::config::{CursorModels, EngineChoiceConfig, ProjectConfig};
use crate::engines::claude::sandbox_enabled;
use crate::engines::cursor::with_project;
use crate::engines::{
    Placement, Role, SessionResult, SessionSpec, engine_for, engine_of, place, run_session, start_engine,
};
use crate::env::Env;
use crate::jobs::host::{JobHost, JobRole, JobSession, JobSessionConfig};
use crate::local_model::{LocalModel, LocalSlot};
use crate::release_notes::writer_model;
use crate::tiers::{LOCAL, OPUS, SONNET, claude_model_for, floor};

pub struct ClaudeJobs {
    choice: EngineChoiceConfig,
    env: Env,
    cursor_models: CursorModels,
    local: LocalModel,
    sessions: Sessions,
}

impl ClaudeJobs {
    pub fn new(
        choice: EngineChoiceConfig,
        env: Env,
        cursor_models: CursorModels,
        local: LocalModel,
        sessions: Sessions,
    ) -> Arc<Self> {
        Arc::new(Self {
            choice,
            env,
            cursor_models,
            local,
            sessions,
        })
    }
}

struct Ready {
    config: JobSessionConfig,
    placed: Placement,
    project: ProjectConfig,
    role: Role,
    label: String,
    sessions: Sessions,
    /// The local model's slot, held until the session is dropped.
    _slot: Option<LocalSlot>,
}

impl JobSession for Ready {
    fn config(&self) -> &JobSessionConfig {
        &self.config
    }

    fn run(&self, spec: SessionSpec) -> BoxFuture<'static, anyhow::Result<SessionResult>> {
        let (placed, project) = (self.placed.clone(), self.project.clone());
        let listed = self
            .sessions
            .register_spec(&project.id_or_derived(), &self.label, None, self.role, &spec);
        Box::pin(async move {
            let engine = engine_of(Some(placed.engine))?;
            let quiet = watch_for(&project, placed.engine);
            listed
                .scope(run_session(engine.as_ref(), &placed.bin, &spec, "", None, quiet))
                .await
        })
    }
}

impl JobHost for ClaudeJobs {
    fn session(
        &self,
        project: &ProjectConfig,
        job: JobRole,
    ) -> BoxFuture<'static, Result<Box<dyn JobSession>, String>> {
        let (choice, env, cursor_models) = (self.choice.clone(), self.env.clone(), self.cursor_models.clone());
        let (local, sessions) = (self.local.clone(), self.sessions.clone());
        let project = project.clone();
        Box::pin(async move {
            let role = match job {
                JobRole::TriageLook | JobRole::TriageCheck => Role::Triager,
                JobRole::Vet | JobRole::Release => Role::Writer,
            };
            let engine = start_engine(None, engine_for(&choice, project.engines.as_ref(), role, None));
            let placed = place(engine, role, &env, &cursor_models)?;
            let ladder = local.ladder_for(placed.engine == crate::engines::EngineId::Claude);
            let tier = match job {
                JobRole::TriageLook => floor(SONNET, false, ladder),
                JobRole::TriageCheck => floor(OPUS, false, ladder),
                // The scouts' tier: the local model when it can run the role, else haiku.
                JobRole::Vet | JobRole::Release => writer_model(if ladder { LOCAL } else { SONNET }),
            };
            // Waits for the local model's slot when the tier is `local`; held until the session is dropped.
            let claimed = local.claim(tier).await;
            let budget = match job {
                JobRole::TriageLook | JobRole::TriageCheck => project.budget.reviewer_usd,
                JobRole::Vet => project.budget.scout_usd,
                JobRole::Release => project.budget.release_notes_usd,
            };
            let (slot, target) = match claimed {
                Some((slot, target)) => (Some(slot), Some(target)),
                None => (None, None),
            };
            let config = JobSessionConfig {
                cwd: judge_cwd(
                    std::path::Path::new(&project.worktrees_dir),
                    std::path::Path::new(&project.repo_path),
                ),
                budget_usd: budget,
                model: claude_model_for(placed.engine, tier),
                local: target,
                engine: Some(placed.engine),
                cursor: placed.cursor.clone().map(|c| with_project(&project, c)),
                sandbox: sandbox_enabled(project.sandbox, cfg!(windows)),
                shell: true,
            };
            let label = match job {
                JobRole::TriageLook => "_triage",
                JobRole::TriageCheck => "_triage-check",
                JobRole::Vet => "_vet",
                JobRole::Release => "_release",
            };
            Ok(Box::new(Ready {
                config,
                placed,
                project,
                role,
                label: label.to_string(),
                sessions,
                _slot: slot,
            }) as Box<dyn JobSession>)
        })
    }
}
