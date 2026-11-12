//! The real implementer session starter for `claim` mode (`startItem`/`implement` in Retaskable.com's
//! `agent/src/steps.ts`): prepares the lane, builds the prompt, runs `claude -p` and turns what it
//! answered into the [`SessionOutcome`] the loop settles. The session runs on the engine the request names,
//! placed by [`crate::engines::place`] (Claude Code, Cursor, pi or Codex).
//! A fresh session's prompt ends with "Where to start" ([`crate::implementer::start_files`]), built in
//! the prepared lane; a resume or a follow-up restart gets none.
//! Left out: Claude's sandbox, and the decisions a person already gave
//! (the claimed item doesn't carry them). The tier the store names on the claim (section 7.1, `sonnet` when it names none) is the
//! implementer's, then moved off Opus for a first try ([`crate::tiers::implementer_model`]).
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use serde_json::Value;

pub(crate) use super::cursor_guard::watch_for;
use super::driver::{Abandon, SessionOutcome, SessionResult, SessionStarter, StartRequest};
use super::fresh_start::{MAX_TURNS_FOLLOW_UP, picking_up_prompt};
use super::steps::StepRequest;
use crate::backlog::Item;
use crate::config::{CursorModels, Engine, EngineChoiceConfig, ProjectConfig};
use crate::decisions::split_asked;
use crate::engines::claude::sandbox_enabled;
use crate::engines::model_slots::ModelSlots;
use crate::engines::{
    CouldNotStart, EngineId, Role, SessionStart, TranscriptSink, engine_for, engine_of, lane_engine, place, program_of,
    run_session,
};
use crate::env::Env;
use crate::implementer::abandon::abandon;
use crate::implementer::{
    CursorTimeKilled, ImplementerResult, Interrupted, LimitHit, MaxTurns, Session, SilenceKilled, Start,
    new_session_id, run_implementer, session_spec,
};
use crate::implementer::{publish, start_files};
use crate::item_lifecycle::ImplementerOutcome;
use crate::orchestrator::live::sessions::{SessionInfo, Sessions, model_name};
use crate::telemetry::now_ms;
use crate::tiers::{LOCAL, floor, implementer_model, named_tier};
use crate::{lanes, paths};
pub(super) use gates::{ChecksStage, FreshRules, GateRounds, Gated, Step, gate_rounds};

const IMPLEMENTER_TEMPLATE: &str = include_str!("../../prompts/implementer.md");

/// Runs Claude implementer sessions for the claimed items of the projects it was built with.
pub struct ClaudeStarter {
    pub(super) data_dir: PathBuf,
    pub(super) projects: Vec<ProjectConfig>,
    pub(super) choice: EngineChoiceConfig,
    pub(super) env: Env,
    max_turns: u32,
    /// `MachineConfig::max_resume_context_tokens`: a send-back to a session over this size starts fresh.
    max_resume_context_tokens: u32,
    /// The machine's Cursor model names, for placing a Cursor session.
    pub(super) cursor_models: CursorModels,
    /// Sessions started and not yet ended (the loop's `activeSessions`).
    pub(super) running: Arc<AtomicU32>,
    /// The sessions running now: each one lists itself while it runs.
    pub(super) sessions: Sessions,
    /// The computer's local model, for the tier ladder.
    pub(super) local: crate::local_model::LocalModel,
}

impl ClaudeStarter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        data_dir: PathBuf,
        projects: Vec<ProjectConfig>,
        choice: EngineChoiceConfig,
        env: Env,
        max_turns: u32,
        max_resume_context_tokens: u32,
        cursor_models: CursorModels,
        sessions: Sessions,
        local: crate::local_model::LocalModel,
    ) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            projects,
            choice,
            env,
            max_turns,
            max_resume_context_tokens,
            cursor_models,
            running: Arc::new(AtomicU32::new(0)),
            sessions,
            local,
        })
    }
}

/// Whether a fresh Claude implementer may start: `sonnet`, what a claim with no tier (and a first try) runs on, isn't
/// capped and full. The tier arrives with the claim, so it isn't known before it. `pending` is the starts already made
/// that haven't taken their slot yet.
pub fn model_has_room(slots: Option<&ModelSlots>, pending: u32) -> bool {
    slots.is_none_or(|s| s.has_room_after(crate::tiers::SONNET, pending))
}

