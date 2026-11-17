//! Whether CI runs for pull requests into the base branch, kept as the store's `ci_coverage` fact
//! (`readCiCoverage` in `agent/src/orchestratorSync.ts`; `kept_facts` keeps it until the store takes it): about once a minute the hash of `.github/workflows` on
//! `origin/<base>` is read, and only when it changed since the last write (or on the first look) are the workflows
//! read and the answer queued. GitHub projects only: the TS agent also answers `covered` for a Woodpecker pipeline or a
//! GitLab CI file, which this agent doesn't read, so a project on another host writes no row rather than a wrong
//! `none`.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

use super::project::ProjectLoop;
use crate::ci_coverage::{ci_coverage, read_workflows, workflows_tree};

/// How often the base branch's workflows are looked at.
const LOOK_EVERY: Duration = Duration::from_secs(60);

/// What a look found: the base branch, the workflows tree and the fact to queue.
type Found = (String, Option<String>, serde_json::Value);

#[derive(Default)]
pub(super) struct State {
    /// The look running off the loop's thread: the fact to queue, or `None` when the workflows hadn't changed.
    task: Option<JoinHandle<Option<Found>>>,
    looked: Option<Instant>,
    /// The base branch and workflows tree last queued.
    written: Option<(String, Option<String>)>,
}

impl ProjectLoop {
    /// One look: queues the fact of a look that came back, and starts the next when a minute has passed.
    pub(super) async fn look_at_ci_coverage(&mut self, now: Instant) {
        if let Some(task) = self.run_loop.ci_coverage.task.take_if(|t| t.is_finished()) {
            if let Ok(Some((base, tree, fact))) = task.await {
                self.run_loop.ci_coverage.written = Some((base, tree));
                crate::kept_facts::shared(self.store.dir()).keep_result(crate::kept_facts::CI_COVERAGE, &fact);
            }
            return;
        }
        let state = &mut self.run_loop.ci_coverage;
        if state.task.is_some()
            || !crate::main_ci::reads(&self.project)
            || state.looked.is_some_and(|t| now.duration_since(t) < LOOK_EVERY)
        {
            return;
        }
        state.looked = Some(now);
        let (repo, base, written) = (
            PathBuf::from(&self.project.repo_path),
            self.project.base_branch.clone(),
            state.written.clone(),
        );
        state.task = Some(tokio::task::spawn_blocking(move || {
            let at = format!("origin/{base}");
            let tree = workflows_tree(&repo, &at);
            if written.as_ref() == Some(&(base.clone(), tree.clone())) {
                return None;
            }
            let coverage = ci_coverage(&read_workflows(&repo, &at), &base);
            let fact = coverage.to_fact(&base, tree.as_deref());
            Some((base, tree, fact))
        }));
    }
}
