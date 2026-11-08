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

use super::baseline_wait::BaselineStage;
use super::driver::{RunEnd, RunProcesses, RunRequest, RunStarter, piece_id};
use super::piece_review::{ReviewState, Reviewed, review as review_change};
use super::session::{
    Attempt, ChecksStage, GateRounds, Gated, Step, exists, gate_rounds, piece_branch_name, quiet_timeout,
};
use crate::backlog::runs::LockTake;
use crate::backlog::runs::{RunKind, RunOutcome, RunPiece};
use crate::backlog::{Item, ItemStatus};
use crate::config::{Engine, EngineChoiceConfig, ModelsConfig, ProjectConfig, parse_engine};
use crate::engines::claude::sandbox_enabled;
use crate::engines::resolve::resolve_claude_on_path;
use crate::engines::{EngineId, Role, SessionStart, TranscriptSink, engine_for, engine_of, run_session_tracked};
use crate::env::Env;
use crate::implementer::{Session, Start, publish, run_implementer, session_spec, start_files};
use crate::runs::chore::{Chore, Outside, chore_end, chore_lock_name, chore_of, model_for, tier_for};
use crate::runs::{Pushed, end_from_answer, needs_push};
use crate::{lanes, paths};

const PIECE_TEMPLATE: &str = include_str!("../../prompts/implementer-piece.md");
const CHORE_TEMPLATE: &str = include_str!("../../prompts/chore.md");

/// Runs `build` and `fix` runs of the projects it was built with.
pub struct PieceStarter {
    pub data_dir: PathBuf,
    pub projects: Vec<ProjectConfig>,
    pub choice: EngineChoiceConfig,
    pub env: Env,
    pub max_turns: u32,
    pub models: ModelsConfig,
    pub flags: std::sync::Arc<crate::flags::Flags>,
    /// This computer, for a chore lock's note.
    pub machine: String,
}

impl PieceStarter {
    /// A run on an engine this agent can't run yet isn't refused (that would send it back `interrupted`, to bounce
    /// between computers): it ends `failed`, an outside cause, so the store gives it a look that can move the
    /// piece's engine. With `run-limits-and-causes` Off it is given back as before.
    fn cannot_run(&self, request: RunRequest, engine: String) -> Result<(), String> {
        let (ends, token, flags) = (request.ends, request.token, self.flags.clone());
        tokio::spawn(async move {
            let (outcome, result) = if flags.on("run-limits-and-causes").await {
                let why = format!("this computer can't run {engine} sessions yet");
                (RunOutcome::Failed(why), json!({ "outside": true }))
            } else {
                (RunOutcome::Interrupted, json!({}))
            };
            ends.send(RunEnd { token, outcome, result });
        });
        Ok(())
    }
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
        // The piece's own engine wins; an unknown name is an engine this agent can't run, not Claude.
        let chosen = match request.piece.other.get("engine").and_then(Value::as_str) {
            Some(name) => parse_engine(name).ok_or(name.to_string()),
            None => Ok(engine_for(
                &self.choice,
                project.engines.as_ref(),
                Role::Implementer,
                None,
            )),
        };
        let engine = match chosen {
            Ok(Engine::Claude) => EngineId::Claude,
            Ok(other) => return self.cannot_run(request, other.to_string()),
            Err(name) => return self.cannot_run(request, name),
        };
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
            engine,
            piece: request.piece,
            processes: request.processes,
            flags: self.flags.clone(),
            context: request.context,
            machine: self.machine.clone(),
            lock: None,
        };
        let ends = request.ends;
        let token = request.token;
        // The inner task is joined, so even a panic in it ends in exactly one end.
        tokio::spawn(async move {
            let (outcome, result) = match tokio::spawn(run_locked(job)).await {
                Ok(end) => end,
                Err(e) => (RunOutcome::Failed(format!("the run's task crashed: {e}")), json!({})),
            };
            ends.send(RunEnd { token, outcome, result });
        });
        Ok(())
    }
}

pub(super) struct Job {
    pub(super) data_dir: PathBuf,
    pub(super) project: ProjectConfig,
    pub(super) bin: PathBuf,
    pub(super) gh: PathBuf,
    pub(super) max_turns: u32,
    pub(super) models: ModelsConfig,
    pub(super) lane: PathBuf,
    pub(super) token: String,
    pub(super) kind: RunKind,
    pub(super) engine: EngineId,
    pub(super) piece: RunPiece,
    pub(super) processes: RunProcesses,
    pub(super) flags: std::sync::Arc<crate::flags::Flags>,
    pub(super) context: crate::orchestrator::driver::RunContext,
    pub(super) machine: String,
    /// The token of the chore lock the run holds, checked before it pushes.
    pub(super) lock: Option<String>,
}

/// A sender for a session's process tree: it goes to the run as soon as the session starts, so a stop can cut it.
pub(super) fn tree_sender(job: &Job) -> tokio::sync::oneshot::Sender<std::sync::Arc<crate::process_tree::ProcessTree>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (processes, token) = (job.processes.clone(), job.token.clone());
    tokio::spawn(async move {
        if let Ok(tree) = rx.await {
            processes.add(&token, tree);
        }
    });
    tx
}

