//! The real implementer session starter for `claim` mode (`startItem`/`implement` in Retaskable.com's
//! `agent/src/steps.ts`): prepares the lane, builds the prompt, runs `claude -p` and turns what it
//! answered into the [`SessionOutcome`] the loop settles. Claude only; any other engine refuses.
//! Left out: "Where to start" in the prompt, the usage-window model tier (always `sonnet`), Claude's sandbox,
//! and the decisions a person already gave (the claimed item doesn't carry them).
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;

use super::driver::{SessionOutcome, SessionResult, SessionStarter, StartRequest};
use super::steps::StepRequest;
use crate::backlog::Item;
use crate::config::{Engine, EngineChoiceConfig, ProjectConfig};
use crate::decisions::split_asked;
use crate::engines::claude::sandbox_enabled;
use crate::engines::resolve::resolve_claude_on_path;
use crate::engines::{EngineId, Role, SessionStart, TranscriptSink, engine_for, engine_of, run_session};
use crate::env::Env;
use crate::implementer::{ImplementerResult, Session, SilenceKilled, Start, run_implementer, session_spec};
use crate::item_lifecycle::{ImplementerOutcome, ItemRun};
use crate::telemetry::now_ms;
use crate::{lanes, paths};

const IMPLEMENTER_TEMPLATE: &str = include_str!("../../prompts/implementer.md");