impl SessionStarter for ClaudeStarter {
    fn implementer_may_start(&self, pending: u32) -> bool {
        model_has_room(crate::engines::model_slots::global(), pending)
    }

    fn start(&self, request: StartRequest) -> Result<(), String> {
        let project = self
            .projects
            .iter()
            .find(|p| p.id_or_derived() == request.project)
            .ok_or_else(|| format!("no project {} is configured", request.project))?
            .clone();
        // Fail before anything starts when the engine's binary isn't there: the claim goes back.
        place(
            config_engine(request.engine),
            Role::Implementer,
            &self.env,
            &self.cursor_models,
        )?;
        // The implementer's engine is set to `local` (it is Claude Code): it takes the local model while it's up.
        let wants_local = !request.resume
            && request.engine == EngineId::Claude
            && engine_for(
                &self.choice,
                project.engines.as_ref(),
                Role::Implementer,
                Some(request.lane),
            ) == Engine::Local;
        let StartRequest {
            project: project_id,
            item_id,
            lane,
            item,
            outcomes,
            session_id,
            engine: session_engine,
            resume,
            follow_up,
            items,
            attempt,
            notes,
            takeover,
            ..
        } = request;
        let job = Job {
            local: self.local.clone(),
            wants_local,
            data_dir: self.data_dir.clone(),
            project,
            project_id: project_id.clone(),
            env: self.env.clone(),
            cursor_models: self.cursor_models.clone(),
            gh: self
                .env
                .get("RETASKABLE_GH")
                .map_or_else(|| PathBuf::from("gh"), PathBuf::from),
            max_turns: self.max_turns,
            lane,
            item,
            max_resume_context_tokens: self.max_resume_context_tokens,
            session_id,
            engine: session_engine,
            lane_engine: lane_engine(&self.choice, lane).map(program_of),
            takeover,
            resume,
            follow_up,
            items,
            attempt,
            notes,
            sessions: self.sessions.clone(),
        };
        // The inner task is joined, so even a panic in it ends in exactly one outcome, and one count down.
        let running = self.running.clone();
        running.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            // A fresh start's failure here is before any session ran (an own failure, section 7.3): it goes
            // back to the queue. A resumed session already ran once, so the same failure here may have left
            // real work behind, and needs a person instead.
            let (result, usage, context_tokens, notes, new_session) = match tokio::spawn(run(job)).await {
                Ok(Ok(ran)) => ran,
                Ok(Err(e)) if resume => (
                    SessionResult::NeedsHuman(format!("the session couldn't resume: {e:#}")),
                    None,
                    None,
                    vec![],
                    None,
                ),
                Ok(Err(e)) => (SessionResult::Failed(format!("{e:#}")), None, None, vec![], None),
                Err(e) if resume => (
                    SessionResult::NeedsHuman(format!("the session's task crashed while resuming: {e}")),
                    None,
                    None,
                    vec![],
                    None,
                ),
                Err(e) => (
                    SessionResult::Failed(format!("the session task crashed: {e}")),
                    None,
                    None,
                    vec![],
                    None,
                ),
            };
            running.fetch_sub(1, Ordering::SeqCst);
            outcomes.send(SessionOutcome {
                project: project_id,
                item_id,
                lane,
                result,
                context_tokens,
                usage,
                notes,
                session_id: new_session,
            });
        });
        Ok(())
    }

    fn triage(&self, request: super::driver::TriageRequest) -> Result<(), String> {
        self.start_triage(request)
    }

    fn second_look(&self, request: super::driver::SecondLookRequest) -> Result<(), String> {
        self.start_second_look(request)
    }

    fn active(&self) -> u32 {
        self.running.load(Ordering::SeqCst)
    }

    fn abandon(&self, old: Abandon) -> BoxFuture<'static, Result<(), String>> {
        let repo = self
            .projects
            .iter()
            .find(|p| p.id_or_derived() == old.project)
            .map(|p| PathBuf::from(&p.repo_path));
        let gh = self
            .env
            .get("RETASKABLE_GH")
            .map_or_else(|| PathBuf::from("gh"), PathBuf::from);
        Box::pin(async move {
            let repo = repo.ok_or_else(|| format!("no project {} is configured", old.project))?;
            tokio::task::spawn_blocking(move || {
                abandon(
                    crate::git_bin::program(),
                    &gh,
                    &repo,
                    old.branch.as_deref(),
                    old.pr_url.as_deref(),
                )
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))
        })
    }

    /// Checks, CI, review and merge aren't built in the Rust agent yet: a coded item waits in `checking`.
    fn advance(&self, _request: StepRequest) -> Option<BoxFuture<'static, crate::orchestrator::steps::StepDone>> {
        None
    }
}

