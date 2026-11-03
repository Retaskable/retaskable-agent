//! The real implementer session starter for `claim` mode (`startItem`/`implement` in Retaskable.com's
//! `agent/src/steps.ts`): prepares the lane, builds the prompt, runs `claude -p` and turns what it
//! answered into the [`SessionOutcome`] the loop settles. Claude only; any other engine refuses.
//! A fresh session's prompt ends with "Where to start" ([`crate::implementer::start_files`]), built in
//! the prepared lane; a resume or a follow-up restart gets none.
//! Left out: Claude's sandbox, and the decisions a person already gave
//! (the claimed item doesn't carry them). The usage-window model tier (section 7.1) is picked for the
//! implementer ([`crate::tiers::implementer_tier`]), then moved off Opus for a first try
//! ([`crate::tiers::implementer_model`]); the other roles it covers have no sessions here yet.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use serde_json::Value;

use super::driver::{SessionOutcome, SessionResult, SessionStarter, StartRequest};
use super::fresh_start::{FreshStart, MAX_TURNS_FOLLOW_UP, picking_up_prompt};
use super::steps::StepRequest;
use crate::backlog::Item;
use crate::config::{Engine, EngineChoiceConfig, ModelsConfig, ProjectConfig};
use crate::decisions::split_asked;
use crate::engines::claude::sandbox_enabled;
use crate::engines::resolve::resolve_claude_on_path;
use crate::engines::{EngineId, Role, SessionStart, TranscriptSink, engine_for, engine_of, run_session};
use crate::env::Env;
use crate::implementer::{
    ImplementerResult, Interrupted, MaxTurns, Session, SilenceKilled, Start, code_gates, new_session_id,
    run_implementer, session_spec,
};
use crate::implementer::{publish, start_files};
use crate::item_lifecycle::{ImplementerOutcome, ItemRun};
use crate::telemetry::now_ms;
use crate::tiers::{implementer_model, implementer_tier};
use crate::{lanes, paths};

const IMPLEMENTER_TEMPLATE: &str = include_str!("../../prompts/implementer.md");

/// Runs Claude implementer sessions for the claimed items of the projects it was built with.
pub struct ClaudeStarter {
    data_dir: PathBuf,
    projects: Vec<ProjectConfig>,
    choice: EngineChoiceConfig,
    env: Env,
    max_turns: u32,
    models: ModelsConfig,
    /// Sessions started and not yet ended (the loop's `activeSessions`).
    running: Arc<AtomicU32>,
}

impl ClaudeStarter {
    pub fn new(
        data_dir: PathBuf,
        projects: Vec<ProjectConfig>,
        choice: EngineChoiceConfig,
        env: Env,
        max_turns: u32,
        models: ModelsConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            projects,
            choice,
            env,
            max_turns,
            models,
            running: Arc::new(AtomicU32::new(0)),
        })
    }
}

