//! The real [`ExpertHost`]: one turn of an expert session is one `claude -p` run in the session's lane, the first
//! with the expert prompt and a new session id, each after it resuming that id with the person's bare message
//! (docs/specs/expert-chat.md). The lane is set up before the first turn, and again, from the pushed branch, after a
//! park. Claude only, on Sonnet, with no dollar cap (the person is in the loop). The session's process tree
//! registers with `process_tree`, so a restart or Quit cuts it; `stop` cuts it too and what the lane left running.
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;

use super::driver::{ExpertHost, ExpertJob, HelperJob, Opened, ShipJob, TurnEnd, WorkRead};
use super::session::watch_for;
use crate::config::{CursorModels, Engine, ProjectConfig};
use crate::engines::claude::sandbox_enabled;
use crate::engines::{
    EngineId, Role, SessionSpec, SessionStart, TranscriptSink, engine_of, place, run_session, run_session_tracked,
};
use crate::env::Env;
use crate::expert::claude_sessions::Continued;
use crate::expert::helpers::{self, Ended};
use crate::expert::history::{self, Turn};
use crate::expert::work;
use crate::expert::{self, claude_sessions};
use crate::git_bin::command;
use crate::implementer::publish::publish;
use crate::implementer::session::new_session_id;
use crate::merge_update::run as git;
use crate::tiers::SONNET;
use crate::{lanes, paths};

/// What a turn is cut at (`maxTurns` of an expert session).
const MAX_TURNS: u32 = 300;
/// The lighter tier a helper runs on (`scoutTier` on Claude).
const HELPER_MODEL: &str = "haiku";

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
        return Ok(if r.limit_hit {
            TurnEnd::Limited(why)
        } else {
            TurnEnd::Failed(why)
        });
    }
    let session_id = if r.session_id.is_empty() {
        id
    } else {
        r.session_id.clone()
    };
    let reply = expert::parse(&r.structured.unwrap_or_default());
    Ok(TurnEnd::Done { session_id, reply })
}

/// Makes lane `lane` ready for a lane helper: on its own branch, from the session's last commit (the session's branch,
/// as this repository has it: nothing of it is pushed).
fn prepare_helper_lane(project: &ProjectConfig, lane: u32, branch: &str, from: &str) -> Result<PathBuf> {
    let (repo, trees) = (PathBuf::from(&project.repo_path), worktrees(project));
    let dir = lanes::lane_path(&trees, lane);
    lanes::prepare_at(&repo, &trees, &dir, branch, from)
}

