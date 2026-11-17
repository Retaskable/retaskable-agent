//! The jobs the store sends as tasks (`task.job`, docs/specs/pools.md section 3f; the TypeScript agent's `runJob` in
//! `jobTasks.ts` and the job chain in `poolTasks.ts`): Lighthouse, the probes, the accessibility sweep, vetting,
//! deprecated specs, triage and release notes. The agent decides nothing: a task is queued, run one at a time (Chrome
//! twice at once is heavy), and ended with its raw result, which is kept like any `task_end` until the store takes it.
//! A job that runs a session (`JobKind::runs_session`) takes one of the computer's session slots while it runs and
//! waits for one; the others run in no lane. Nothing starts while the project is paused or the agent is stopping.
use std::collections::VecDeque;
use std::sync::Arc;

use serde_json::Value;
use tokio::task::JoinHandle;

use super::ProjectLoop;
use crate::backlog::pool::{JobSpec as TaskJob, TaskEnd, TaskInput};
use crate::jobs::host::JobHost;
use crate::jobs::lighthouse_reports::ReportFlush;
use crate::jobs::run::{JobCtx, run_job};
use crate::jobs::store::StoreAccess;
use crate::jobs::{ChromeFlag, JobKind, JobOutcome, JobSpec, end_body, unknown_kind};

struct Running {
    id: String,
    /// The item a triage job looks into.
    item: Option<String>,
    session: bool,
    handle: JoinHandle<JobOutcome>,
}

/// One project's job tasks: the queue, the one running, and what outlives a run (Chrome found missing, the reports
/// waiting for the store).
#[derive(Default)]
pub(super) struct Jobs {
    pub(super) chrome: Arc<ChromeFlag>,
    pub(super) flush: Arc<ReportFlush>,
    /// The smoke tasks, outside the queue (`smoke_task.rs`).
    pub(super) smokes: super::smoke_task::Smokes,
    queue: VecDeque<(String, JobSpec)>,
    running: Option<Running>,
}

impl Jobs {
    /// Whether a job that runs a session is running now (it holds a session slot).
    pub(super) fn session_running(&self) -> bool {
        self.running.as_ref().is_some_and(|r| r.session)
    }
}

/// The item a `triage` job's args name.
fn triaged_item(spec: &JobSpec) -> Option<String> {
    (spec.kind == "triage")
        .then(|| spec.args.get("id").and_then(Value::as_str))
        .flatten()
        .map(str::to_string)
}

fn spec_of(job: &TaskJob) -> JobSpec {
    JobSpec {
        kind: job.kind.clone(),
        day: job.day.clone(),
        args: job
            .args
            .as_ref()
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    }
}

impl ProjectLoop {
    /// A `task.job` the store sent: queued to run, or refused in words when this computer doesn't run that kind.
    pub(super) fn job_task(&mut self, task: &TaskInput, job: &TaskJob, host: Option<&Arc<dyn JobHost>>) {
        let spec = spec_of(job);
        if host.is_none() || JobKind::parse(&spec.kind).is_none() {
            let why = crate::jobs::end_body(&unknown_kind(&spec.kind))["refused"]["why"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            tracing::info!("{}: task {}: {why}", self.id, task.id);
            return self.end_task(TaskEnd::refused(&task.id, why));
        }
        tracing::info!("{}: task {}: a {} job", self.id, task.id, spec.kind);
        self.pool.runs.start_job(&task.id, &spec.kind);
        // Claude is looking into the item while its triage job waits or runs (`claudeOnIt`).
        if let Some(item) = triaged_item(&spec) {
            self.triaging.insert(item);
        }
        self.jobs.queue.push_back((task.id.clone(), spec));
    }

    /// Ends the job that finished, and starts the next one when no job runs and a session slot is free for one that
    /// needs it. Called each pass, before `health` is reported.
    pub(super) async fn jobs_pass(
        &mut self,
        host: Option<&Arc<dyn JobHost>>,
        links: &super::Links,
        paused: bool,
        stopping: bool,
        slots: &mut u32,
    ) {
        if self.jobs.running.as_ref().is_some_and(|r| r.handle.is_finished()) {
            let Running { id, item, handle, .. } = self.jobs.running.take().expect("checked");
            if let Some(item) = item {
                self.triaging.remove(&item);
            }
            let out = match handle.await {
                Ok(out) => out,
                Err(e) => crate::jobs::run_failed::<Value>("job", &e),
            };
            self.pool.runs.end_job(&id);
            self.end_task(TaskEnd::from_job_body(&id, &end_body(&out)));
            self.save_pool();
        }
        let (Some(host), false) = (host, paused || stopping || self.jobs.running.is_some()) else {
            return;
        };
        let Some((_, next)) = self.jobs.queue.front() else {
            return;
        };
        let session = JobKind::parse(&next.kind).is_some_and(JobKind::runs_session);
        if session && *slots == 0 {
            return;
        }
        let (id, spec) = self.jobs.queue.pop_front().expect("checked");
        if session {
            *slots -= 1;
        }
        let handle = self.start_job_run(host.clone(), links, spec.clone());
        let item = triaged_item(&spec);
        self.jobs.running = Some(Running {
            id,
            item,
            session,
            handle,
        });
    }

    fn start_job_run(&self, host: Arc<dyn JobHost>, links: &super::Links, spec: JobSpec) -> JoinHandle<JobOutcome> {
        let project = self.project.clone();
        let id = self.id.clone();
        let (chrome, flush) = (self.jobs.chrome.clone(), self.jobs.flush.clone());
        let data_dir = self.data_dir.clone().unwrap_or_default();
        let store = self.repo.map(|repo| StoreAccess {
            client: self.client.clone(),
            repo,
            access: links.access.clone(),
        });
        let hub = links.bugs.get(&id).cloned();
        let (env, bug_version) = (links.env.clone(), links.bug_version.clone());
        let now_ms = crate::telemetry::now_ms();
        // When a usage limit lifts: the five-hour reset, never sooner than ten minutes (`limitResume`).
        let resets = self
            .store
            .usage()
            .as_ref()
            .and_then(crate::tiers::five_hour_resets_at)
            .unwrap_or(0)
            * 1000;
        let limit_resume_ms = resets.max(now_ms + 10 * 60_000);
        tokio::spawn(async move {
            let item = spec.args.get("id").and_then(Value::as_str).unwrap_or_default();
            let ctx = JobCtx {
                project_id: &id,
                project: &project,
                chrome: &chrome,
                flush: &flush,
                project_dir: &crate::paths::project_dir(&data_dir, &id),
                store,
                hub,
                env: &env,
                bug_version: &bug_version,
                runs_dir: crate::paths::run_dir(&data_dir, &id, item).display().to_string(),
                limit_resume_ms,
            };
            run_job(&ctx, &*host, &spec).await
        })
    }
}