/// The configured engine whose program is `id`.
fn config_engine(id: EngineId) -> Engine {
    match id {
        EngineId::Claude => Engine::Claude,
        EngineId::Cursor => Engine::Cursor,
        EngineId::Pi => Engine::Pi,
        EngineId::Codex => Engine::Codex,
    }
}

/// Whether the resolved binary is there: a path by itself, a bare name somewhere on `PATH`.
pub(crate) fn exists(bin: &Path, env: &Env) -> bool {
    if bin.components().count() > 1 {
        return bin.is_file();
    }
    crate::env_path::path_dirs(env).iter().any(|dir| {
        let found = dir.join(bin);
        found.is_file() || found.with_extension("exe").is_file()
    })
}

struct Job {
    local: crate::local_model::LocalModel,
    /// The implementer's engine is set to `local` (`Placement::local`).
    wants_local: bool,
    data_dir: PathBuf,
    project: ProjectConfig,
    project_id: String,
    env: Env,
    cursor_models: CursorModels,
    /// The `gh` program (`RETASKABLE_GH`, or `gh` on PATH).
    gh: PathBuf,
    max_turns: u32,
    lane: u32,
    item: Item,
    max_resume_context_tokens: u32,
    /// The id the session runs under (`--session-id`, or `--resume` when `resume`).
    session_id: String,
    /// The engine the session runs on (`StartRequest::engine`).
    engine: EngineId,
    /// The engine the lane now has of its own, if any: a send-back resumes only if it matches `engine`.
    lane_engine: Option<EngineId>,
    takeover: Option<EngineId>,
    resume: bool,
    /// `None` prepares the lane fresh, as a newly claimed item does. `Some` is the picking-up prompt for a
    /// restart after the lane's prior session ran out of turns or had grown too large: the lane is reused
    /// as-is instead.
    follow_up: Option<String>,
    /// Every item in the project's backlog; the unbuilt ones are listed in a fresh prompt, and a `waits_on`
    /// result is checked against them.
    items: Vec<Item>,
    /// Start over's attempt (the branch's `-r<attempt>`), and its notes for a fresh session's first prompt.
    attempt: u32,
    notes: Option<String>,
    sessions: Sessions,
}

/// The watchdog's timeout (07-orchestrator-loop.md's `tuning.sessionQuietMinutes`): 0 turns it off.
pub(super) fn quiet_timeout(minutes: u32) -> Option<std::time::Duration> {
    (minutes > 0).then(|| std::time::Duration::from_secs(u64::from(minutes) * 60))
}

/// The follow-up an interrupted implementer is resumed with (`INTERRUPTED` in `steps.ts`).
const INTERRUPTED: &str = "You were interrupted. Continue from where you left off, then return the result.";

/// A title as a branch-name slug: lowercase words joined by `-`, at most 40 characters.
fn title_slug(title: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(40)
        .collect();
    slug.trim_end_matches('-').to_string()
}

/// A piece's branch: `retaskable/<piece id>-<slug of the title>` (pieces.md: one branch per piece, kept for its
/// `fix` runs).
pub(super) fn piece_branch_name(piece_id: &str, title: &str) -> String {
    format!("retaskable/{piece_id}-{}", title_slug(title))
}

/// `branchName`: `retaskable/<id>-<slug of the title>-s<slice>`, then `-r<attempt>` once a person has started the
/// slice over.
pub(super) fn branch_name(item: &Item, attempt: u32) -> String {
    let slug = title_slug(&item.title);
    let retry = if attempt > 0 {
        format!("-r{attempt}")
    } else {
        String::new()
    };
    format!("retaskable/{}-{}-s{}{retry}", item.id, slug, item.slice)
}

