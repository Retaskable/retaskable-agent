//! The real [`ExpertHost`]: one turn of an expert session is one `claude -p` run in the session's lane, the first
//! with the expert prompt and a new session id, each after it resuming that id with the person's bare message
//! (docs/specs/expert-chat.md). The lane is set up before the first turn, and again, from the pushed branch, after a
//! park. Claude only, on Sonnet, with no dollar cap (the person is in the loop). The session's process tree
//! registers with `process_tree`, so a restart or Quit cuts it; `stop` cuts it too and what the lane left running.
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;

use super::driver::{ExpertHost, ExpertJob, TurnEnd};
use super::session::watch_for;
use crate::config::{CursorModels, Engine, ProjectConfig};
use crate::engines::claude::sandbox_enabled;
use crate::engines::{EngineId, Role, SessionSpec, SessionStart, TranscriptSink, engine_of, place, run_session};
use crate::env::Env;
use crate::expert;
use crate::git_bin::command;
use crate::implementer::session::new_session_id;
use crate::merge_update::run as git;
use crate::tiers::SONNET;
use crate::{lanes, paths};

/// What a turn is cut at (`maxTurns` of an expert session).
const MAX_TURNS: u32 = 300;

pub struct ClaudeExperts {
    data_dir: PathBuf,
    env: Env,
    cursor_models: CursorModels,
}

impl ClaudeExperts {
    pub fn new(data_dir: PathBuf, env: Env, cursor_models: CursorModels) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            env,
            cursor_models,
        })
    }
}

fn worktrees(project: &ProjectConfig) -> PathBuf {
    PathBuf::from(&project.worktrees_dir)
}

/// Sets up the lane for the session: on the base for a new one, on the pushed branch after a park.
async fn prepare(job: &ExpertJob) -> Result<PathBuf> {
    let project = &job.project;
    let (repo, trees, lane, branch, base) = (
        PathBuf::from(&project.repo_path),
        worktrees(project),
        job.lane,
        job.branch.clone(),
        project.base_branch.clone(),
    );
    let from_branch = job.from_branch;
    tokio::task::spawn_blocking(move || {
        if from_branch {
            let dir = lanes::lane_path(&trees, lane);
            lanes::prepare_at(&repo, &trees, &dir, &branch, &format!("origin/{branch}"))
        } else {
            lanes::prepare(&repo, &trees, lane, &branch, &base)
        }
    })
    .await?
}

async fn run_turn(host: &ClaudeExperts, job: ExpertJob) -> Result<TurnEnd> {
    let project = &job.project;
    let placed = place(Engine::Claude, Role::Implementer, &host.env, &host.cursor_models)
        .map_err(|why| anyhow::anyhow!("{why}"))?;
    let cwd = if job.prepare {
        prepare(&job).await?
    } else {
        lanes::lane_path(&worktrees(project), job.lane)
    };
    let (prompt, start, id) = match &job.session_id {
        Some(id) => (job.message.clone(), SessionStart::Resume(id.clone()), id.clone()),
        None => {
            let id = new_session_id();
            let prompt = expert::first_prompt(&job.branch, &project.base_branch, &job.message)?;
            (prompt, SessionStart::Fresh(id.clone()), id)
        }
    };
    let spec = SessionSpec {
        engine: Some(EngineId::Claude),
        model: SONNET.to_string(),
        prompt,
        schema: expert::schema(),
        cwd: cwd.clone(),
        max_turns: Some(MAX_TURNS),
        budget_usd: 0.0,
        start,
        readonly: false,
        shell: true,
        cursor: None,
        local: None,
        sandbox: sandbox_enabled(project.sandbox, cfg!(windows)),
    };
    let project_id = project.id_or_derived();
    let sink = TranscriptSink {
        path: paths::run_dir(&host.data_dir, &project_id, &job.item).join(format!("{id}.jsonl")),
        lane: Some(job.lane),
    };
    let engine = engine_of(Some(EngineId::Claude))?;
    let quiet = watch_for(project, EngineId::Claude);
    let listed = job
        .sessions
        .register_spec(&project_id, &job.item, None, Role::Implementer, &spec);
    let run = listed.scope(run_session(
        engine.as_ref(),
        &placed.bin,
        &spec,
        &id,
        Some(&sink),
        quiet,
    ));
    let r = tokio::select! {
        r = run => r?,
        () = job.stop.notified() => {
            let dir = cwd.clone();
            let _ = tokio::task::spawn_blocking(move || lanes::kill_processes_in(&dir)).await;
            return Ok(TurnEnd::Stopped);
        }
    };
    if r.subtype == "interrupted" {
        return Ok(TurnEnd::Failed("a restart cut the turn".into()));
    }
    if !r.ok {
        let why = r.error.clone().unwrap_or_else(|| r.subtype.clone());
        return Ok(TurnEnd::Failed(why));
    }
    let session_id = if r.session_id.is_empty() {
        id
    } else {
        r.session_id.clone()
    };
    let reply = expert::parse(&r.structured.unwrap_or_default());
    Ok(TurnEnd::Done { session_id, reply })
}

impl ExpertHost for ClaudeExperts {
    fn turn(&self, job: ExpertJob) -> BoxFuture<'static, TurnEnd> {
        let host = Self {
            data_dir: self.data_dir.clone(),
            env: self.env.clone(),
            cursor_models: self.cursor_models.clone(),
        };
        Box::pin(async move {
            match run_turn(&host, job).await {
                Ok(end) => end,
                Err(e) => TurnEnd::Failed(format!("{e:#}")),
            }
        })
    }

    fn park(&self, project: &ProjectConfig, lane: u32, branch: &str) -> BoxFuture<'static, Result<(), String>> {
        let (dir, branch) = (lanes::lane_path(&worktrees(project), lane), branch.to_string());
        Box::pin(async move {
            let pushed = tokio::task::spawn_blocking(move || {
                // Whatever the session left uncommitted is saved first, so the push carries all of it.
                let _ = git(&command, &dir, &["add", "-A"]);
                let _ = git(
                    &command,
                    &dir,
                    &["commit", "--quiet", "-m", "Expert session: work in progress"],
                );
                git(
                    &command,
                    &dir,
                    &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
                )
            })
            .await;
            match pushed {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(e) => Err(format!("{e}")),
            }
        })
    }

    fn discard(&self, project: &ProjectConfig, lane: Option<u32>, branch: &str) -> BoxFuture<'static, ()> {
        let (repo, dir, branch) = (
            PathBuf::from(&project.repo_path),
            lane.map(|l| lanes::lane_path(&worktrees(project), l)),
            branch.to_string(),
        );
        Box::pin(async move {
            let _ = tokio::task::spawn_blocking(move || -> Result<()> {
                if let Some(dir) = dir {
                    lanes::kill_processes_in(&dir).context("cutting what the lane runs")?;
                }
                git(&command, &repo, &["push", "origin", "--delete", &branch])?;
                Ok(())
            })
            .await;
        })
    }
}
