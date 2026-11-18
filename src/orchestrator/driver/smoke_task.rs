//! The staging smoke, as the store's task (`job.kind == "smoke"`, docs/specs/agent/06.3-review.md; the TypeScript agent's
//! `runSmokeTask` in `smoke.ts`, started with `void go()` in `poolTasks.ts`). The store sends it after a merge into the
//! branch staging deploys from, with `{ sha, url, command }`; the agent is a plain client of it. It runs outside the job
//! chain (`job_tasks.rs`): it takes no session slot and no lane, and it waits for a deploy for up to 20 minutes, which
//! must not hold the other jobs up. It is reported in `health.running` until it ends, never run twice (the task's id is
//! seen once, `TaskRuns::fresh`), and ended with `{ sha, outcome, tail? }` through the same owed-end path as any task. It
//! writes no fact and sends no trigger: the store does both from the result.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tokio::task::JoinHandle;

use super::ProjectLoop;
use crate::backlog::pool::{JobSpec as TaskJob, TaskEnd, TaskInput};
use crate::smoke::{self, Ran, Timing};

/// What `kind` names for this task.
pub(super) const KIND: &str = "smoke";

struct Running {
    id: String,
    sha: String,
    handle: JoinHandle<Ran>,
}

/// One project's smoke tasks under way, and the flag that calls their waits off when the agent stops.
#[derive(Default)]
pub(super) struct Smokes {
    running: Vec<Running>,
    cancel: Arc<AtomicBool>,
    /// How long each part waits ([`Timing::waiting`] with the store's number; a test gives short ones).
    timing: Option<Timing>,
}

impl Smokes {
    #[cfg(test)]
    pub(super) fn set_timing(&mut self, timing: Timing) {
        self.timing = Some(timing);
    }
}

impl Drop for Smokes {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl ProjectLoop {
    /// A smoke task the store sent: refused in words when this computer can't vouch for it (smoke::accept), else started
    /// on its own thread, reported running until it ends.
    pub(super) fn smoke_task(&mut self, task: &TaskInput, job: &TaskJob) {
        let args = job
            .args
            .as_ref()
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let sha = match smoke::accept(&self.project, &args) {
            Ok(sha) => sha,
            Err(why) => {
                tracing::info!("{}: smoke task {}: refused: {why}", self.id, task.id);
                return self.end_task(TaskEnd::refused(&task.id, why));
            }
        };
        let Some(target) = smoke::enabled(&self.project).cloned() else {
            return;
        };
        // How long the deploy is given is the store's number; unknown, the task is let go and comes again.
        let Some(deploy_wait) = self.project.tuning.store.map(|t| t.deploy_wait()) else {
            return self.pool.runs.forget(&task.id);
        };
        tracing::info!(
            "{}: smoke task {}: waiting for staging to serve {sha}",
            self.id,
            task.id
        );
        self.pool.runs.start_job(&task.id, KIND);
        let p = &self.project;
        let (repo_path, worktrees) = (
            std::path::PathBuf::from(&p.repo_path),
            std::path::PathBuf::from(&p.worktrees_dir),
        );
        let (flag, timing, at) = (self.jobs.smokes.cancel.clone(), self.jobs.smokes.timing, sha.clone());
        let handle = tokio::task::spawn_blocking(move || {
            smoke::run_with(
                &repo_path,
                &worktrees,
                &target,
                &at,
                &timing.unwrap_or_else(|| Timing::waiting(deploy_wait)),
                &|| flag.load(Ordering::SeqCst),
            )
        });
        self.jobs.smokes.running.push(Running {
            id: task.id.clone(),
            sha,
            handle,
        });
    }

    /// Ends the smokes that finished, and calls the waits off while the agent stops (the store sends the task again; a
    /// stop must not wait out a 20-minute deploy). Called each pass, before `health` is reported.
    pub(super) async fn smokes_pass(&mut self, stopping: bool) {
        if stopping {
            self.jobs.smokes.cancel.store(true, Ordering::SeqCst);
        }
        let (done, running): (Vec<_>, Vec<_>) = std::mem::take(&mut self.jobs.smokes.running)
            .into_iter()
            .partition(|r| r.handle.is_finished());
        self.jobs.smokes.running = running;
        for Running { id, sha, handle } in done {
            self.pool.runs.end_job(&id);
            let ran = match handle.await {
                Ok(ran) => ran,
                Err(e) => Ran::Failed(format!("The smoke run failed: {e}")),
            };
            match smoke::end_body(&sha, &ran) {
                Some(body) => self.end_task(TaskEnd::from_job_body(&id, &body)),
                None => tracing::info!("{}: smoke task {id} called off", self.id),
            }
        }
        self.save_pool();
    }
}
