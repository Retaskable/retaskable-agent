//! The trial plan's step (first-run-check.md section 3.3, "When it runs"): once per project, in `claim` mode, behind
//! the `trial-plan-draft` release flag. A project with a repo, passing setup checks and an empty backlog takes a free
//! lane (ahead of chores, never in front of roadmap work already holding lanes) and one session slot, saves `running`
//! in its state, and runs [`TrialHost::run`]; a backlog with items saves `skipped` and never starts one later. On a
//! later pass the end is taken in: `Opened` saves `open` with the PR's URL, `Dropped` saves `failed` and logs why in
//! the project's event log (never on a backlog card), and the lane goes back either way. A restart that finds
//! `running` with no session takes it as dropped: no second trial.
use std::sync::Arc;

use anyhow::Result;
use tokio::task::JoinHandle;

use super::Shared;
use super::machine::SetupChecksHandle;
use super::project::{ProjectLoop, signed_out};
use crate::implementer::session::new_session_id;
use crate::lane_pool::{Holder, LanePool};
use crate::orchestrator::trial_host::{TrialEnd, TrialHost, TrialJob};
use crate::telemetry::{Event, event};
use crate::trial_plan::{Decision, TRIAL_BRANCH, TrialRecord, TrialState, should_start};

/// The release flag that gates the whole step.
const FLAG: &str = "trial-plan-draft";

/// The trial session running now: its lane and task, in memory only.
pub(super) struct RunningTrial {
    lane: u32,
    task: JoinHandle<Result<TrialEnd>>,
}

impl ProjectLoop {
    /// Whether the trial's session is running here now.
    pub(super) fn trialing(&self) -> bool {
        self.trial.is_some()
    }

    fn holder(&self) -> Holder {
        Holder::Trial {
            project: self.id.clone(),
        }
    }

    fn save_trial(&mut self, record: TrialRecord) -> bool {
        match self.store.set_trial(record) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("{}: saving the trial plan's state failed: {e:#}", self.id);
                false
            }
        }
    }

    fn trial_dropped(&mut self, reason: String) {
        self.save_trial(TrialRecord::dropped());
        self.events.add_in(
            &self.id,
            Event::TrialDropped(event::TrialDropped {
                reason: Some(reason),
                ..Default::default()
            }),
        );
    }

    /// Takes in a finished trial, then (unless `paused`) starts one when it's due and `slots` and a lane allow.
    pub(super) async fn trial_pass(
        &mut self,
        pool: &mut LanePool,
        slots: &mut u32,
        paused: bool,
        host: &Arc<dyn TrialHost>,
        checks: &SetupChecksHandle,
        shared: &Shared<'_>,
    ) {
        if self.trial.as_ref().is_some_and(|t| t.task.is_finished()) {
            let RunningTrial { lane, task } = self.trial.take().expect("checked above");
            pool.release(lane, &self.holder());
            match task.await {
                Ok(Ok(TrialEnd::Opened { pr_url })) => {
                    self.save_trial(TrialRecord::opened(pr_url.clone()));
                    self.events.add_in(
                        &self.id,
                        Event::TrialOpened(event::TrialOpened {
                            pr_url: Some(pr_url),
                            ..Default::default()
                        }),
                    );
                }
                Ok(Ok(TrialEnd::Dropped { reason })) => self.trial_dropped(reason),
                Ok(Err(e)) => self.trial_dropped(format!("{e:#}")),
                Err(e) => self.trial_dropped(format!("the session stopped: {e}")),
            }
        }
        let record = self.store.trial();
        if self.trial.is_some() {
            return;
        }
        if record.state == TrialState::Running {
            return self.trial_dropped("the agent restarted while the trial plan was running".into());
        }
        if paused || *slots == 0 || shared.stopping || signed_out(shared) || !self.flags.on(FLAG).await {
            return;
        }
        let checks = checks.lock().unwrap().clone();
        match should_start(&record, &self.project, checks.as_ref(), self.backlog_items) {
            Decision::Wait => {}
            Decision::Skip => {
                self.save_trial(TrialRecord::skip());
            }
            Decision::Start => {
                let Some(lane) = pool.free_lane(false, None, None, |_| true) else {
                    return;
                };
                if !pool.take(lane, self.holder()) || !self.save_trial(TrialRecord::started()) {
                    pool.release(lane, &self.holder());
                    return;
                }
                *slots -= 1;
                tracing::info!("{}: starting the trial plan in lane {lane}", self.id);
                let run = host.run(TrialJob {
                    project: self.project.clone(),
                    lane,
                    branch: TRIAL_BRANCH.to_string(),
                    session_id: new_session_id(),
                    usage: self.store.usage(),
                    placement: self.flags.on("engine-placement").await,
                    sessions: self.live.sessions().clone(),
                });
                self.trial = Some(RunningTrial {
                    lane,
                    task: tokio::spawn(run),
                });
            }
        }
    }
}