/// One implementer session in `cwd`: builds its spec from `start`, runs it with its transcript at
/// `<run folder>/<name>`, and returns its answer with the stream's usage reading and the session's last
/// context size. A `fresh` start runs under the id the driver chose and saved, so a restart of it can
/// resume it. It runs on `engine`, placed afresh: a fresh start can have moved to another engine than the job's.
#[allow(clippy::too_many_arguments)]
async fn attempt(
    job: &Job,
    engine: EngineId,
    cwd: &Path,
    start: Start<'_>,
    session_id: &str,
    fresh: bool,
    name: String,
    where_to_start: &str,
) -> Result<Attempt> {
    let Job {
        data_dir,
        project,
        project_id,
        env,
        cursor_models,
        max_turns,
        lane,
        item,
        items,
        notes,
        attempt: attempt_no,
        sessions,
        ..
    } = job;
    let placed = place(config_engine(engine), Role::Implementer, env, cursor_models).map_err(anyhow::Error::msg)?;
    let cursor = placed.cursor.map(|c| crate::engines::cursor::with_project(project, c));
    // Only a Claude session may use the local model; its slot is held until
    // the session ends. A `local` model is started as `--model sonnet` by the engine, whose environment maps it onto the model.
    // A resume stays on the backend its first transcript line names: a local session goes back to the model while
    // it's up (else its Anthropic tier), an Anthropic one (or one with no readable transcript) never goes local.
    let ladder = placed.engine == EngineId::Claude;
    let up = ladder && job.local.available();
    let resumed = matches!(start, Start::Resume { .. });
    let was_local = resumed
        && crate::engines::transcripts::started_on(data_dir, project_id, &item.id, session_id)
            .is_some_and(|s| s.model == LOCAL);
    let tier = if up && (if resumed { was_local } else { job.wants_local }) {
        LOCAL
    } else {
        // The store named the tier (none is `sonnet`); `local` without the model up is `sonnet`. Claude at its
        // limit reads as the floor tier for a fresh session only.
        let named = floor(
            named_tier(item.tier.as_deref()),
            !resumed && sessions.claude_at_limit(),
            up && !resumed,
        );
        if named == LOCAL && !(up && !resumed) {
            crate::tiers::SONNET
        } else {
            named
        }
    };
    let local_run = if tier == LOCAL {
        Some(job.local.run().await)
    } else {
        None
    };
    let model = implementer_model(tier, item, *attempt_no).to_string();
    let info = SessionInfo {
        project: project_id.clone(),
        item_id: item.id.clone(),
        title: Some(item.title.clone()),
        role: Role::Implementer,
        lane: Some(*lane),
        engine: placed.engine,
        model: if local_run.is_some() {
            LOCAL.to_string()
        } else {
            model_name(placed.engine, &model, cursor.as_ref())
        },
    };
    let branch = branch_name(item, *attempt_no);
    let quiet = watch_for(project, placed.engine);
    let mut spec = session_spec(
        &Session {
            template: IMPLEMENTER_TEMPLATE,
            project_type: project.project_type,
            item,
            branch: &branch,
            base_branch: &project.base_branch,
            github_repo: &project.github_repo,
            checks: &project.checks,
            code_gates: project.code_gates,
            where_to_start,
            notes: notes.as_deref(),
            items,
            cwd,
            model,
            budget_usd: project.budget.implementer_usd,
            max_turns: Some(*max_turns),
            engine: Some(placed.engine),
            cursor,
            local: local_run.as_ref().map(|r| r.target.clone()),
            sandbox: sandbox_enabled(project.sandbox, cfg!(windows)),
        },
        start,
    )?;
    if fresh {
        spec.start = SessionStart::Fresh(session_id.to_string());
    }
    let sink = TranscriptSink {
        path: paths::run_dir(data_dir, project_id, &item.id).join(name),
        lane: Some(*lane),
    };
    let runner = engine_of(Some(placed.engine))?;
    let (runner, bin, id, sink) = (runner.as_ref(), &placed.bin, session_id, &sink);
    let mut context_tokens = None;
    let ctx = &mut context_tokens;
    let (answer, usage) = sessions
        .track(
            info,
            run_implementer(spec, |spec| async move {
                let r = run_session(runner, bin, &spec, id, Some(sink), quiet).await?;
                *ctx = r.context_tokens;
                Ok(r)
            }),
        )
        .await;
    Ok(Attempt {
        answer,
        usage,
        context_tokens,
    })
}

/// How an [`attempt`] ended.
pub(super) struct Attempt {
    pub(super) answer: Result<ImplementerResult>,
    pub(super) usage: Option<Value>,
    pub(super) context_tokens: Option<u64>,
}

/// What `run` returns: the result, the stream's usage reading, the session's last context size and the lines
/// for the item's timeline (the code gates' notes).
/// and the id of the fresh session the job carried on in, if it did.
type Ran = (SessionResult, Option<Value>, Option<u64>, Vec<String>, Option<String>);