fn failed(why: impl Into<String>) -> (RunOutcome, Value) {
    (RunOutcome::Failed(why.into()), json!({}))
}

/// The piece as the item the session helpers want (its id, for "Where to start" and the tier).
pub(super) fn piece_item(piece: &RunPiece) -> Item {
    Item {
        id: piece_id(piece).to_string(),
        title: piece.title.clone(),
        status: ItemStatus::Building,
        ..Item::default()
    }
}

/// A chore holds its kind's store lock through the run (section 6.7): another holder leaves the chore to them
/// (`interrupted`, nothing started), a store that can't be asked fails closed (`failed`, outside). The lock is
/// dropped whatever the run's end. A roadmap piece and a playtest fix take none.
async fn run_locked(mut job: Job) -> (RunOutcome, Value) {
    let chore = if job.flags.on("chore-rotation").await {
        chore_of(&job.piece)
    } else {
        None
    };
    let Some(name) = chore.and_then(|c| chore_lock_name(c.kind, &job.project.base_branch)) else {
        return run(job).await;
    };
    let ctx = job.context.clone();
    let note = format!("{} on {}", piece_id(&job.piece), job.machine);
    let token = match ctx
        .client
        .lock_take(ctx.repo, Some(ctx.token.as_str()), &name, Some(&note))
        .await
    {
        Ok(LockTake::Held(token)) => token,
        Ok(LockTake::HeldBy { by, note }) => {
            let note = note.map(|n| format!(" ({n})")).unwrap_or_default();
            tracing::info!(
                "{}: the {name} lock is held by {by}{note}; leaving the chore to them",
                job.token
            );
            return (RunOutcome::Interrupted, json!({}));
        }
        Err(e) => {
            let why = format!("Couldn't take the {name} lock: {e}");
            return (RunOutcome::Failed(why), json!({ "outside": true }));
        }
    };
    ctx.locks.add(&token, &name, std::time::Instant::now());
    job.lock = Some(token.clone());
    let end = run(job).await;
    let _ = ctx.client.lock_drop(ctx.repo, Some(ctx.token.as_str()), &token).await;
    ctx.locks.remove(&token);
    end
}