impl SessionStarter for ClaudeStarter {
    fn start(&self, request: StartRequest) -> Result<(), String> {
        let project = self
            .projects
            .iter()
            .find(|p| p.id_or_derived() == request.project)
            .ok_or_else(|| format!("no project {} is configured", request.project))?
            .clone();
        let engine = engine_for(
            &self.choice,
            project.engines.as_ref(),
            Role::Implementer,
            Some(request.lane),
        );
        if engine != Engine::Claude {
            return Err("only Claude sessions run in the Rust agent yet".into());
        }
        let bin = resolve_claude_on_path(&self.env).map_err(|e| format!("{e:#}"))?;
        if !exists(&bin, &self.env) {
            return Err(format!("the claude binary wasn't found ({})", bin.display()));
        }
        let StartRequest {
            project: project_id,
            item_id,
            lane,
            item,
            outcomes,
            usage,
            session_id,
            resume,
            follow_up,
            items,
            ..
        } = request;
        let job = Job {
            data_dir: self.data_dir.clone(),
            project,
            project_id: project_id.clone(),
            bin,
            gh: self
                .env
                .get("RETASKABLE_GH")
                .map_or_else(|| PathBuf::from("gh"), PathBuf::from),
            max_turns: self.max_turns,
            lane,
            item,
            usage,
            models: self.models,
            session_id,
            resume,
            follow_up,
            items,
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

    fn active(&self) -> u32 {
        self.running.load(Ordering::SeqCst)
    }

    /// Checks, CI, review and merge aren't built in the Rust agent yet: a coded item waits in `checking`.
    fn advance(&self, _request: StepRequest) -> Option<BoxFuture<'static, ItemRun>> {
        None
    }
}

/// Whether the resolved binary is there: a path by itself, a bare name somewhere on `PATH`.
pub(super) fn exists(bin: &Path, env: &Env) -> bool {
    if bin.components().count() > 1 {
        return bin.is_file();
    }
    crate::env_path::path_dirs(env).iter().any(|dir| {
        let found = dir.join(bin);
        found.is_file() || found.with_extension("exe").is_file()
    })
}

struct Job {
    data_dir: PathBuf,
    project: ProjectConfig,
    project_id: String,
    bin: PathBuf,
    /// The `gh` program (`RETASKABLE_GH`, or `gh` on PATH).
    gh: PathBuf,
    max_turns: u32,
    lane: u32,
    item: Item,
    /// The latest `state.usage` reading, for picking the implementer's model tier.
    usage: Option<Value>,
    models: ModelsConfig,
    /// The id the session runs under (`--session-id`, or `--resume` when `resume`).
    session_id: String,
    resume: bool,
    /// `None` prepares the lane fresh, as a newly claimed item does. `Some` is the picking-up prompt for a
    /// restart after the lane's prior session ran out of turns or had grown too large: the lane is reused
    /// as-is instead.
    follow_up: Option<String>,
    /// Every item in the project's backlog; the unbuilt ones are listed in a fresh prompt, and a `waits_on`
    /// result is checked against them.
    items: Vec<Item>,
}

/// The watchdog's timeout (07-orchestrator-loop.md's `tuning.sessionQuietMinutes`): 0 turns it off.
pub(super) fn quiet_timeout(minutes: u32) -> Option<std::time::Duration> {
    (minutes > 0).then(|| std::time::Duration::from_secs(u64::from(minutes) * 60))
}

/// The follow-up an interrupted implementer is resumed with (`INTERRUPTED` in `steps.ts`).
const INTERRUPTED: &str = "You were interrupted. Continue from where you left off, then return the result.";

/// `branchName`: `retaskable/<id>-<slug of the title>-s<slice>`.
pub(super) fn branch_name(item: &Item) -> String {
    let slug: String = item
        .title
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(40)
        .collect();
    format!("retaskable/{}-{}-s{}", item.id, slug.trim_end_matches('-'), item.slice)
}

/// One implementer session in `cwd`: builds its spec from `start`, runs it with its transcript at
/// `<run folder>/<name>`, and returns its answer with the stream's usage reading and the session's last
/// context size. A `fresh` start runs under the id the driver chose and saved, so a restart of it can
/// resume it.
async fn attempt(
    job: &Job,
    cwd: &Path,
    start: Start<'_>,
    fresh: bool,
    sid: &str,
    name: String,
    where_to_start: &str,
) -> Result<Attempt> {
    let Job {
        data_dir,
        project,
        project_id,
        bin,
        max_turns,
        lane,
        item,
        usage,
        models,
        items,
        ..
    } = job;
    let session_id = &sid.to_string();
    let now = crate::telemetry::now_ms() / 1000;
    let tier = implementer_tier(usage.as_ref(), now, models);
    let model = implementer_model(tier, item).to_string();
    let branch = branch_name(item);
    let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
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
            items,
            cwd,
            model,
            budget_usd: project.budget.implementer_usd,
            max_turns: Some(*max_turns),
            engine: Some(EngineId::Claude),
            cursor: None,
            local: None,
            sandbox: sandbox_enabled(project.sandbox, cfg!(windows)),
        },
        start,
    )?;
    if fresh {
        spec.start = SessionStart::Fresh(session_id.clone());
    }
    let sink = TranscriptSink {
        path: paths::run_dir(data_dir, project_id, &item.id).join(name),
        lane: Some(*lane),
    };
    let engine = engine_of(Some(EngineId::Claude))?;
    let (engine, id, sink) = (engine.as_ref(), session_id, &sink);
    let mut context_tokens = None;
    let ctx = &mut context_tokens;
    let (answer, usage) = run_implementer(spec, |spec| async move {
        let r = run_session(engine, bin, &spec, id, Some(sink), quiet).await?;
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

/// How an [`attempt`] ended.
struct Attempt {
    answer: Result<ImplementerResult>,
    usage: Option<Value>,
    context_tokens: Option<u64>,
}

/// What `run` returns: the result, the stream's usage reading, the session's last context size and the lines
/// for the item's timeline (the code gates' notes).
/// and the id of the fresh session the job restarted in after running out of turns, if it did.
type Ran = (SessionResult, Option<Value>, Option<u64>, Vec<String>, Option<String>);

fn ran_out_of_turns(a: &Attempt) -> bool {
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
            let (base, branch, lane) = (job.project.base_branch.clone(), branch_name(&job.item), job.lane);
            tokio::task::spawn_blocking(move || lanes::prepare(&repo, &worktrees, lane, &branch, &base))
                .await
                .context("preparing the lane")?
                .context("preparing the lane")
        }
    }
}

