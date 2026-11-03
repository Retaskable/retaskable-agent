//! The `build` and `fix` runs of a piece (docs/specs/pieces.md sections 3.1 and 3.2): prepare the run's lane (a
//! fresh branch from `origin/<base>`, or the piece's pushed branch as pushed), run a fresh implementer session
//! with the piece as its brief, put its change through the code gates, push and open or update the change, and
//! report the end through `request.ends`. Modelled on [`super::session::ClaudeStarter`]: the same engine, model
//! tier and gate rounds, but nothing is resumed across runs and a piece's own `engine` field wins.
//! A beat's `stop` and the ceiling cut the session's process tree ([`RunProcesses`]); the loop releases the
//! lane whatever the outcome.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::driver::{RunEnd, RunProcesses, RunRequest, RunStarter, piece_id};
use super::session::{Attempt, GateRounds, Gated, Step, exists, gate_rounds, piece_branch_name, quiet_timeout};
use crate::backlog::runs::{RunKind, RunOutcome, RunPiece};
use crate::backlog::{Item, ItemStatus};
use crate::config::{Engine, EngineChoiceConfig, ModelsConfig, ProjectConfig};
use crate::engines::claude::sandbox_enabled;
use crate::engines::resolve::resolve_claude_on_path;
use crate::engines::{
    EngineId, Role, SessionStart, TranscriptSink, engine_for, engine_id_from_str, engine_of, run_session_tracked,
};
use crate::env::Env;
use crate::implementer::{Session, Start, publish, run_implementer, session_spec, start_files};
use crate::runs::{Pushed, end_from_answer, needs_push};
use crate::tiers::{implementer_model, implementer_tier};
use crate::{lanes, paths};

const PIECE_TEMPLATE: &str = include_str!("../../prompts/implementer-piece.md");

/// Runs `build` and `fix` runs of the projects it was built with.
pub struct PieceStarter {
    pub data_dir: PathBuf,
    pub projects: Vec<ProjectConfig>,
    pub choice: EngineChoiceConfig,
    pub env: Env,
    pub max_turns: u32,
    pub models: ModelsConfig,
}

impl RunStarter for PieceStarter {
    fn kinds(&self) -> Vec<RunKind> {
        vec![RunKind::Build, RunKind::Fix]
    }

    fn start(&self, request: RunRequest) -> Result<(), String> {
        let project = self
            .projects
            .iter()
            .find(|p| p.id_or_derived() == request.project)
            .ok_or_else(|| format!("no project {} is configured", request.project))?
            .clone();
        let lane = request.lane.clone().ok_or("the run has no lane")?;
        let engine = match request.piece.other.get("engine").and_then(Value::as_str) {
            Some(e) => engine_id_from_str(Some(e)) == EngineId::Claude,
            None => engine_for(&self.choice, project.engines.as_ref(), Role::Implementer, None) == Engine::Claude,
        };
        if !engine {
            return Err("only Claude sessions run in the Rust agent yet".into());
        }
        let bin = resolve_claude_on_path(&self.env).map_err(|e| format!("{e:#}"))?;
        if !exists(&bin, &self.env) {
            return Err(format!("the claude binary wasn't found ({})", bin.display()));
        }
        let job = Job {
            data_dir: self.data_dir.clone(),
            project,
            bin,
            gh: self
                .env
                .get("RETASKABLE_GH")
                .map_or_else(|| PathBuf::from("gh"), PathBuf::from),
            max_turns: self.max_turns,
            models: self.models,
            lane,
            token: request.token.clone(),
            kind: request.kind,
            piece: request.piece,
            processes: request.processes,
        };
        let ends = request.ends;
        let token = request.token;
        // The inner task is joined, so even a panic in it ends in exactly one end.
        tokio::spawn(async move {
            let (outcome, result) = match tokio::spawn(run(job)).await {
                Ok(end) => end,
                Err(e) => (RunOutcome::Failed(format!("the run's task crashed: {e}")), json!({})),
            };
            ends.send(RunEnd { token, outcome, result });
        });
        Ok(())
    }
}