pub(super) fn ran_out_of_turns(a: &Attempt) -> bool {
    matches!(&a.answer, Err(e) if e.downcast_ref::<MaxTurns>().is_some())
}

fn settled(a: Attempt) -> Ran {
    let Attempt {
        answer,
        usage,
        context_tokens,
    } = a;
    let (result, usage) = session_result_for_answer(answer, usage);
    (result, usage, context_tokens, vec![], None)
}

/// The folder a session runs in: an `existing` lane kept as it is, after stopping anything the old session left
/// running in it, or else the lane prepared on the item's branch (the branch keeps the commits).
async fn lane_for(job: &Job, existing: Option<PathBuf>) -> Result<PathBuf> {
    match existing {
        Some(dir) => {
            let stop = dir.clone();
            tokio::task::spawn_blocking(move || lanes::kill_processes_in(&stop))
                .await
                .context("stopping what the old session left running")?
                .context("stopping what the old session left running")?;
            Ok(dir)
        }
        None => {
            let (repo, worktrees) = (
                PathBuf::from(&job.project.repo_path),
                PathBuf::from(&job.project.worktrees_dir),
            );
            let (base, branch, lane) = (
                job.project.base_branch.clone(),
                branch_name(&job.item, job.attempt),
                job.lane,
            );
            tokio::task::spawn_blocking(move || lanes::prepare(&repo, &worktrees, lane, &branch, &base))
                .await
                .context("preparing the lane")?
                .context("preparing the lane")
        }
    }
}

async fn gated(job: &Job, cwd: &Path, a: Attempt) -> Result<Ran> {
    let rounds = GateRounds {
        cwd,
        base_branch: &job.project.base_branch,
        code_gates: job.project.code_gates,
        max_check_rounds: job.project.max_check_rounds,
        fresh: Some(FreshRules {
            max_turns: job.max_turns,
            max_resume_context_tokens: job.max_resume_context_tokens,
            engine: job.engine,
            lane_engine: job.lane_engine,
            takeover: job.takeover,
        }),
        checks: None,
    };
    // The session now answering, and whether it is a fresh one (so its id is reported).
    let state = std::sync::Mutex::new((job.session_id.clone(), false, job.engine));
    let next = |step: Step| {
        let (current, _, mut engine) = state.lock().unwrap().clone();
        let (follow_up, fresh_prompt) = match &step {
            Step::OutOfTurns => (
                picking_up_prompt(job.item.pr_url.is_some(), &job.project.base_branch, MAX_TURNS_FOLLOW_UP),
                true,
            ),
            Step::FixFresh(f, to) => {
                engine = to.unwrap_or(engine);
                (picking_up_prompt(false, &job.project.base_branch, f), true)
            }
            Step::Fix(f) => (f.clone(), false),
        };
        let id = if fresh_prompt { new_session_id() } else { current };
        if fresh_prompt {
            *state.lock().unwrap() = (id.clone(), true, engine);
        }
        async move {
            if fresh_prompt {
                let start = Start::Continuing { follow_up: &follow_up };
                attempt(job, engine, cwd, start, &id, true, format!("{id}.jsonl"), "").await
            } else {
                let start = Start::Resume {
                    session_id: id.clone(),
                    follow_up: &follow_up,
                    decisions: &[],
                };
                let name = format!("{id}-resumed-{}.jsonl", now_ms());
                attempt(job, engine, cwd, start, &id, false, name, "").await
            }
        }
    };
    let gated = gate_rounds(&rounds, a, next).await?;
    let (current, started_fresh, _) = state.into_inner().unwrap();
    let (a, mut notes) = match gated {
        Gated::Settled(a, notes) => (a, notes),
        Gated::GaveUp(a, notes, why) => {
            return Ok((
                SessionResult::NeedsHuman(why),
                a.usage,
                a.context_tokens,
                notes,
                started_fresh.then_some(current),
            ));
        }
    };
    let titles = match &a.answer {
        Ok(r) if r.status == "pr_opened" => Some((r.change_title.clone(), r.change_body.clone(), r.summary.clone())),
        _ => None,
    };
    let mut ran = settled(a);
    if let Some((title, body, summary)) = titles {
        // The gates passed (or couldn't run): push the branch and open its PR, or find the one already open. A
        // failure here means a session ran and its work is committed, so a person is needed, not the queue.
        let title = title
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("{}: {}", job.item.id, job.item.title));
        let body = body
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty())
            .unwrap_or(summary);
        let (gh, dir, branch, base) = (
            job.gh.clone(),
            cwd.to_path_buf(),
            branch_name(&job.item, job.attempt),
            job.project.base_branch.clone(),
        );
        let published = tokio::task::spawn_blocking(move || {
            publish::publish(crate::git_bin::program(), &gh, &dir, &branch, &base, &title, &body)
        })
        .await
        .context("publishing the change")?;
        match published {
            Ok(p) => notes.push(format!(
                "{} PR #{}: {}",
                if p.created { "Opened" } else { "Pushed to the open" },
                p.number,
                p.url
            )),
            Err(e) => {
                ran.0 = SessionResult::NeedsHuman(format!("The work is committed but couldn't be published: {e:#}"))
            }
        }
    }
    ran.3 = notes;
    ran.4 = started_fresh.then_some(current);
    Ok(ran)
}