/// Code gates before the push (06.2-implementer.md): while the session answers `pr_opened`, the gates check the
/// lane's change against `origin/<baseBranch>`. A finding resumes the same session with it, in a new transcript,
/// and gates its next answer too; every such round counts against `maxCheckRounds`, and past it a person is
/// needed. Gates that can't run (a git error) are noted and the answer stands. Any other answer is `settled`.
async fn gated(job: &Job, cwd: &Path, mut a: Attempt) -> Result<Ran> {
    let mut notes = Vec::new();
    let mut rounds = 0;
    let mut sid = job.session_id.clone();
    let mut restarted = false;
    loop {
        // Out of turns (06.2-implementer.md "Fresh-start rules on resume"): once per job, a fresh session
        // carries on in the same lane, uncommitted work kept (no `lanes::prepare`), and its answer goes through
        // the gates like any other. A second time in a row needs a person.
        if ran_out_of_turns(&a) {
            if restarted {
                let why = format!("Implementer ran out of turns ({}) twice in a row", job.max_turns);
                return Ok((
                    SessionResult::NeedsHuman(why),
                    a.usage,
                    a.context_tokens,
                    notes,
                    Some(sid),
                ));
            }
            restarted = true;
            sid = new_session_id();
            notes.push(FreshStart::OutOfTurns.history_line());
            let prompt = picking_up_prompt(job.item.pr_url.is_some(), &job.project.base_branch, MAX_TURNS_FOLLOW_UP);
            let start = Start::Continuing { follow_up: &prompt };
            a = attempt(job, cwd, start, true, &sid, format!("{sid}.jsonl"), "").await?;
            continue;
        }
        if !matches!(&a.answer, Ok(r) if r.status == "pr_opened") {
            break;
        }
        let (dir, base, gates) = (
            cwd.to_path_buf(),
            job.project.base_branch.clone(),
            job.project.code_gates,
        );
        let found = tokio::task::spawn_blocking(move || code_gates::run(&dir, &base, &gates))
            .await
            .context("running the code gates")?;
        let found = match found {
            Ok(Some(found)) => found,
            Ok(None) => break,
            Err(e) => {
                notes.push(format!("Code gates couldn't run, so the push goes ahead: {e:#}"));
                break;
            }
        };
        notes.push(found.note.clone());
        rounds += 1;
        if rounds > job.project.max_check_rounds {
            let (usage, context_tokens) = (a.usage, a.context_tokens);
            let why = format!(
                "The code gates still found problems after {} rounds: {}",
                job.project.max_check_rounds, found.note
            );
            return Ok((
                SessionResult::NeedsHuman(why),
                usage,
                context_tokens,
                notes,
                restarted.then_some(sid),
            ));
        }
        let follow_up = format!("{}\n\nFix them, commit, and return the result again.", found.message);
        let start = Start::Resume {
            session_id: sid.clone(),
            follow_up: &follow_up,
            decisions: &[],
        };
        let name = format!("{sid}-resumed-{}.jsonl", now_ms());
        a = attempt(job, cwd, start, false, &sid, name, "").await?;
    }
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
            branch_name(&job.item),
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
    ran.4 = restarted.then_some(sid);
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
        let a = attempt(&job, &cwd, start, true, &job.session_id, name, "").await?;
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
        let a = attempt(&job, &cwd, start, !resumed, &job.session_id, name, &where_to_start).await?;
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
/// ([`SilenceKilled`]) is this computer's problem, so the item goes back to the queue for a healthy
/// lane to claim again, the same as any other own failure (07-orchestrator-loop.md's watchdog). A
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
        Err(e) if e.downcast_ref::<SilenceKilled>().is_some() => (
            SessionResult::Failed(format!("went quiet and was killed: {e:#}")),
            usage,
        ),
        Err(e) => (
            SessionResult::NeedsHuman(format!("The session ended without answering: {e:#}")),
            usage,
        ),
        Ok(r) => (classify(&r), usage),
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