struct Job {
    data_dir: PathBuf,
    project: ProjectConfig,
    bin: PathBuf,
    gh: PathBuf,
    max_turns: u32,
    models: ModelsConfig,
    lane: PathBuf,
    token: String,
    kind: RunKind,
    piece: RunPiece,
    processes: RunProcesses,
}

fn failed(why: impl Into<String>) -> (RunOutcome, Value) {
    (RunOutcome::Failed(why.into()), json!({}))
}

/// The piece as the item the session helpers want (its id, for "Where to start" and the tier).
fn piece_item(piece: &RunPiece) -> Item {
    Item {
        id: piece_id(piece).to_string(),
        title: piece.title.clone(),
        status: ItemStatus::Building,
        ..Item::default()
    }
}

/// The run's end: its lane is prepared on the piece's branch, a session runs, the gates check it and its change is
/// published. A failure before any session ran is this computer's own (`interrupted`, the run goes back); after
/// it, the run `failed`.
async fn run(job: Job) -> (RunOutcome, Value) {
    let (branch, start_point) = match (job.kind, job.piece.branch.as_deref()) {
        (RunKind::Fix, Some(b)) => (b.to_string(), format!("origin/{b}")),
        (RunKind::Fix, None) => return failed("A fix run needs the piece's pushed branch, and the piece has none."),
        _ => (
            piece_branch_name(piece_id(&job.piece), &job.piece.title),
            format!("origin/{}", job.project.base_branch),
        ),
    };
    let cwd = match prepare(&job, &branch, start_point).await {
        Ok(cwd) => cwd,
        Err(e) => {
            tracing::warn!("{}: preparing the lane: {e:#}", job.token);
            return (RunOutcome::Interrupted, json!({}));
        }
    };
    match build(&job, &cwd, &branch).await {
        Ok(end) => end,
        Err(e) => failed(format!("{e:#}")),
    }
}

async fn prepare(job: &Job, branch: &str, start_point: String) -> Result<PathBuf> {
    let (repo, worktrees, lane, branch) = (
        PathBuf::from(&job.project.repo_path),
        PathBuf::from(&job.project.worktrees_dir),
        job.lane.clone(),
        branch.to_string(),
    );
    let fix = job.kind == RunKind::Fix;
    tokio::task::spawn_blocking(move || {
        if fix {
            // The branch was pushed by an earlier run, maybe from another computer: fetch it for sure.
            lanes::fetch_is_stale(&repo);
        }
        lanes::prepare_at(&repo, &worktrees, &lane, &branch, &start_point)
    })
    .await
    .context("preparing the lane")?
    .context("preparing the lane")
}

/// One implementer session in `cwd` (a fresh one, or the resume of a gate round's follow-up).
async fn session(
    job: &Job,
    cwd: &Path,
    branch: &str,
    start: Start<'_>,
    where_to_start: &str,
    ids: &Mutex<String>,
) -> Result<Attempt> {
    let project = &job.project;
    let item = piece_item(&job.piece);
    let now = crate::telemetry::now_ms() / 1000;
    let model = implementer_model(implementer_tier(None, now, &job.models), &item).to_string();
    let spec = session_spec(
        &Session {
            template: "",
            project_type: project.project_type,
            item: &item,
            branch,
            base_branch: &project.base_branch,
            github_repo: &project.github_repo,
            checks: &project.checks,
            code_gates: project.code_gates,
            where_to_start,
            notes: None,
            items: &[],
            cwd,
            model,
            budget_usd: project.budget.implementer_usd,
            max_turns: Some(job.max_turns),
            engine: Some(EngineId::Claude),
            cursor: None,
            local: None,
            sandbox: sandbox_enabled(project.sandbox, cfg!(windows)),
        },
        start,
    )?;
    let id = match &spec.start {
        SessionStart::Fresh(id) | SessionStart::Resume(id) => id.clone(),
        SessionStart::Ephemeral => String::new(),
    };
    *ids.lock().unwrap() = id.clone();
    let sink = TranscriptSink {
        path: paths::run_dir(&job.data_dir, &project.id_or_derived(), piece_id(&job.piece))
            .join(format!("{id}-{}.jsonl", crate::telemetry::now_ms())),
        lane: None,
    };
    let engine = engine_of(Some(EngineId::Claude))?;
    let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
    let mut context_tokens = None;
    let ctx = &mut context_tokens;
    let (answer, usage) = run_implementer(spec, |spec| async move {
        // The tree goes to the run as soon as the session starts, so a stop can cut it.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (processes, token) = (job.processes.clone(), job.token.clone());
        tokio::spawn(async move {
            if let Ok(tree) = rx.await {
                processes.add(&token, tree);
            }
        });
        let r = run_session_tracked(engine.as_ref(), &job.bin, &spec, &id, Some(&sink), quiet, Some(tx)).await?;
        *ctx = r.context_tokens;
        Ok(r)
    })
    .await;
    Ok(Attempt {
        answer,
        usage,
        context_tokens,
    })
}

