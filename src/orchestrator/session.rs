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
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use serde_json::Value;

use super::driver::{SessionOutcome, SessionResult, SessionStarter, StartRequest};
use super::steps::StepRequest;
use crate::backlog::Item;
use crate::config::{Engine, EngineChoiceConfig, ModelsConfig, ProjectConfig};
use crate::decisions::split_asked;
use crate::engines::claude::sandbox_enabled;
use crate::engines::resolve::resolve_claude_on_path;
use crate::engines::{EngineId, Role, SessionStart, TranscriptSink, engine_for, engine_of, run_session};
use crate::env::Env;
use crate::implementer::start_files;
use crate::implementer::{
    ImplementerResult, Interrupted, MaxTurns, Session, SilenceKilled, Start, code_gates, run_implementer, session_spec,
};
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
            ..
        } = request;
        let job = Job {
            data_dir: self.data_dir.clone(),
            project,
            project_id: project_id.clone(),
            bin,
            max_turns: self.max_turns,
            lane,
            item,
            usage,
            models: self.models,
            session_id,
            resume,
            follow_up,
        };
        // The inner task is joined, so even a panic in it ends in exactly one outcome, and one count down.
        let running = self.running.clone();
        running.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            // A fresh start's failure here is before any session ran (an own failure, section 7.3): it goes
            // back to the queue. A resumed session already ran once, so the same failure here may have left
            // real work behind, and needs a person instead.
            let (result, usage, context_tokens, notes) = match tokio::spawn(run(job)).await {
                Ok(Ok(ran)) => ran,
                Ok(Err(e)) if resume => (
                    SessionResult::NeedsHuman(format!("the session couldn't resume: {e:#}")),
                    None,
                    None,
                    vec![],
                ),
                Ok(Err(e)) => (SessionResult::Failed(format!("{e:#}")), None, None, vec![]),
                Err(e) if resume => (
                    SessionResult::NeedsHuman(format!("the session's task crashed while resuming: {e}")),
                    None,
                    None,
                    vec![],
                ),
                Err(e) => (
                    SessionResult::Failed(format!("the session task crashed: {e}")),
                    None,
                    None,
                    vec![],
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
        session_id,
        ..
    } = job;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
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
type Ran = (SessionResult, Option<Value>, Option<u64>, Vec<String>);

fn settled(a: Attempt, max_turns: u32) -> Ran {
    let Attempt {
        answer,
        usage,
        context_tokens,
    } = a;
    if matches!(&answer, Err(e) if e.downcast_ref::<MaxTurns>().is_some()) {
        return (SessionResult::MaxTurns(max_turns), usage, context_tokens, vec![]);
    }
    let (result, usage) = session_result_for_answer(answer, usage);
    (result, usage, context_tokens, vec![])
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
    loop {
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
            return Ok((SessionResult::NeedsHuman(why), usage, context_tokens, notes));
        }
        let follow_up = format!("{}\n\nFix them, commit, and return the result again.", found.message);
        let start = Start::Resume {
            session_id: job.session_id.clone(),
            follow_up: &follow_up,
            decisions: &[],
        };
        let name = format!("{}-resumed-{}.jsonl", job.session_id, now_ms());
        a = attempt(job, cwd, start, false, name).await?;
    }
    let mut ran = settled(a, job.max_turns);
    ran.3 = notes;
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
<<<<<<< HEAD
        let a = attempt(&job, &cwd, start, true, name).await?;
        return gated(&job, &cwd, a).await;
=======
        return Ok(settled(attempt(&job, &cwd, start, true, name, "").await?, max_turns));
>>>>>>> origin/main
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
        let a = attempt(&job, &cwd, start, !resumed, name, &where_to_start).await?;
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
<<<<<<< HEAD
#[path = "session_tests.rs"]
mod tests;
=======
mod tests {
    use std::time::Duration;

    use tokio::sync::mpsc::UnboundedReceiver;

    use super::*;
    use crate::backlog::ItemStatus;
    use crate::config::RoleEngines;
    use crate::orchestrator::driver::OutcomeSender;

    const FINISHED: &str = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"pr_opened","summary":"done"},"total_cost_usd":0.1,"session_id":"s"}"#;

    struct Rig {
        _dir: tempfile::TempDir,
        root: PathBuf,
        starter: Arc<ClaudeStarter>,
        args: PathBuf,
    }

    /// A project over a real repo, whose `claude` prints `lines` (or, with `None`, exits 3 at once).
    fn rig(lines: Option<&str>, choice: EngineChoiceConfig, claude: bool) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let repo = crate::test_support::repo_with_origin(&root);
        let (out, args, bin) = (root.join("out.jsonl"), root.join("args.txt"), root.join(fake_name()));
        std::fs::write(&out, lines.unwrap_or("")).unwrap();
        if claude {
            fake_claude(&bin, &out, &args, lines.is_some());
        }
        let mut project = ProjectConfig {
            id: Some("p".into()),
            repo_path: repo.to_string_lossy().into_owned(),
            worktrees_dir: root.join("worktrees").to_string_lossy().into_owned(),
            base_branch: "main".into(),
            github_repo: "o/p".into(),
            ..Default::default()
        };
        project.budget.implementer_usd = 5.0;
        let bin = bin.to_string_lossy().into_owned();
        let env = Env::new(move |k| (k == "RETASKABLE_CLAUDE").then(|| bin.clone()));
        let starter = ClaudeStarter::new(
            root.join("data"),
            vec![project],
            choice,
            env,
            50,
            ModelsConfig::default(),
        );
        Rig {
            _dir: dir,
            root,
            starter,
            args,
        }
    }

    fn item(spec_refs: &[&str], remaining: Option<&str>) -> Item {
        Item {
            id: "implementer-session".into(),
            title: "Have a session write the code".into(),
            status: ItemStatus::Building,
            rank: 1,
            do_next: false,
            depends_on: Vec::new(),
            spec_refs: spec_refs.iter().map(ToString::to_string).collect(),
            slice: 2,
            remaining: remaining.map(str::to_string),
            pr_url: None,
            branch: None,
            kind: None,
            milestone: None,
            reopened: None,
        }
    }

    fn fake_name() -> &'static str {
        if cfg!(windows) { "claude.cmd" } else { "claude.sh" }
    }

    fn fake_claude(bin: &Path, out: &Path, args: &Path, ok: bool) {
        let code = if ok { 0 } else { 3 };
        if cfg!(windows) {
            let script = format!(
                "@echo off\r\necho %* > \"{0}\"\r\nfindstr \"^\" >> \"{0}\"\r\ntype \"{1}\"\r\nexit /b {code}\r\n",
                args.display(),
                out.display()
            );
            std::fs::write(bin, script).unwrap();
        } else {
            let script = format!(
                "#!/bin/sh\necho \"$@\" > \"{0}\"\ncat >> \"{0}\"\ncat \"{1}\"\nexit {code}\n",
                args.display(),
                out.display()
            );
            crate::test_support::write_executable(bin, &script);
        }
    }

    fn request() -> (StartRequest, UnboundedReceiver<SessionOutcome>) {
        let (outcomes, rx) = OutcomeSender::channel();
        let request = StartRequest {
            project: "p".into(),
            item_id: "implementer-session".into(),
            lane: 1,
            token: "t".into(),
            item: item(&[], Some("Wire it.")),
            session_id: "sess-1".into(),
            resume: false,
            outcomes,
            usage: None,
            follow_up: None,
        };
        (request, rx)
    }

    /// Starts one session and waits for its outcome, checking that no second one follows.
    async fn outcome_of(rig: &Rig) -> SessionResult {
        let (request, mut rx) = request();
        rig.starter.start(request).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(120), rx.recv())
            .await
            .expect("an outcome")
            .expect("the channel stays open");
        assert_eq!((got.project.as_str(), got.lane), ("p", 1));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(rx.try_recv().is_err(), "exactly one outcome per start");
        got.result
    }

    fn answer(status: &str, questions: &str) -> String {
        format!(
            r#"{{"type":"result","subtype":"success","is_error":false,"structured_output":{{"status":"{status}","summary":"s","questions":{questions}}},"session_id":"s"}}"#
        )
    }

    #[tokio::test]
    async fn a_finished_session_sends_finished_and_keeps_its_transcript_locally() {
        let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
        assert_eq!(
            outcome_of(&rig).await,
            SessionResult::Implementer(ImplementerOutcome::Finished)
        );
        let args = std::fs::read_to_string(&rig.args).unwrap();
        assert!(args.contains("--permission-mode auto"), "{args}");
        assert!(!args.contains("--dangerously-skip-permissions"), "{args}");
        let runs = paths::run_dir(&rig.root.join("data"), "p", "implementer-session");
        let files: Vec<_> = std::fs::read_dir(runs).unwrap().collect();
        assert_eq!(files.len(), 1);
    }

    #[tokio::test]
    async fn a_fresh_session_runs_under_the_given_id_and_a_resumed_one_continues_it() {
        let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
        outcome_of(&rig).await;
        let args = std::fs::read_to_string(&rig.args).unwrap();
        assert!(
            args.contains("--session-id sess-1") && !args.contains("--resume"),
            "{args}"
        );
        let (mut request, mut rx) = request();
        request.resume = true;
        rig.starter.start(request).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(120), rx.recv()).await;
        assert!(matches!(got, Ok(Some(_))));
        let args = std::fs::read_to_string(&rig.args).unwrap();
        assert!(
            args.contains("--resume sess-1") && !args.contains("--session-id"),
            "{args}"
        );
        let runs = paths::run_dir(&rig.root.join("data"), "p", "implementer-session");
        assert_eq!(
            std::fs::read_dir(runs).unwrap().count(),
            2,
            "the first transcript is kept"
        );
    }

    /// Commits a spec that names `src/lib.rs`, and that file, to the rig's repo and origin.
    fn commit_spec(rig: &Rig) {
        let repo = rig.root.join("repo");
        std::fs::create_dir_all(repo.join("docs/specs")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("docs/specs/a.md"), "Touches `src/lib.rs`.\n").unwrap();
        std::fs::write(repo.join("src/lib.rs"), "").unwrap();
        for args in [
            vec!["add", "."],
            vec!["commit", "--quiet", "-m", "spec"],
            vec!["push", "--quiet", "origin", "main"],
        ] {
            let ok = crate::test_support::git(&repo).args(args).status().unwrap().success();
            assert!(ok);
        }
    }

    async fn start_with(rig: &Rig, spec_refs: &[&str], resume: bool) {
        let (mut request, mut rx) = request();
        request.item.spec_refs = spec_refs.iter().map(ToString::to_string).collect();
        request.resume = resume;
        rig.starter.start(request).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(120), rx.recv()).await;
        assert!(matches!(got, Ok(Some(_))));
    }

    #[tokio::test]
    async fn a_fresh_session_is_pointed_at_the_items_specs_and_the_code_they_name() {
        let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
        commit_spec(&rig);
        start_with(&rig, &["docs/specs/a.md"], false).await;
        let args = std::fs::read_to_string(&rig.args).unwrap();
        assert!(args.contains("## Where to start"), "{args}");
        assert!(
            args.contains("docs/specs/a.md") && args.contains("src/lib.rs"),
            "{args}"
        );
    }

    #[tokio::test]
    async fn a_resumed_session_gets_only_the_follow_up() {
        let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
        commit_spec(&rig);
        start_with(&rig, &["docs/specs/a.md"], false).await;
        start_with(&rig, &["docs/specs/a.md"], true).await;
        let args = std::fs::read_to_string(&rig.args).unwrap();
        assert!(args.contains("--resume sess-1"), "{args}");
        assert!(
            !args.contains("## Where to start") && !args.contains("docs/specs/a.md"),
            "{args}"
        );
    }

    #[tokio::test]
    async fn a_session_still_starts_when_git_gives_nothing_to_list() {
        let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
        // Break the lane's view of origin after it's prepared is not possible, so name specs git can't show.
        start_with(&rig, &["docs/specs/missing.md"], false).await;
        let args = std::fs::read_to_string(&rig.args).unwrap();
        assert!(args.contains("--session-id sess-1"), "{args}");
        assert!(!args.contains("## Where to start"), "{args}");
    }

    #[tokio::test]
    async fn a_session_that_exits_without_an_answer_needs_a_human() {
        let rig = rig(None, EngineChoiceConfig::default(), true);
        assert!(matches!(outcome_of(&rig).await, SessionResult::NeedsHuman(_)));
    }

    fn classified(json: &str) -> SessionResult {
        classify(&serde_json::from_str(json).unwrap())
    }

    #[test]
    fn each_status_maps_to_what_the_ts_agent_does() {
        let why = |r: SessionResult| match r {
            SessionResult::NeedsHuman(why) => why,
            other => panic!("{other:?}"),
        };
        assert!(why(classified(r#"{"status":"failed","summary":"stuck"}"#)).contains("Implementer gave up: stuck"));
        assert!(why(classified(r#"{"status":"nothing_left","summary":"built"}"#)).contains("Nothing left to build"));
        let steps = why(classified(
            r#"{"status":"person_steps","personSteps":["Press go"],"summary":"s"}"#,
        ));
        assert!(steps.contains("1. Press go"), "{steps}");
        assert!(why(classified(r#"{"status":"person_steps","summary":"s"}"#)).contains("gave up"));
        let waits = why(classified(r#"{"status":"waits_on","waitsOn":["a","b"],"summary":"s"}"#));
        assert!(waits.contains("a, b"), "{waits}");
        assert!(why(classified(r#"{"status":"odd","summary":"s"}"#)).contains("answered with odd"));
    }

    #[test]
    fn a_watchdog_kill_is_an_own_failure_not_a_needs_human() {
        let killed = Err(anyhow::Error::new(SilenceKilled(
            "ended with killed_for_silence: no output".into(),
        )));
        assert!(
            matches!(session_result_for_answer(killed, None).0, SessionResult::Failed(_)),
            "a session the watchdog killed should requeue, not need a person"
        );
        let crashed = Err(anyhow::anyhow!("ended with error_during_execution"));
        assert!(matches!(
            session_result_for_answer(crashed, None).0,
            SessionResult::NeedsHuman(_)
        ));
    }

    #[test]
    fn running_out_of_turns_maps_to_max_turns_carrying_the_cap_and_the_context_size() {
        let out = Attempt {
            answer: Err(anyhow::Error::new(MaxTurns("ended with error_max_turns".into()))),
            usage: None,
            context_tokens: Some(7),
        };
        assert_eq!(settled(out, 300), (SessionResult::MaxTurns(300), None, Some(7)));
    }

    #[test]
    fn a_cut_sessions_error_maps_to_interrupted_not_needs_human_or_failed() {
        let cut = Err(anyhow::Error::new(Interrupted("ended with interrupted".into())));
        assert_eq!(session_result_for_answer(cut, None).0, SessionResult::Interrupted);

        let killed = Err(anyhow::Error::new(SilenceKilled(
            "ended with killed_for_silence: no output".into(),
        )));
        assert!(matches!(
            session_result_for_answer(killed, None).0,
            SessionResult::Failed(_)
        ));

        let crashed = Err(anyhow::anyhow!("ended with error_during_execution"));
        assert!(matches!(
            session_result_for_answer(crashed, None).0,
            SessionResult::NeedsHuman(_)
        ));
    }

    #[test]
    fn the_usage_reading_travels_with_the_result_whichever_way_the_session_ended() {
        let usage = serde_json::json!({"status": "rejected", "resetsAt": i64::MAX});
        let killed = Err(anyhow::Error::new(SilenceKilled(
            "ended with killed_for_silence".into(),
        )));
        assert_eq!(
            session_result_for_answer(killed, Some(usage.clone())).1,
            Some(usage.clone())
        );
        let crashed = Err(anyhow::anyhow!("ended with error_during_execution"));
        assert_eq!(session_result_for_answer(crashed, Some(usage.clone())).1, Some(usage));
    }

    #[test]
    fn the_sandbox_follows_the_platform() {
        use crate::config::SandboxMode;
        assert!(sandbox_enabled(SandboxMode::Auto, false));
        assert!(!sandbox_enabled(SandboxMode::Auto, true));
        assert!(!sandbox_enabled(SandboxMode::Off, false));
    }

    #[tokio::test]
    async fn a_sandboxed_platform_passes_the_sandbox_settings() {
        let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
        outcome_of(&rig).await;
        let args = std::fs::read_to_string(&rig.args).unwrap();
        assert_eq!(args.contains("--settings"), !cfg!(windows), "{args}");
    }

    #[tokio::test]
    async fn a_session_with_questions_sends_questions() {
        let rig = rig(
            Some(&answer("needs_clarification", r#"[{"question":"Which one?"}]"#)),
            EngineChoiceConfig::default(),
            true,
        );
        assert_eq!(
            outcome_of(&rig).await,
            SessionResult::Implementer(ImplementerOutcome::Questions {
                all_already_answered: false
            })
        );
    }

    #[tokio::test]
    async fn no_usage_reading_yet_still_starts_a_first_try_on_sonnet() {
        let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
        let (mut request, mut rx) = request();
        request.usage = None;
        rig.starter.start(request).unwrap();
        rx.recv().await.unwrap();
        let args = std::fs::read_to_string(&rig.args).unwrap();
        let model = args.split_whitespace().skip_while(|a| *a != "--model").nth(1);
        assert_eq!(model, Some("sonnet"), "{args}");
    }

    #[tokio::test]
    async fn a_reopened_item_with_no_usage_reading_starts_on_opus() {
        let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
        let (mut request, mut rx) = request();
        request.usage = None;
        request.item.reopened = Some(crate::backlog::Reopened {
            notes: Some("The list is empty".into()),
            at: Some("2026-10-29T00:00:00Z".into()),
            by: None,
        });
        rig.starter.start(request).unwrap();
        rx.recv().await.unwrap();
        let args = std::fs::read_to_string(&rig.args).unwrap();
        let model = args.split_whitespace().skip_while(|a| *a != "--model").nth(1);
        assert_eq!(model, Some("opus"), "{args}");
    }

    #[tokio::test]
    async fn a_usage_reading_below_opus_above_pct_starts_the_implementer_on_sonnet() {
        let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
        let (mut request, mut rx) = request();
        request.usage = Some(serde_json::json!({"status": "allowed_warning", "resetsAt": i64::MAX}));
        rig.starter.start(request).unwrap();
        rx.recv().await.unwrap();
        let args = std::fs::read_to_string(&rig.args).unwrap();
        let model = args.split_whitespace().skip_while(|a| *a != "--model").nth(1);
        assert_eq!(model, Some("sonnet"), "{args}");
    }

    /// A starter like `rig`'s, but whose only project's repo path is missing, so preparing its lane fails.
    fn starter_with_unpreparable_lane(rig: &Rig) -> Arc<ClaudeStarter> {
        let mut projects = rig.starter.projects.clone();
        projects[0].repo_path = rig.root.join("missing").to_string_lossy().into_owned();
        ClaudeStarter::new(
            rig.root.join("data"),
            projects,
            EngineChoiceConfig::default(),
            rig.starter.env.clone(),
            50,
            ModelsConfig::default(),
        )
    }

    #[tokio::test]
    async fn a_lane_that_cannot_be_prepared_fails() {
        let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
        let starter = starter_with_unpreparable_lane(&rig);
        let (request, mut rx) = request();
        starter.start(request).unwrap();
        let got = rx.recv().await.unwrap();
        assert!(matches!(got.result, SessionResult::Failed(_)), "{got:?}");
    }

    #[tokio::test]
    async fn a_resumed_session_that_cannot_prepare_its_lane_needs_a_human_not_the_queue() {
        let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
        let starter = starter_with_unpreparable_lane(&rig);
        let (mut request, mut rx) = request();
        request.resume = true;
        starter.start(request).unwrap();
        let got = rx.recv().await.unwrap();
        assert!(matches!(got.result, SessionResult::NeedsHuman(_)), "{got:?}");
    }

    #[tokio::test]
    async fn another_engine_refuses_to_start() {
        let choice = EngineChoiceConfig {
            roles: RoleEngines {
                implementer: Some(Engine::Cursor),
                ..Default::default()
            },
            ..Default::default()
        };
        let rig = rig(Some(FINISHED), choice, true);
        let (request, mut rx) = request();
        assert_eq!(
            rig.starter.start(request),
            Err("only Claude sessions run in the Rust agent yet".to_string())
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_missing_binary_refuses_to_start() {
        let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), false);
        let (request, mut rx) = request();
        let err = rig.starter.start(request).unwrap_err();
        assert!(err.contains("wasn't found"), "{err}");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn the_branch_is_the_id_a_short_slug_and_the_slice() {
        assert_eq!(
            branch_name(&item(&[], None)),
            "retaskable/implementer-session-have-a-session-write-the-code-s2"
        );
    }

    #[test]
    fn quiet_timeout_turns_off_at_zero_and_is_minutes_otherwise() {
        assert_eq!(quiet_timeout(0), None);
        assert_eq!(quiet_timeout(20), Some(Duration::from_secs(20 * 60)));
    }
}
>>>>>>> origin/main
