//! The real [`CiWatchHost`]: GitHub's reads through `gh`, and the judge's read-only session on the scouts' tier with
//! the project's `budget.scoutUsd`, in the base checkout when there is one (`judgeRed` in the TS agent's
//! `ciWatchFix.ts`). The session's process tree registers with `process_tree`, so a restart or Quit cuts it.
//! The fix session is the implementer lane's (`fixRed` in the same file): the implementer role's engine, `budget.implementerUsd`,
//! `machine.maxTurns`, the lane's own spec builder, in the throwaway worktree the pass gives it.
//! The store's bell lines are logged: the agent has no notification with a title and a body of its own (the tray's
//! `notify::notify` takes one text, under its own title, and runs only while the tray is up).
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::driver::CiWatchHost;
use super::live::sessions::Sessions;
use super::session::watch_for;
use crate::backlog::ci_result::CiVerdict;
use crate::ci_fix::{self, FixSession};
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
use crate::tiers::{LOCAL, SONNET, claude_model_for, floor};

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
    /// `machine.maxTurns`, the implementer's.
    max_turns: u32,
}

impl ClaudeCiWatch {
    pub fn new(
        choice: EngineChoiceConfig,
        env: Env,
        cursor_models: CursorModels,
        local: LocalModel,
        sessions: Sessions,
        max_turns: u32,
    ) -> Arc<Self> {
        Arc::new(Self {
            choice,
            env,
            cursor_models,
            local,
            sessions,
            max_turns,
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

    /// The fix session in `dir`: true when it went ok. The implementer's engine, tier (the floor tier when Claude is at
    /// its limit and the local model is up, else sonnet: this computer works no usage tier out), budget and turns.
    async fn fix_session(
        &self,
        project: &ProjectConfig,
        pr: u64,
        title: &str,
        branch: &str,
        what: &str,
        dir: &Path,
    ) -> bool {
        let id = project.id_or_derived();
        let engine = start_engine(
            None,
            engine_for(&self.choice, project.engines.as_ref(), Role::Implementer, None),
        );
        let placed = match place(engine, Role::Implementer, &self.env, &self.cursor_models) {
            Ok(p) => p,
            Err(why) => {
                tracing::warn!("{id}: ci watch: no fix for pull request #{pr}: {why}");
                return false;
            }
        };
        let up = self.local.ladder_for(placed.engine == EngineId::Claude);
        let tier = floor(SONNET, self.sessions.claude_at_limit(), up);
        // Waits for the local model's slot when the tier is `local`; held until the session ends.
        let claimed = self.local.claim(tier).await;
        let session = FixSession {
            project,
            cwd: dir,
            model: claude_model_for(placed.engine, tier).to_string(),
            max_turns: self.max_turns,
            engine: Some(placed.engine),
            cursor: placed.cursor.clone().map(|c| with_project(project, c)),
            local: claimed.as_ref().map(|(_, target)| target.clone()),
        };
        let spec = match ci_fix::session_spec(pr, title, branch, what, &session) {
            Ok(spec) => spec,
            Err(e) => {
                tracing::warn!("{id}: ci watch: fix for pull request #{pr}: {e:#}");
                return false;
            }
        };
        let Ok(engine) = engine_of(Some(placed.engine)) else {
            return false;
        };
        let quiet = watch_for(project, placed.engine);
        let listed = self
            .sessions
            .register_spec(&id, &format!("_ci-{pr}"), None, Role::Implementer, &spec);
        let ran = listed
            .scope(run_session(engine.as_ref(), &placed.bin, &spec, "", None, quiet))
            .await;
        drop(claimed);
        ran.is_ok_and(|r| r.ok)
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

    fn fix(
        &self,
        project: &ProjectConfig,
        pr: u64,
        title: String,
        branch: String,
        what: String,
        dir: PathBuf,
    ) -> BoxFuture<'static, bool> {
        let (me, project) = (self.clone(), project.clone());
        Box::pin(async move { me.fix_session(&project, pr, &title, &branch, &what, &dir).await })
    }

    fn bell(&self, title: &str, body: &str) {
        tracing::info!("ci watch: {title}: {body}");
    }
}

#[cfg(test)]
mod tests;