/// A fresh session on the piece, the code gates' rounds, and the push: the run's end.
async fn build(job: &Job, cwd: &Path, branch: &str) -> Result<(RunOutcome, Value)> {
    let base = &job.project.base_branch;
    let base_ref = format!("origin/{base}");
    let where_to_start = start_files::render(
        &start_files::build(crate::git_bin::program(), cwd, &base_ref, &piece_item(&job.piece)).await,
    );
    let start = Start::Piece {
        template: PIECE_TEMPLATE,
        kind: job.kind,
        piece: &job.piece,
    };
    let ids = Mutex::new(String::new());
    let first = session(job, cwd, branch, start, &where_to_start, &ids).await?;
    // A gate round resumes the session of this same run (section 2: a fix round inside a run may resume).
    let rounds = GateRounds {
        cwd,
        base_branch: base,
        code_gates: job.project.code_gates,
        max_check_rounds: job.project.max_check_rounds,
        fresh: None,
    };
    let ids = &ids;
    let resume = |step: Step| {
        let (Step::Fix(follow_up) | Step::FixFresh(follow_up)) = step else {
            unreachable!("a piece run has no fresh-start rules")
        };
        let id = ids.lock().unwrap().clone();
        async move {
            let start = Start::Resume {
                session_id: id,
                follow_up: &follow_up,
                decisions: &[],
            };
            session(job, cwd, branch, start, "", ids).await
        }
    };
    let a = match gate_rounds(&rounds, first, resume).await? {
        Gated::Settled(a, _) => a,
        Gated::GaveUp(_, _, why) => return Ok(failed(why)),
    };
    let pushed = match &a.answer {
        Ok(r) if needs_push(&r.status, commits_ahead(cwd, &base_ref).await) => {
            let title = non_empty(r.change_title.as_deref()).unwrap_or_else(|| job.piece.title.clone());
            let body = non_empty(r.change_body.as_deref()).unwrap_or_else(|| r.summary.clone());
            let (gh, dir, branch, base) = (job.gh.clone(), cwd.to_path_buf(), branch.to_string(), base.clone());
            let published = tokio::task::spawn_blocking(move || {
                publish::publish(crate::git_bin::program(), &gh, &dir, &branch, &base, &title, &body)
                    .map(|change| Pushed { branch, change })
            })
            .await
            .context("publishing the change")?;
            match published {
                Ok(p) => Some(p),
                Err(e) => {
                    return Ok(failed(format!(
                        "The work is committed but couldn't be published: {e:#}"
                    )));
                }
            }
        }
        _ => None,
    };
    let decisions = crate::implementer::piece_decision_list(&job.piece);
    Ok(end_from_answer(a.answer, pushed.as_ref(), &decisions))
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// How many commits the lane's `HEAD` has that `base_ref` hasn't; 0 when git can't say.
async fn commits_ahead(cwd: &Path, base_ref: &str) -> u64 {
    let out = tokio::process::Command::new(crate::git_bin::program())
        .current_dir(cwd)
        .args(["rev-list", "--count", &format!("{base_ref}..HEAD")])
        .output()
        .await;
    out.ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "piece_runs_tests.rs"]
mod tests;
