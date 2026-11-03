//! The `look` run (docs/specs/pieces.md sections 3.2 and 3.5): one read-only triager-role session on the middle
//! tier, in the project's base checkout as it is (the repo when that doesn't exist yet), that decides what's next
//! for a piece or plan that keeps failing. It takes no lane, and only one look (or plan) runs at a time on this
//! computer, across all projects ([`Slot`]). Its process tree is registered with the run, so a beat's `stop` or
//! the ceiling cuts it; the run ends exactly once through `request.ends`, unless the loop already ended it.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::driver::{RunRequest, RunStarter, piece_id};
use super::plan_run::{Cut, Job, Roster, spawn_run};
use super::session::quiet_timeout;
pub use super::slot::Slot;
use crate::backlog::runs::{Run, RunKind, RunOutcome};
use crate::connector::bug_link::BugLink;
use crate::engines::claude::sandbox_enabled;
use crate::engines::{EngineId, EphemeralSessionConfig, Role, TranscriptSink, engine_of, run_session_tracked};
use crate::looks::{LookBrief, Looker, decide};
use crate::process_tree::ProcessTree;
use crate::tiers::{implementer_tier, mid_tier};
use crate::{lanes, paths};

mod bug;
pub use bug::BugReports;

const TEMPLATE: &str = include_str!("../../prompts/look.md");

/// Runs `look` runs of the projects it was built with, one at a time on this computer.
pub struct LookRuns {
    pub roster: Roster,
    /// Shared with the plan runner: one plan or look at a time.
    pub slot: Slot,
    /// Where a look's Retaskable bug is reported; with no link for the project, nothing is sent.
    pub bugs: BugReports,
}

impl LookRuns {
    pub fn new(roster: Roster, slot: Slot) -> Self {
        let bugs = BugReports {
            links: Default::default(),
            version: String::new(),
            env: roster.env.clone(),
        };
        Self { roster, slot, bugs }
    }

    /// Reports a look's bug over each project's hub link in `links`.
    pub fn with_bugs(mut self, links: HashMap<String, Arc<BugLink>>, version: String) -> Self {
        self.bugs.links = links;
        self.bugs.version = version;
        self
    }
}

impl RunStarter for LookRuns {
    fn kinds(&self) -> Vec<RunKind> {
        self.roster.kinds(Role::Triager, RunKind::Look, &self.slot)
    }

    fn start(&self, request: RunRequest) -> Result<(), String> {
        let model = mid_tier(implementer_tier(
            None,
            crate::telemetry::now_ms() / 1000,
            &self.roster.models,
        ))
        .to_string();
        let (job, guard) = self.roster.begin(Role::Triager, &request, &self.slot, model)?;
        let cut = Cut::default();
        let work = look(job, request.clone(), cut.clone(), self.bugs.clone());
        spawn_run(guard, request, cut, async move { Some(work.await) });
        Ok(())
    }
}

async fn look(job: Job, request: RunRequest, cut: Cut, bugs: BugReports) -> (RunOutcome, Value) {
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
        Some(r) => {
            // Sent from its own task: the end below doesn't wait for the hub.
            bugs.report(&job.project, &request, &r);
            (RunOutcome::Looked, r.to_wire())
        }
        None => (
            RunOutcome::Failed(decided.note.unwrap_or_else(|| "the look gave no decision".into())),
            json!({}),
        ),
    }
}

#[cfg(all(test, unix))]
#[path = "look_run_tests.rs"]
mod tests;
