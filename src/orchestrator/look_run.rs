//! [`LookRuns`]: the store's `look` runs (docs/specs/pieces.md sections 3.2 and 3.5). One read-only session in the
//! project's base checkout decides what's next for a piece or plan that keeps failing; the decision goes back as the
//! run's `looked` result. One look at a time on this computer.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::driver::{RunEnd, RunRequest, RunStarter, piece_id};
use super::one_at_a_time::{Held, OneAtATime};
use super::session::{exists, quiet_timeout};
use crate::backlog::runs::{Run, RunKind, RunOutcome};
use crate::config::{Engine, EngineChoiceConfig, ModelsConfig, ProjectConfig};
use crate::engines::claude::sandbox_enabled;
use crate::engines::resolve::resolve_claude_on_path;
use crate::engines::{
    EngineId, EphemeralSessionConfig, Role, TranscriptSink, engine_for, engine_of, run_session_tracked,
};
use crate::env::Env;
use crate::looks::{LookBrief, Looker, decide};
use crate::process_tree::ProcessTree;
use crate::runs::winding_down;
use crate::tiers::{implementer_tier, mid_tier};
use crate::{lanes, paths};

const TEMPLATE: &str = include_str!("../../prompts/look.md");

/// Runs `look` runs of the projects it was built with, one at a time on this computer.
pub struct LookRuns {
    pub data_dir: PathBuf,
    pub projects: Vec<ProjectConfig>,
    pub choice: EngineChoiceConfig,
    pub env: Env,
    pub models: ModelsConfig,
    /// Held by a look while it runs. The future `plan` runner shares it: one plan or look at a time.
    pub slot: OneAtATime,
}

impl LookRuns {
    pub fn new(
        data_dir: PathBuf,
        projects: Vec<ProjectConfig>,
        choice: EngineChoiceConfig,
        env: Env,
        models: ModelsConfig,
    ) -> Self {
        Self {
            data_dir,
            projects,
            choice,
            env,
            models,
            slot: OneAtATime::default(),
        }
    }

    fn triager_on_claude(&self, project: Option<&ProjectConfig>) -> bool {
        let engines = project.and_then(|p| p.engines.as_ref());
        engine_for(&self.choice, engines, Role::Triager, None) == Engine::Claude
    }
}

impl RunStarter for LookRuns {
    fn kinds(&self) -> Vec<RunKind> {
        // Only Claude sessions run in the Rust agent yet.
        let claude = self.triager_on_claude(None) && self.projects.iter().all(|p| self.triager_on_claude(Some(p)));
        if claude && self.slot.is_free() {
            vec![RunKind::Look]
        } else {
            Vec::new()
        }
    }

    fn start(&self, request: RunRequest) -> Result<(), String> {
        let project = self
            .projects
            .iter()
            .find(|p| p.id_or_derived() == request.project)
            .ok_or_else(|| format!("no project {} is configured", request.project))?
            .clone();
        if !self.triager_on_claude(Some(&project)) {
            return Err("only Claude sessions run in the Rust agent yet".into());
        }
        let bin = resolve_claude_on_path(&self.env).map_err(|e| format!("{e:#}"))?;
        if !exists(&bin, &self.env) {
            return Err(format!("the claude binary wasn't found ({})", bin.display()));
        }
        let held = self
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
        let (ends, token) = (request.ends.clone(), request.token.clone());
        let cut = Cut::default();
        // The inner task is joined, so even a panic in it ends in exactly one end.
        tokio::spawn(async move {
            let _held: Held = held;
            let end = match tokio::spawn(look(job, request.clone(), cut.clone())).await {
                Ok(end) => end,
                Err(e) => Some((RunOutcome::Failed(format!("the run's task crashed: {e}")), json!({}))),
            };
            // A run the loop already ended (a stop or the ceiling cut its tree) sends nothing.
            if let Some((outcome, result)) = end.filter(|_| !request.stop.stopped() && !cut.is_cut()) {
                ends.send(RunEnd { token, outcome, result });
            }
        });
        Ok(())
    }
}

struct Job {
    data_dir: PathBuf,
    project: ProjectConfig,
    bin: PathBuf,
    model: String,
}

/// The session's process tree, kept so a cut that lands after the loop has let the run go is still seen.
#[derive(Clone, Default)]
struct Cut(Arc<Mutex<Option<Arc<ProcessTree>>>>);

impl Cut {
    fn is_cut(&self) -> bool {
        self.0.lock().unwrap().as_ref().is_some_and(|t| t.was_cut())
    }
}

/// How the run ended; `None` when the loop ended it.
async fn look(job: Job, request: RunRequest, cut: Cut) -> Option<(RunOutcome, Value)> {
    let project = &job.project;
    let worktrees = PathBuf::from(&project.worktrees_dir);
    let base = lanes::base_checkout_path(&worktrees);
    let cwd = if base.exists() {
        base
    } else {
        PathBuf::from(&project.repo_path)
    };
    let brief = LookBrief::from_run(&Run {
        token: request.token.clone(),
        kind: request.kind,
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
    let (processes, token, seen) = (request.processes.clone(), request.token.clone(), cut.clone());
    tokio::spawn(async move {
        if let Ok(tree) = rx.await {
            *seen.0.lock().unwrap() = Some(tree.clone());
            processes.add(&token, tree);
        }
    });
    let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
    let decided = decide(&looker, &brief, |spec| async move {
        let engine = engine_of(Some(EngineId::Claude))?;
        run_session_tracked(engine.as_ref(), &job.bin, &spec, "look", Some(&sink), quiet, Some(tx)).await
    })
    .await;
    if request.stop.stopped() || cut.is_cut() {
        return None;
    }
    if winding_down() {
        return Some((RunOutcome::Interrupted, json!({})));
    }
    Some(match decided.result {
        Some(r) => (RunOutcome::Looked, r.to_wire()),
        None => (
            RunOutcome::Failed(format!(
                "The look session {}",
                decided.note.unwrap_or_else(|| "returned nothing usable.".into())
            )),
            json!({}),
        ),
    })
}

#[cfg(all(test, unix))]
#[path = "look_run_tests.rs"]
mod tests;