/// The run's end: its lane is prepared on the piece's branch, a session runs, the gates check it and its change is
/// published. A failure before any session ran is this computer's own (`interrupted`, the run goes back); after
/// it, the run `failed`.
async fn run(job: Job) -> (RunOutcome, Value) {
    // A piece with a pushed branch carries on from it, whatever the run's kind: a fresh branch of the same name
    // would be rejected as non-fast-forward when published.
    let (branch, start_point) = match (job.kind, job.piece.branch.as_deref()) {
        (_, Some(b)) => (b.to_string(), format!("origin/{b}")),
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
    let (outcome, result) = match build(&job, &cwd, &branch).await {
        Ok(end) => end,
        Err(e) if e.is::<Outside>() => (RunOutcome::Failed(format!("{e:#}")), json!({ "outside": true })),
        Err(e) => failed(format!("{e:#}")),
    };
    // A chore never waits on a person (section 6.7): its questions or steps become a failure, its PR closed.
    let chore = if job.flags.on("chore-rotation").await {
        chore_of(&job.piece)
    } else {
        None
    };
    let Some(chore) = chore else {
        return (outcome, result);
    };
    let waits = matches!(outcome, RunOutcome::Questions | RunOutcome::PersonSteps);
    let outcome = chore_end(chore.kind, outcome, &result);
    if let (true, RunOutcome::Failed(why)) = (waits, &outcome) {
        let (gh, dir, branch, why) = (job.gh.clone(), cwd, branch, why.clone());
        let comment = format!("Closed: a chore never waits on a person. {why}");
        let closed = tokio::task::spawn_blocking(move || publish::close_change(&gh, &dir, &branch, &comment)).await;
        if let Err(e) = closed.map_err(anyhow::Error::from).and_then(|r| r) {
            tracing::warn!("{}: closing the chore's change: {e:#}", job.token);
        }
    }
    (outcome, result)
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

/// The piece's chore, with `chore-rotation` On; Off, a chore piece is run like any other.
async fn chore_for(job: &Job) -> Option<Chore> {
    if job.flags.on("chore-rotation").await {
        chore_of(&job.piece)
    } else {
        None
    }
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
    // With `chore-rotation` Off, a chore piece is run like any other.
    let chore = chore_for(job).await;
    let tier = tier_for(chore.as_ref(), now, &job.models);
    let model = model_for(chore.as_ref(), tier, &item).to_string();
    let own_prompt = matches!(start, Start::Chore { .. });
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
            engine: Some(job.engine),
            cursor: None,
            local: None,
            sandbox: sandbox_enabled(project.sandbox, cfg!(windows)),
        },
        start,
    )
    // Our own template, not the work: the run ends `failed`, `outside`.
    .map_err(|e| {
        if own_prompt {
            anyhow::Error::new(Outside(format!("{e:#}")))
        } else {
            e
        }
    })?;
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
    let engine = engine_of(Some(job.engine))?;
    let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
    let mut context_tokens = None;
    let ctx = &mut context_tokens;
    let (answer, usage) = run_implementer(spec, |spec| async move {
        let r = run_session_tracked(
            engine.as_ref(),
            &job.bin,
            &spec,
            &id,
            Some(&sink),
            quiet,
            Some(tree_sender(job)),
        )
        .await?;
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
    let chore = chore_for(job).await.filter(Chore::has_prompt);
    let start = match &chore {
        Some(chore) => Start::Chore {
            template: CHORE_TEMPLATE,
            kind: job.kind,
            piece: &job.piece,
            chore,
        },
        None => Start::Piece {
            template: PIECE_TEMPLATE,
            kind: job.kind,
            piece: &job.piece,
        },
    };
    let ids = Mutex::new(String::new());
    let first = session(job, cwd, branch, start, &where_to_start, &ids).await?;
    // A gate round resumes the session of this same run (section 2: a fix round inside a run may resume).
    // Stage 1 of review runs in the lane after the gates; its commands' trees go to the run like the session's.
    let stage_one = Mutex::new(None);
    let (processes, token) = (job.processes.clone(), job.token.clone());
    let on_tree = std::sync::Arc::new(move |tree| processes.add(&token, tree));
    let project_id = job.project.id_or_derived();
    let (repo_path, worktrees_dir) = (
        PathBuf::from(&job.project.repo_path),
        PathBuf::from(&job.project.worktrees_dir),
    );
    let stage = ChecksStage {
        checks: &job.project.checks,
        backlog_path: &job.project.backlog_path,
        on_tree,
        result: &stage_one,
        baseline: job.flags.on("chore-rotation").await.then(|| BaselineStage {
            project: &project_id,
            repo_path: &repo_path,
            worktrees_dir: &worktrees_dir,
            checks: &job.project.checks,
        }),
    };
    let rounds = GateRounds {
        cwd,
        base_branch: base,
        code_gates: job.project.code_gates,
        max_check_rounds: job.project.max_check_rounds,
        fresh: None,
        checks: Some(&stage),
    };
    let ids = &ids;
    let resume = |step: Step| {
        let (Step::Fix(follow_up) | Step::FixFresh(follow_up, _)) = step else {
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
    let decisions = crate::implementer::piece_decision_list(&job.piece);
    let mut review = ReviewState::default();
    let mut gated = gate_rounds(&rounds, first, &resume).await?;
    loop {
        let a = match gated {
            Gated::Settled(a, _) => a,
            Gated::GaveUp(_, _, why) => return Ok(failed(why)),
        };
        let pushed = match &a.answer {
            Ok(_) if job.lock.as_ref().is_some_and(|t| !job.context.locks.held(t)) => {
                tracing::warn!("{}: the chore's lock was lost; nothing is pushed", job.token);
                return Ok((RunOutcome::Interrupted, json!({})));
            }
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
        let described = a
            .answer
            .as_ref()
            .ok()
            .map(|r| (r.slice_description.clone(), r.playtest.clone()));
        // With `run-limits-and-causes` Off, a usage limit ends `failed` as before.
        let limited_as = job
            .flags
            .on("run-limits-and-causes")
            .await
            .then(|| job.engine.to_string());
        let (outcome, mut result) = end_from_answer(
            a.answer,
            pushed.as_ref(),
            &decisions,
            limited_as.as_deref(),
            chrono::Utc::now(),
        );
        let (RunOutcome::PrOpened, Some(pushed), Some((description, playtest))) = (&outcome, &pushed, described) else {
            return Ok((outcome, result));
        };
        // The checks' evidence goes to review and to the run's result.
        let stage = stage_one.lock().unwrap().take();
        let checks = stage
            .as_ref()
            .map(|s| crate::checks::checks_summary(&s.checks))
            .unwrap_or_default();
        match review_change(job, cwd, pushed, &description, playtest.as_ref(), &checks, &mut review).await? {
            Reviewed::Approved => {
                if let Some(stage) = stage {
                    result["checks_summary"] = json!(checks);
                    result["diff_stat"] = json!(stage.diff_stat);
                }
                return Ok((outcome, result));
            }
            Reviewed::Ends(outcome, result) => return Ok((outcome, result)),
            Reviewed::Changes(follow_up) => {
                // A fix round inside the run: the same session, then the gates and checks again.
                let id = ids.lock().unwrap().clone();
                let start = Start::Resume {
                    session_id: id,
                    follow_up: &follow_up,
                    decisions: &[],
                };
                let next = session(job, cwd, branch, start, "", ids).await?;
                gated = gate_rounds(&rounds, next, &resume).await?;
            }
        }
    }
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

#[cfg(all(test, unix))]
#[path = "piece_runs_tests.rs"]
mod tests;
