//! The `look` run (docs/specs/pieces.md sections 3.2 and 3.5): one read-only triager-role session on the middle
//! tier, in the project's base checkout as it is (the repo when that doesn't exist yet), that decides what's next
//! for a piece or plan that keeps failing. It takes no lane, and only one look (or plan) runs at a time on this
//! computer, across all projects ([`Slot`]). Its process tree is registered with the run, so a beat's `stop` or
//! the ceiling cuts it; the run ends exactly once through `request.ends`, unless the loop already ended it.
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use super::driver::{RunRequest, RunStarter, piece_id};
use super::plan_run::{Cut, claude_project, spawn_run};
use super::session::quiet_timeout;
use crate::backlog::runs::{Run, RunKind, RunOutcome};
use crate::config::{Engine, EngineChoiceConfig, ModelsConfig, ProjectConfig};
use crate::engines::claude::sandbox_enabled;
use crate::engines::{
    EngineId, EphemeralSessionConfig, Role, TranscriptSink, engine_for, engine_of, run_session_tracked,
};
use crate::env::Env;
use crate::looks::{LookBrief, Looker, decide};
use crate::process_tree::ProcessTree;
use crate::tiers::{implementer_tier, mid_tier};
use crate::{lanes, paths};

const TEMPLATE: &str = include_str!("../../prompts/look.md");

/// The computer's one plan-or-look at a time: whoever holds the slot is the one running.
#[derive(Clone, Default)]
pub struct Slot(Arc<AtomicBool>);

/// Holds the [`Slot`] until it's dropped, however the run's task ends (a panic included).
pub struct SlotGuard(Arc<AtomicBool>);

impl Slot {
    pub fn busy(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// The slot, if nobody else has it.
    pub fn take(&self) -> Option<SlotGuard> {
        (!self.0.swap(true, Ordering::SeqCst)).then(|| SlotGuard(self.0.clone()))
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Runs `look` runs of the projects it was built with, one at a time on this computer.
pub struct LookRuns {
    pub data_dir: PathBuf,
    pub projects: Vec<ProjectConfig>,
    pub choice: EngineChoiceConfig,
    pub env: Env,
    pub models: ModelsConfig,
    slot: Slot,
}

impl LookRuns {
    pub fn new(
        data_dir: PathBuf,
        projects: Vec<ProjectConfig>,
        choice: EngineChoiceConfig,
        env: Env,
        models: ModelsConfig,
        slot: Slot,
    ) -> Self {
        Self {
            data_dir,
            projects,
            choice,
            env,
            models,
            slot,
        }
    }

    fn triager_on_claude(&self, project: Option<&ProjectConfig>) -> bool {
        let engines = project.and_then(|p| p.engines.as_ref());
        engine_for(&self.choice, engines, Role::Triager, None) == Engine::Claude
    }
}

impl RunStarter for LookRuns {
    fn kinds(&self) -> Vec<RunKind> {
        // Only Claude sessions run in the Rust agent yet; the role's engine is the machine's choice unless a project overrides it.
        let claude = self.triager_on_claude(None) && self.projects.iter().all(|p| self.triager_on_claude(Some(p)));
        if claude && !self.slot.busy() {
            vec![RunKind::Look]
        } else {
            Vec::new()
        }
    }

    fn start(&self, request: RunRequest) -> Result<(), String> {
        let (project, bin) = claude_project(&self.projects, &request, &self.env, |p| self.triager_on_claude(Some(p)))?;
        let guard = self
            .slot
            .take()
            .ok_or("a plan or look run is already running on this computer")?;
        let model = mid_tier(implementer_tier(None, crate::telemetry::now_ms() / 1000, &self.models)).to_string();
        let job = Job {
            data_dir: self.data_dir.clone(),
            project,
            bin,
            model,
        };
        let cut = Cut::default();
        let work = look(job, request.clone(), cut.clone());
        spawn_run(guard, request, cut, async move { Some(work.await) });
        Ok(())
    }
}

struct Job {
    data_dir: PathBuf,
    project: ProjectConfig,
    bin: PathBuf,
    model: String,
}

/// How the run ended: the decision, or why there isn't one.
async fn look(job: Job, request: RunRequest, cut: Cut) -> (RunOutcome, Value) {
    let project = &job.project;
    let base = lanes::base_checkout_path(&PathBuf::from(&project.worktrees_dir));
    let cwd = if base.exists() {
        base
    } else {
        PathBuf::from(&project.repo_path)
    };
    let brief = LookBrief::from_run(&Run {
        token: request.token.clone(),
        kind: RunKind::Look,
        piece: request.piece.clone(),
        other: request.other.clone(),
    });
    let looker = Looker {
        template: TEMPLATE,
        project_type: project.project_type,
        github_repo: &project.github_repo,
        session: EphemeralSessionConfig {
            cwd: &cwd,
            budget_usd: project.budget.reviewer_usd,
            model: job.model.clone(),
            local: None,
            engine: Some(EngineId::Claude),
            cursor: None,
            sandbox: sandbox_enabled(project.sandbox, cfg!(windows)),
            shell: true,
        },
    };
    let sink = TranscriptSink {
        path: paths::run_dir(&job.data_dir, &project.id_or_derived(), piece_id(&request.piece))
            .join(format!("look-{}.jsonl", crate::telemetry::now_ms())),
        lane: None,
    };
    let (tx, rx) = tokio::sync::oneshot::channel::<Arc<ProcessTree>>();
    let (processes, token) = (request.processes.clone(), request.token.clone());
    tokio::spawn(async move {
        if let Ok(tree) = rx.await {
            *cut.0.lock().unwrap() = Some(tree.clone());
            processes.add(&token, tree);
        }
    });
    let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
    let decided = decide(&looker, &brief, |spec| async move {
        let engine = engine_of(Some(EngineId::Claude))?;
        run_session_tracked(engine.as_ref(), &job.bin, &spec, "look", Some(&sink), quiet, Some(tx)).await
    })
    .await;
    match decided.result {
        Some(r) => (RunOutcome::Looked, r.to_wire()),
        None => (
            RunOutcome::Failed(decided.note.unwrap_or_else(|| "the look gave no decision".into())),
            json!({}),
        ),
    }
}

#[cfg(all(test, unix))]
#[path = "look_run_tests.rs"]
mod tests;
