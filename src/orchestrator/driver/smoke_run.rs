//! The staging smoke after a merge (docs/specs/agent/06.3-review.md, "After the merge: the staging smoke"; `agent/src/smoke.ts`
//! and its `afterMerge` and `resume`). Every merge this computer makes sets [`State::merged`]; the next pass fetches and takes
//! `origin/<base>`'s head as the merged commit, and a smoke waits off the loop's thread until staging serves it
//! (`crate::smoke::run_with`) and runs the project's smoke command. One at a time per project, the newest merge only: a merge
//! while one still waits for its deploy calls that wait off and smokes the newer commit (its deploy has the older one's changes
//! too); one already running its command is let finish.
//!
//! What the computer tells the store, and nothing more: each state (`waiting`, `running`, then `passed`, `failed` or `timeout`)
//! is the store's `smoke` fact ([`crate::kept_facts`] holds it until the store takes it), and a failure is reported as
//! `chore_trigger staging_smoke { sha, url, command, tail }`. The store decides whether a fix follows (one open at a time,
//! chores off) and sends it as a task; this agent adds no chore and takes no lock. A trigger the store did not answer is kept
//! and sent again on the next pass (a restart forgets it, as the TS agent does); any answer, a skip too, ends it. A timeout and
//! a local problem (git, the filesystem) are only a status.
//!
//! A smoke a restart cut off (the fact says `waiting` or `running` and names this computer) starts again.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;

use super::project::ProjectLoop;
use crate::backlog::pool::Trigger;
use crate::kept_facts::{self, SMOKE};
use crate::smoke;
use crate::store::{SmokeState, SmokeStatus};
use crate::telemetry::events::{iso_utc_millis, now_ms};

type Outcome = anyhow::Result<Option<SmokeState>>;

/// A smoke under way.
struct Flight {
    sha: String,
    task: JoinHandle<Outcome>,
    /// Set by a newer merge (or a stop) to call the wait for the deploy off.
    cancel: Arc<AtomicBool>,
    /// The `running` state the task reports when it starts the command.
    running: Arc<Mutex<Option<SmokeState>>>,
    /// The `running` state was kept as a fact.
    told: bool,
}

#[derive(Default)]
pub(super) struct State {
    /// The newest merged commit still to smoke.
    next: Option<String>,
    /// A merge happened: the next pass takes the base branch's head after a fetch.
    merged: bool,
    head: Option<JoinHandle<Option<String>>>,
    flight: Option<Flight>,
    /// The store's `smoke` fact was read once for a smoke a restart cut off.
    resumed: bool,
    /// A failure the store has not answered yet.
    trigger: Option<Trigger>,
}

impl State {
    /// A merge this computer made: its commit is smoked on the next pass.
    pub(super) fn merge_made(&mut self) {
        self.merged = true;
    }
}