/// Runs Claude implementer sessions for the claimed items of the projects it was built with.
pub struct ClaudeStarter {
    data_dir: PathBuf,
    projects: Vec<ProjectConfig>,
    choice: EngineChoiceConfig,
    env: Env,
    max_turns: u32,
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
    ) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            projects,
            choice,
            env,
            max_turns,
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
            session_id,
            resume,
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
            session_id,
            resume,
        };
        // The inner task is joined, so even a panic in it ends in exactly one outcome, and one count down.
        let running = self.running.clone();
        running.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            // A fresh start's failure here is before any session ran (an own failure, section 7.3): it goes
            // back to the queue. A resumed session already ran once, so the same failure here may have left
            // real work behind, and needs a person instead.
            let result = match tokio::spawn(run(job)).await {
                Ok(Ok(result)) => result,
                Ok(Err(e)) if resume => SessionResult::NeedsHuman(format!("the session couldn't resume: {e:#}")),
                Ok(Err(e)) => SessionResult::Failed(format!("{e:#}")),
                Err(e) if resume => {
                    SessionResult::NeedsHuman(format!("the session's task crashed while resuming: {e}"))
                }
                Err(e) => SessionResult::Failed(format!("the session task crashed: {e}")),
            };
            running.fetch_sub(1, Ordering::SeqCst);
            outcomes.send(SessionOutcome {
                project: project_id,
                item_id,
                lane,
                result,
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
    /// The id the session runs under (`--session-id`, or `--resume` when `resume`).
    session_id: String,
    resume: bool,
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

async fn run(job: Job) -> Result<SessionResult> {
    let Job {
        data_dir,
        project,
        project_id,
        bin,
        max_turns,
        lane,
        item,
        session_id,
        resume,
    } = job;
    let branch = branch_name(&item);
    let (repo, worktrees, base) = (
        PathBuf::from(&project.repo_path),
        PathBuf::from(&project.worktrees_dir),
        project.base_branch.clone(),
    );
    let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
    // A restart resumes the session where it left off: the lane is kept as it is (preparing it would
    // reset away the work), and anything the old session left running in it is stopped. Not ported: the
    // TS rules that start fresh instead when the session has grown past `maxResumeContextTokens` or ran
    // out of turns (neither is tracked here yet); a transcript Claude Code no longer has is handled below.
    let mut resuming = resume
        .then(|| lanes::lane_path(&worktrees, lane))
        .filter(|d| d.is_dir());
    loop {
        let cwd = match resuming.clone() {
            Some(dir) => {
                let stop = dir.clone();
                tokio::task::spawn_blocking(move || lanes::kill_processes_in(&stop))
                    .await
                    .context("stopping what the old session left running")?
                    .context("stopping what the old session left running")?;
                dir
            }
            None => {
                let (repo, worktrees, base, branch) = (repo.clone(), worktrees.clone(), base.clone(), branch.clone());
                tokio::task::spawn_blocking(move || lanes::prepare(&repo, &worktrees, lane, &branch, &base))
                    .await
                    .context("preparing the lane")?
                    .context("preparing the lane")?
            }
        };
        let resumed = resuming.is_some();
        let start = if resumed {
            Start::Resume {
                session_id: session_id.clone(),
                follow_up: INTERRUPTED,
                decisions: &[],
            }
        } else {
            Start::Fresh
        };
        let mut spec = session_spec(
            &Session {
                template: IMPLEMENTER_TEMPLATE,
                project_type: project.project_type,
                item: &item,
                branch: &branch,
                base_branch: &project.base_branch,
                github_repo: &project.github_repo,
                checks: &project.checks,
                code_gates: project.code_gates,
                where_to_start: "",
                cwd: &cwd,
                model: "sonnet".to_string(),
                budget_usd: project.budget.implementer_usd,
                max_turns: Some(max_turns),
                engine: Some(EngineId::Claude),
                cursor: None,
                local: None,
                sandbox: sandbox_enabled(project.sandbox, cfg!(windows)),
            },
            start,
        )?;
        if !resumed {
            // The driver chose the id and saved it, so a restart can resume this session.
            spec.start = SessionStart::Fresh(session_id.clone());
        }
        // A resumed session's transcript is a new file: the run folder keeps the earlier one whole.
        let name = if resumed {
            format!("{session_id}-resumed-{}.jsonl", now_ms())
        } else {
            format!("{session_id}.jsonl")
        };
        let sink = TranscriptSink {
            path: paths::run_dir(&data_dir, &project_id, &item.id).join(name),
            lane: Some(lane),
        };
        let engine = engine_of(Some(EngineId::Claude))?;
        let (engine, bin, id, sink) = (engine.as_ref(), &bin, &session_id, &sink);
        let answer = run_implementer(spec, |spec| async move {
            run_session(engine, bin, &spec, id, Some(sink), quiet).await
        })
        .await;
        // The old session was stopped before Claude Code saved it (a restart moments after it began): the
        // same id starts a fresh one, as `implement` in `steps.ts` does on "No conversation found".
        if resumed && matches!(&answer, Err(e) if format!("{e:#}").contains("No conversation found")) {
            resuming = None;
            continue;
        }
        return Ok(session_result_for_answer(answer));
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
/// lane to claim again, the same as any other own failure (07-orchestrator-loop.md's watchdog). Any
/// other error means a session ran and didn't come back clean, so it needs a person instead (it may
/// have left real work behind, section 7.3).
fn session_result_for_answer(answer: Result<ImplementerResult>) -> SessionResult {
    match answer {
        Err(e) if e.downcast_ref::<SilenceKilled>().is_some() => {
            SessionResult::Failed(format!("went quiet and was killed: {e:#}"))
        }
        Err(e) => SessionResult::NeedsHuman(format!("The session ended without answering: {e:#}")),
        Ok(r) => classify(&r),
    }
}

#[cfg(test)]
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
        let starter = ClaudeStarter::new(root.join("data"), vec![project], choice, env, 50);
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
            spec_refs: spec_refs.iter().map(|s| s.to_string()).collect(),
            slice: 2,
            remaining: remaining.map(str::to_string),
            pr_url: None,
            branch: None,
            kind: None,
            milestone: None,
        }
    }

    fn fake_name() -> &'static str {
        if cfg!(windows) { "claude.cmd" } else { "claude.sh" }
    }

    fn fake_claude(bin: &Path, out: &Path, args: &Path, ok: bool) {
        let code = if ok { 0 } else { 3 };
        if cfg!(windows) {
            let script = format!(
                "@echo off\r\necho %* > \"{}\"\r\ntype \"{}\"\r\nexit /b {code}\r\n",
                args.display(),
                out.display()
            );
            std::fs::write(bin, script).unwrap();
        } else {
            let script = format!(
                "#!/bin/sh\necho \"$@\" > \"{}\"\ncat \"{}\"\nexit {code}\n",
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
            matches!(session_result_for_answer(killed), SessionResult::Failed(_)),
            "a session the watchdog killed should requeue, not need a person"
        );
        let crashed = Err(anyhow::anyhow!("ended with error_during_execution"));
        assert!(matches!(
            session_result_for_answer(crashed),
            SessionResult::NeedsHuman(_)
        ));
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