async fn run(job: Job) -> Result<Ran> {
    let worktrees = PathBuf::from(&job.project.worktrees_dir);
    let lane = job.lane;
    if let Some(follow_up) = &job.follow_up {
        // A restart after running out of turns, or of a session that had grown too large, reuses the lane
        // exactly as it was left (stopping what the old session left running in it): `lanes::prepare` would
        // hard-reset the branch, discarding the prior session's uncommitted work. It's only prepared if the
        // folder is gone. Never resumes: it always runs under a fresh id.
        let existing = Some(lanes::lane_path(&worktrees, lane)).filter(|d| d.is_dir());
        let cwd = lane_for(&job, existing).await?;
        let name = format!("{}.jsonl", job.session_id);
        let start = Start::Continuing {
            follow_up: follow_up.as_str(),
        };
        let a = attempt(&job, job.engine, &cwd, start, &job.session_id, true, name, "").await?;
        return gated(&job, &cwd, a).await;
    }
    // A restart resumes the session where it left off: the lane is kept as it is (preparing it would
    // reset away the work), and anything the old session left running in it is stopped. The driver decides
    // whether to resume at all, weighing the record's last known size against `maxResumeContextTokens` before
    // ever calling this with `resume: true`; a transcript Claude Code no longer has is handled below.
    let mut resuming = job
        .resume
        .then(|| lanes::lane_path(&worktrees, lane))
        .filter(|d| d.is_dir());
    loop {
        let cwd = lane_for(&job, resuming.clone()).await?;
        let resumed = resuming.is_some();
        let start = if resumed {
            Start::Resume {
                session_id: job.session_id.clone(),
                follow_up: INTERRUPTED,
                decisions: &[],
            }
        } else {
            Start::Fresh
        };
        // A resumed session's transcript is a new file: the run folder keeps the earlier one whole.
        let name = if resumed {
            format!("{}-resumed-{}.jsonl", job.session_id, now_ms())
        } else {
            format!("{}.jsonl", job.session_id)
        };
        // Only a fresh start gets "Where to start". Building it reads the lane's specs and runs git; git
        // errors leave the list shorter (`build` skips what it can't read), never failing the item.
        let where_to_start = if resumed {
            String::new()
        } else {
            let base_ref = format!("origin/{}", job.project.base_branch);
            start_files::render(&start_files::build(crate::git_bin::program(), &cwd, &base_ref, &job.item).await)
        };
        let a = attempt(
            &job,
            job.engine,
            &cwd,
            start,
            &job.session_id,
            !resumed,
            name,
            &where_to_start,
        )
        .await?;
        // The old session was stopped before Claude Code saved it (a restart moments after it began): the
        // same id starts a fresh one, as `implement` in `steps.ts` does on "No conversation found".
        if resumed && matches!(&a.answer, Err(e) if format!("{e:#}").contains("No conversation found")) {
            resuming = None;
            continue;
        }
        return gated(&job, &cwd, a).await;
    }
}