impl ProjectLoop {
    /// One pass of the smoke: collects what finished, resumes a cut-off smoke once, takes the merged commit and starts
    /// the next smoke when none runs.
    pub(super) async fn smoke_pass(&mut self, repo: u64, token: Option<&str>, machine: &str, stopping: bool) {
        self.send_smoke_trigger(repo, token).await;
        if let Some(flight) = self.run_loop.smoke.flight.take_if(|f| f.task.is_finished()) {
            let sha = flight.sha;
            match flight.task.await {
                Ok(Ok(Some(state))) => self.smoked(state),
                Ok(Ok(None)) => {} // called off for a newer merge
                // A local problem (git, the filesystem), not staging's: shown as failed, but no fix is asked for.
                Ok(Err(e)) => self.keep_smoke(&sha, SmokeStatus::Failed, Some(format!("{e:#}"))),
                Err(e) => tracing::warn!("{}: the staging smoke task failed: {e}", self.id),
            }
        }
        let Some(target) = smoke::enabled(&self.project).cloned() else {
            let st = &mut self.run_loop.smoke;
            (st.next, st.merged) = (None, false);
            return;
        };
        if !self.run_loop.smoke.resumed {
            match self.client.facts_get(repo, token, &[SMOKE]).await {
                Ok(facts) => {
                    self.run_loop.smoke.resumed = true;
                    let ours = facts.get(SMOKE).filter(|f| f.machine.as_deref() == Some(machine));
                    if let Some(state) = ours.and_then(|f| serde_json::from_value::<SmokeState>(f.data.clone()).ok())
                        && state.resumable()
                    {
                        self.run_loop.smoke.next = Some(state.sha);
                    }
                }
                Err(e) => self.store_error("facts_get", &e),
            }
        }
        self.take_merged_head();
        let st = &mut self.run_loop.smoke;
        if let Some(head) = st.head.take_if(|h| h.is_finished())
            && let Ok(Some(sha)) = head.await
        {
            self.run_loop.smoke.next = Some(sha);
        }
        let st = &mut self.run_loop.smoke;
        if let Some(flight) = &mut st.flight {
            let waiting = flight.running.lock().unwrap().is_none();
            // A newer merge replaces a smoke still waiting for its deploy; a stop calls it off to resume on restart.
            if waiting && (st.next.is_some() || stopping) {
                flight.cancel.store(true, Ordering::SeqCst);
                // Called off by a stop, not a newer merge: the fact still says `waiting`, so it is read again and the
                // smoke starts again once passes are allowed to start things.
                st.resumed &= st.next.is_some();
            }
            let running = flight.running.lock().unwrap().clone();
            if let (Some(state), false) = (running, flight.told) {
                flight.told = true;
                kept_facts::shared(self.store.dir()).keep_result(SMOKE, &state.to_fact());
            }
            return;
        }
        if stopping {
            return;
        }
        let Some(sha) = st.next.take() else {
            return;
        };
        self.keep_smoke(&sha, SmokeStatus::Waiting, None);
        let (cancel, running) = (Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(None)));
        let p = &self.project;
        let (repo_path, worktrees) = (
            std::path::PathBuf::from(&p.repo_path),
            std::path::PathBuf::from(&p.worktrees_dir),
        );
        let (flag, slot, at) = (cancel.clone(), running.clone(), sha.clone());
        let task = tokio::task::spawn_blocking(move || {
            smoke::run_with(
                &repo_path,
                &worktrees,
                &target,
                &at,
                &|| flag.load(Ordering::SeqCst),
                &|state| *slot.lock().unwrap() = Some(state),
            )
        });
        self.run_loop.smoke.flight = Some(Flight {
            sha,
            task,
            cancel,
            running,
            told: false,
        });
    }

    /// A merge was made: starts taking the base branch's head after a fetch, off the loop's thread.
    fn take_merged_head(&mut self) {
        let st = &mut self.run_loop.smoke;
        if !st.merged || st.head.is_some() {
            return;
        }
        st.merged = false;
        let (repo_path, base) = (
            std::path::PathBuf::from(&self.project.repo_path),
            self.project.base_branch.clone(),
        );
        let id = self.id.clone();
        st.head = Some(tokio::task::spawn_blocking(move || {
            smoke::remote_head_sha(&repo_path, &base)
                .inspect_err(|e| tracing::warn!("{id}: the merged commit couldn't be read for the smoke: {e:#}"))
                .ok()
        }));
    }

    /// Keeps `status` for `sha` as the `smoke` fact, with the project's staging url.
    fn keep_smoke(&mut self, sha: &str, status: SmokeStatus, tail_text: Option<String>) {
        let url = smoke::enabled(&self.project).and_then(|t| t.url.clone());
        let state = SmokeState {
            sha: sha.to_string(),
            status,
            at: iso_utc_millis(now_ms()),
            url,
            tail: tail_text,
        };
        kept_facts::shared(self.store.dir()).keep_result(SMOKE, &state.to_fact());
    }

    /// A smoke ended: its state is kept as the fact, and a failure is held to tell the store.
    pub(super) fn smoked(&mut self, state: SmokeState) {
        kept_facts::shared(self.store.dir()).keep_result(SMOKE, &state.to_fact());
        if state.status != SmokeStatus::Failed {
            return;
        }
        let command = smoke::enabled(&self.project).and_then(|t| t.smoke.clone());
        self.run_loop.smoke.trigger = Some(Trigger::StagingSmoke {
            sha: state.sha,
            url: state.url.unwrap_or_default(),
            command: command.unwrap_or_default(),
            tail: state.tail.unwrap_or_default(),
        });
    }

    /// Tells the store of a failed smoke (`chore_trigger staging_smoke`); the store decides whether a fix follows and
    /// what it says. Kept and sent again while the store gives no answer.
    async fn send_smoke_trigger(&mut self, repo: u64, token: Option<&str>) {
        let Some(trigger) = self.run_loop.smoke.trigger.clone() else {
            return;
        };
        match self.client.chore_trigger(repo, token, &trigger).await {
            Ok(answer) => {
                tracing::info!("{}: the staging smoke failed; the store answered {answer:?}", self.id);
                self.run_loop.smoke.trigger = None;
            }
            // A refusal that names a code (the report is out of shape) would be refused again: dropped; else kept.
            Err(e) if e.is_refusal() => {
                self.store_error("chore_trigger", &e);
                self.run_loop.smoke.trigger = None;
            }
            Err(e) => self.store_error("chore_trigger", &e),
        }
    }
}