/// One helper: a scout on the lighter tier, read-only in the session's lane or, with a lane of its own, changing files
/// there. Its result is the `result` of its structured output.
async fn run_helper(host: &ClaudeExperts, job: HelperJob) -> Result<Ended> {
    let project = job.project.clone();
    let placed =
        place(Engine::Claude, Role::Scout, &host.env, &host.cursor_models).map_err(|why| anyhow::anyhow!("{why}"))?;
    let own_branch = helpers::branch(&job.from, job.n);
    let cwd = if job.own {
        let (p, lane, b, from) = (project.clone(), job.lane, own_branch.clone(), job.from.clone());
        tokio::task::spawn_blocking(move || prepare_helper_lane(&p, lane, &b, &from)).await??
    } else {
        lanes::lane_path(&worktrees(&project), job.lane)
    };
    let id = new_session_id();
    let (branch, from) = if job.own {
        (&own_branch, Some(job.from.as_str()))
    } else {
        (&job.from, None)
    };
    let spec = SessionSpec {
        engine: Some(EngineId::Claude),
        model: HELPER_MODEL.to_string(),
        prompt: helpers::prompt(&job.task, branch, from)?,
        schema: helpers::schema(),
        cwd: cwd.clone(),
        max_turns: Some(MAX_TURNS),
        budget_usd: 0.0,
        start: SessionStart::Fresh(id.clone()),
        readonly: !job.own,
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
    let quiet = watch_for(&project, EngineId::Claude);
    let listed = job
        .sessions
        .register_spec(&project_id, &job.item, None, Role::Scout, &spec);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let run = listed.scope(run_session_tracked(
        engine.as_ref(),
        &placed.bin,
        &spec,
        &id,
        Some(&sink),
        quiet,
        Some(tx),
    ));
    tokio::pin!(run);
    let mut tree = None;
    let mut rx = Some(rx);
    let r = loop {
        tokio::select! {
            r = &mut run => break r?,
            Ok(t) = async { rx.as_mut().expect("polled while set").await }, if rx.is_some() => {
                tree = Some(t);
                rx = None;
            }
            () = job.stop.notified() => {
                // Only this helper's own processes: a read helper shares the session's lane, and its turn goes on.
                if let Some(t) = tree.take() {
                    t.cut();
                    let _ = tokio::task::spawn_blocking(move || t.kill_and_wait(crate::process_tree::KILL_LIMIT)).await;
                }
                if job.own {
                    let dir = cwd.clone();
                    let _ = tokio::task::spawn_blocking(move || lanes::kill_processes_in(&dir)).await;
                }
                return Ok(Ended::Stopped);
            }
        }
    };
    if r.subtype == "interrupted" {
        return Ok(Ended::Failed("a restart cut it".into()));
    }
    if !r.ok {
        return Ok(Ended::Failed(r.error.clone().unwrap_or_else(|| r.subtype.clone())));
    }
    Ok(match helpers::result_of(&r.structured.unwrap_or_default()) {
        Some(result) => Ended::Finished(result),
        None => Ended::Failed(r.subtype),
    })
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

    fn continuing(&self, id: &str) -> Result<Continued, String> {
        claude_sessions::continuing(&claude_sessions::projects_root(&self.env), id)
    }

    fn carry_on(
        &self,
        project: &ProjectConfig,
        from: &Continued,
        name: &str,
    ) -> BoxFuture<'static, Result<(), String>> {
        let (repo, base) = (PathBuf::from(&project.repo_path), project.base_branch.clone());
        let (folder, branch, name) = (PathBuf::from(&from.folder), from.branch.clone(), name.to_string());
        Box::pin(async move {
            let done = tokio::task::spawn_blocking(move || {
                claude_sessions::branch_at_tip(&repo, &base, &folder, branch.as_deref(), &name)
            })
            .await;
            done.unwrap_or_else(|e| Err(format!("its branch couldn't be copied ({e})")))
        })
    }

    fn handover(&self, id: &str, request: &str) -> BoxFuture<'static, Option<String>> {
        let root = claude_sessions::projects_root(&self.env);
        let (id, request) = (id.to_string(), request.to_string());
        Box::pin(async move {
            let made = tokio::task::spawn_blocking(move || {
                let from = claude_sessions::continuing(&root, &id).ok()?;
                claude_sessions::handover(&from, &request)
            })
            .await;
            made.ok().flatten()
        })
    }

    fn ship(&self, job: ShipJob) -> BoxFuture<'static, Result<Opened, String>> {
        let gh = self
            .env
            .get("RETASKABLE_GH")
            .map_or_else(|| PathBuf::from("gh"), PathBuf::from);
        Box::pin(async move {
            let shipped = tokio::task::spawn_blocking(move || -> Result<Opened> {
                let (repo, trees) = (PathBuf::from(&job.project.repo_path), worktrees(&job.project));
                let dir = lanes::lane_path(&trees, job.lane);
                // A parked session's lane is set up again from the branch the park pushed.
                if job.prepare {
                    lanes::prepare_at(&repo, &trees, &dir, &job.branch, &format!("origin/{}", job.branch))?;
                }
                // Whatever the session left uncommitted goes in the change.
                let _ = git(&command, &dir, &["add", "-A"]);
                let _ = git(
                    &command,
                    &dir,
                    &["commit", "--quiet", "-m", "Expert session: work in progress"],
                );
                // The lane carries the item's own branch from here on, as an implementer's does.
                git(&command, &dir, &["checkout", "-B", &job.target])?;
                let base = &job.project.base_branch;
                let published = publish(
                    crate::git_bin::program(),
                    &gh,
                    &dir,
                    &job.target,
                    base,
                    &job.title,
                    &job.body,
                )?;
                // The session's own copy on the remote is no longer wanted: the change is on the item's branch.
                if job.branch != job.target {
                    let _ = git(&command, &dir, &["push", "origin", "--delete", &job.branch]);
                }
                // The commit the change is at, for the store's check.
                let head = git(&command, &dir, &["rev-parse", "HEAD"])
                    .ok()
                    .map(|h| h.trim().to_string())
                    .filter(|h| !h.is_empty());
                Ok(Opened {
                    change: published,
                    head,
                })
            })
            .await;
            match shipped {
                Ok(Ok(p)) => Ok(p),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(e) => Err(format!("{e}")),
            }
        })
    }

    fn helper(&self, job: HelperJob) -> BoxFuture<'static, Ended> {
        let host = Self {
            data_dir: self.data_dir.clone(),
            env: self.env.clone(),
            cursor_models: self.cursor_models.clone(),
        };
        Box::pin(async move {
            match run_helper(&host, job).await {
                Ok(end) => end,
                Err(e) => Ended::Failed(format!("{e:#}")),
            }
        })
    }

    fn history(
        &self,
        project: &ProjectConfig,
        item: &str,
        session_id: Option<&str>,
        from: Option<&str>,
    ) -> BoxFuture<'static, (Vec<Turn>, Vec<Turn>)> {
        let own = session_id
            .map(|id| paths::run_dir(&self.data_dir, &project.id_or_derived(), item).join(format!("{id}.jsonl")));
        let root = from.map(|id| (claude_sessions::projects_root(&self.env), id.to_string()));
        Box::pin(async move {
            let read = tokio::task::spawn_blocking(move || {
                let earlier = root
                    .and_then(|(root, id)| claude_sessions::session_file(&root, &id))
                    .map(|f| history::claude_turns(&f))
                    .unwrap_or_default();
                let own = own.map(|f| history::read_turns(&f)).unwrap_or_default();
                (earlier, own)
            })
            .await;
            read.unwrap_or_default()
        })
    }

    fn work(
        &self,
        project: &ProjectConfig,
        lane: Option<u32>,
        branch: &str,
        helpers: u32,
    ) -> BoxFuture<'static, WorkRead> {
        let gh = self
            .env
            .get("RETASKABLE_GH")
            .map_or_else(|| PathBuf::from("gh"), PathBuf::from);
        let (repo, base, branch) = (
            PathBuf::from(&project.repo_path),
            format!("origin/{}", project.base_branch),
            branch.to_string(),
        );
        let dir = lane.map(|l| lanes::lane_path(&worktrees(project), l));
        Box::pin(async move {
            let read = tokio::task::spawn_blocking(move || {
                let work = dir.and_then(|d| work::read(&d, &base, &branch).ok());
                // `gh` finds the repository from the checkout, as every other host call here does.
                let prs = work::prs(&gh, &repo, &branch, helpers).ok();
                WorkRead { work, prs }
            })
            .await;
            read.unwrap_or_default()
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

#[cfg(test)]
mod tests;