/// What the loop does with a session's answer (`implement` in `steps.ts`). `failed` is `needsHuman("Implementer
/// gave up")`, `person_steps` is `waitForSteps` (a needs_human with the steps), `waits_on` records the dependency
/// and gives the item back (no `depends` op here yet, so it goes to a person), and `nothing_left` marks the item
/// done and merged (not built here yet, so it goes to a person too). A status this agent doesn't recognize also
/// needs a person: the session ran, so this is never the before-work own failure that goes back to the queue.
fn classify(r: &ImplementerResult) -> SessionResult {
    match r.status.as_str() {
        "pr_opened" => SessionResult::Implementer(ImplementerOutcome::Finished),
        "needs_clarification" => {
            let asked: Vec<String> = r.questions.iter().map(|q| q.question.clone()).collect();
            // The claimed item carries no decisions yet, so nothing counts as already answered.
            let (answered, open) = split_asked(&asked, &[]);
            SessionResult::Implementer(ImplementerOutcome::Questions {
                all_already_answered: !answered.is_empty() && open.is_empty(),
            })
        }
        "failed" => SessionResult::NeedsHuman(format!("Implementer gave up: {}", r.summary)),
        "nothing_left" => SessionResult::NeedsHuman(format!("Nothing left to build, so mark it done: {}", r.summary)),
        "person_steps" if !r.person_steps.is_empty() => {
            let steps: Vec<String> = r
                .person_steps
                .iter()
                .enumerate()
                .map(|(i, s)| format!("{}. {s}", i + 1))
                .collect();
            SessionResult::NeedsHuman(format!("Only steps a person can do are left:\n{}", steps.join("\n")))
        }
        "person_steps" => SessionResult::NeedsHuman(format!("Implementer gave up: {}", r.summary)),
        "waits_on" => {
            let needs = if r.waits_on.is_empty() {
                "another item".to_string()
            } else {
                r.waits_on.join(", ")
            };
            SessionResult::NeedsHuman(format!(
                "The implementer found this item needs {needs} built first: {}",
                r.summary
            ))
        }
        // An answer in a shape this agent doesn't know: a session ran, so this needs a person rather
        // than going back to the queue.
        other => SessionResult::NeedsHuman(format!(
            "The session answered with {other}, which needs a person: {}",
            r.summary
        )),
    }
}

/// What `run_implementer`'s answer means for the item (`run`'s return): the watchdog's own kill
/// ([`SilenceKilled`]) is an interruption, not a failure: it keeps its lane and lease and is resumed
/// by id a minute later, up to 3 times in a row before a person (07-orchestrator-loop.md's watchdog). A
/// cut session ([`Interrupted`], 07.4-stopping-and-restarting.md) keeps its lease and run record as
/// they are, with nothing posted to the store, so it's resumed by id on the next restart. Any other
/// error means a session ran and didn't come back clean, so it needs a person instead (it may have
/// left real work behind, section 7.3). `usage` travels alongside whatever `answer` is: a session
/// that failed may still have reported a usage reading before it did, and that reading is often
/// exactly the one section 7.1's tier needs most.
fn session_result_for_answer(
    answer: Result<ImplementerResult>,
    usage: Option<Value>,
) -> (SessionResult, Option<Value>) {
    match answer {
        Err(e) if e.downcast_ref::<Interrupted>().is_some() => (SessionResult::Interrupted, usage),
        Err(e) if e.downcast_ref::<LimitHit>().is_some() => {
            let resets_in = e.downcast_ref::<LimitHit>().and_then(|hit| hit.resets_in);
            let why = format!("The session ended without answering: {e:#}");
            (SessionResult::Limited { why, resets_in }, usage)
        }
        Err(e) if e.downcast_ref::<CouldNotStart>().is_some() => (
            SessionResult::NotStarted(format!("The session ended without answering: {e:#}")),
            usage,
        ),
        Err(e) if e.downcast_ref::<SilenceKilled>().is_some() => (
            SessionResult::Silenced(format!("went quiet and was killed: {e:#}"), "killed_for_silence"),
            usage,
        ),
        Err(e) if e.downcast_ref::<CursorTimeKilled>().is_some() => (
            SessionResult::Silenced(format!("ran past its time limit and was killed: {e:#}"), "cursor_time"),
            usage,
        ),
        Err(e) => (
            SessionResult::NeedsHuman(format!("The session ended without answering: {e:#}")),
            usage,
        ),
        Ok(r) => (classify(&r), usage),
    }
}

#[path = "session_gates.rs"]
mod gates;

#[cfg(test)]
#[path = "session_tests.rs"]
pub(super) mod tests;

#[cfg(test)]
#[path = "session_local_tests.rs"]
mod local_tests;

#[cfg(test)]
#[path = "session_engine_tests.rs"]
mod engine_tests;

#[cfg(test)]
#[path = "chat_engine_tests.rs"]
mod chat_engine_tests;
