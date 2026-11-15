//! The real [`CiWatchHost`]: GitHub's reads through `gh`, and the judge's read-only session on the scouts' tier with
//! the project's `budget.scoutUsd`, in the base checkout when there is one (`judgeRed` in the TS agent's
//! `ciWatchFix.ts`). The session's process tree registers with `process_tree`, so a restart or Quit cuts it.
//! The store's bell lines are logged: the agent has no notification with a title and a body of its own (the tray's
//! `notify::notify` takes one text, under its own title, and runs only while the tray is up).
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::driver::CiWatchHost;
use super::live::sessions::Sessions;
use super::session::watch_for;
use crate::backlog::ci_result::CiVerdict;
use crate::ci_judge::{self, judge_cwd};
use crate::ci_pass::{CiHost, host_for};
use crate::config::{CursorModels, EngineChoiceConfig, ProjectConfig};
use crate::engines::claude::sandbox_enabled;
use crate::engines::cursor::with_project;
use crate::engines::engine::LocalTarget;
use crate::engines::{
    EngineId, EphemeralSessionConfig, Placement, Role, engine_for, engine_of, place, run_session, start_engine,
};
use crate::env::Env;
use crate::local_model::LocalModel;
use crate::release_notes::writer_model;
use crate::tiers::{LOCAL, SONNET, claude_model_for};

/// The judge session's settings: the scouts' tier, `budget.scoutUsd`, the project's sandbox setting and `cwd`.
fn session_config<'a>(
    project: &ProjectConfig,
    placed: &Placement,
    tier: &str,
    cwd: &'a Path,
    local: Option<LocalTarget>,
) -> EphemeralSessionConfig<'a> {
    EphemeralSessionConfig {
        cwd,
        budget_usd: project.budget.scout_usd,
        model: claude_model_for(placed.engine, tier),
        local,
        engine: Some(placed.engine),
        cursor: placed.cursor.clone().map(|c| with_project(project, c)),
        sandbox: sandbox_enabled(project.sandbox, cfg!(windows)),
        shell: true,
    }
}

#[derive(Clone)]
pub struct ClaudeCiWatch {
    choice: EngineChoiceConfig,
    env: Env,
    cursor_models: CursorModels,
    local: LocalModel,
    sessions: Sessions,
}

impl ClaudeCiWatch {
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

    fn gh(&self) -> PathBuf {
        self.env.get("RETASKABLE_GH").map_or_else(|| "gh".into(), PathBuf::from)
    }

    async fn judge_session(
        &self,
        project: &ProjectConfig,
        pr: u64,
        title: &str,
        branch: &str,
        what: &str,
    ) -> Option<(CiVerdict, String)> {
        let id = project.id_or_derived();
        let engine = start_engine(
            None,
            engine_for(&self.choice, project.engines.as_ref(), Role::Writer, None),
        );
        let placed = match place(engine, Role::Writer, &self.env, &self.cursor_models) {
            Ok(p) => p,
            Err(why) => {
                tracing::warn!("{id}: ci watch: no judge for pull request #{pr}: {why}");
                return None;
            }
        };
        // The scouts' tier: the local model when it can run the role, else haiku.
        let scout = if self.local.ladder_for(placed.engine == EngineId::Claude) {
            LOCAL
        } else {
            SONNET
        };
        let tier = writer_model(scout);
        // Waits for the local model's slot when the tier is `local`; held until the session ends.
        let claimed = self.local.claim(tier).await;
        let cwd = judge_cwd(Path::new(&project.worktrees_dir), Path::new(&project.repo_path));
        let local = claimed.as_ref().map(|(_, target)| target.clone());
        let cfg = session_config(project, &placed, tier, &cwd, local);
        let verdict = ci_judge::judge(pr, title, branch, what, &cfg, |spec| async move {
            let engine = engine_of(Some(placed.engine))?;
            let quiet = watch_for(project, placed.engine);
            let listed = self
                .sessions
                .register_spec(&id, &format!("_ci-{pr}"), None, Role::Writer, &spec);
            listed
                .scope(run_session(engine.as_ref(), &placed.bin, &spec, "", None, quiet))
                .await
        })
        .await;
        drop(claimed);
        verdict
    }
}

impl CiWatchHost for ClaudeCiWatch {
    fn code_host(&self, project: &ProjectConfig) -> Arc<dyn CiHost> {
        host_for(project, self.gh())
    }

    fn judge(
        &self,
        project: &ProjectConfig,
        pr: u64,
        title: String,
        branch: String,
        what: String,
    ) -> BoxFuture<'static, Option<(CiVerdict, String)>> {
        let (me, project) = (self.clone(), project.clone());
        Box::pin(async move { me.judge_session(&project, pr, &title, &branch, &what).await })
    }

    fn bell(&self, title: &str, body: &str) {
        tracing::info!("ci watch: {title}: {body}");
    }
}

#[cfg(test)]
mod tests;
