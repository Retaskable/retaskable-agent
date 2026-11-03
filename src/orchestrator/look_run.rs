//! [`LookRuns`]: the store's `look` runs (docs/specs/pieces.md sections 3.2 and 3.5). One read-only session in the
//! project's base checkout decides what's next for a piece or plan that keeps failing; the decision goes back as the
//! run's `looked` result. One look at a time on this computer.
use std::path::PathBuf;

use serde_json::{Value, json};

use super::driver::{RunRequest, RunStarter, piece_id};
use super::one_at_a_time::{Cut, OneAtATime, Setup, base_or_repo, claude_project, spawn_run};
use super::session::quiet_timeout;
use crate::backlog::runs::{Run, RunKind, RunOutcome};
use crate::config::{Engine, ProjectConfig};
use crate::engines::claude::sandbox_enabled;
use crate::engines::{
    EngineId, EphemeralSessionConfig, Role, TranscriptSink, engine_for, engine_of, run_session_tracked,
};
use crate::looks::{LookBrief, Looker, decide};
use crate::paths;
use crate::runs::winding_down;
use crate::tiers::{implementer_tier, mid_tier};

const TEMPLATE: &str = include_str!("../../prompts/look.md");

/// Runs `look` runs of the projects it was built with, one at a time on this computer.
pub struct LookRuns {
    pub setup: Setup,
    /// Shared with the plan runner: one plan or look at a time.
    pub slot: OneAtATime,
}

impl LookRuns {
    pub fn new(setup: Setup) -> Self {
        Self {
            setup,
            slot: OneAtATime::default(),
        }
    }

    fn triager_on_claude(&self, project: Option<&ProjectConfig>) -> bool {
        let engines = project.and_then(|p| p.engines.as_ref());
        engine_for(&self.setup.choice, engines, Role::Triager, None) == Engine::Claude
    }
}

impl RunStarter for LookRuns {
    fn kinds(&self) -> Vec<RunKind> {
        // Only Claude sessions run in the Rust agent yet.
        let claude =
            self.triager_on_claude(None) && self.setup.projects.iter().all(|p| self.triager_on_claude(Some(p)));
        if claude && self.slot.is_free() {
            vec![RunKind::Look]
        } else {
            Vec::new()
        }
    }

    fn start(&self, request: RunRequest) -> Result<(), String> {
        let (project, bin) = claude_project(&self.setup.projects, &request, &self.setup.env, |p| {
            self.triager_on_claude(Some(p))
        })?;
        let held = self
            .slot
            .take()
            .ok_or("a plan or look run is already running on this computer")?;
        let model = mid_tier(implementer_tier(
            None,
            crate::telemetry::now_ms() / 1000,
            &self.setup.models,
        ))
        .to_string();
        let job = Job {
            data_dir: self.setup.data_dir.clone(),
            project,
            bin,
            model,
        };
        let cut = Cut::default();
        spawn_run(held, &request, cut.clone(), look(job, request.clone(), cut));
        Ok(())
    }
}

struct Job {
    data_dir: PathBuf,
    project: ProjectConfig,
    bin: PathBuf,
    model: String,
}

/// How the run ended; `None` when the loop ended it.
async fn look(job: Job, request: RunRequest, cut: Cut) -> Option<(RunOutcome, Value)> {
    let project = &job.project;
    let cwd = base_or_repo(project);
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
    let tx = cut.tree_sender(&request);
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
