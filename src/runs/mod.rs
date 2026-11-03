//! This computer's live runs, in memory only (docs/specs/pieces.md section 3.1 "Run", "Beat" and "End"; ceilings in
//! section 3.3). Pure: time is passed in, and the store calls (`run_take`, `run_beat`, `run_done`) are the caller's;
//! it hands them the tokens to beat and the ends to send, and takes their answers back. Nothing is written to
//! state.json.
mod build_end;
mod locks;
mod outside;
mod wind;
pub use build_end::{Pushed, change_json, end_from_answer, needs_push, questions_json};
pub use locks::{RunLocks, lock_ceiling};
pub use outside::is_outside;
pub use wind::{RunWait, WAIT_LIMIT, held_count, report_held, winding_down};

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::backlog::runs::{BeatVerdict, RunDoneError, RunKind, RunOutcome};
use crate::lane_pool::{Holder, LanePool};
use crate::process_tree::{KILL_LIMIT, ProcessTree};

/// How long before its ceiling the agent ends a run itself, so the store never has to.
pub const CEILING_MARGIN: Duration = Duration::from_secs(5 * 60);
/// How long after a refused `run_done` it is sent again.
pub const END_RETRY: Duration = Duration::from_secs(60);

/// A kind's ceiling (section 3.3): the store loses a run past it, even while it beats.
pub fn ceiling(kind: RunKind) -> Duration {
    let hours = match kind {
        RunKind::Build | RunKind::Fix => 3,
        RunKind::Watch => 6,
        RunKind::Plan | RunKind::Look | RunKind::Merge => 1,
    };
    Duration::from_secs(hours * 3600)
}

/// Whether the kind runs in a lane (section 3.2's Lane column).
pub fn needs_lane(kind: RunKind) -> bool {
    matches!(kind, RunKind::Build | RunKind::Fix)
}

/// Set once the loop has ended a run (a beat's `stop`, its ceiling, or its own end), so a worker with no process
/// running at that moment still learns of it. Clones share one flag.
#[derive(Clone, Debug, Default)]
pub struct RunStop(Arc<AtomicBool>);

impl RunStop {
    pub fn stopped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A run in progress here.
pub struct LiveRun {
    pub stop: RunStop,
    pub token: String,
    pub kind: RunKind,
    pub piece: String,
    pub lane: Option<u32>,
    /// The run's process trees, so it can be cut whole.
    pub processes: Vec<Arc<ProcessTree>>,
    pub started: Instant,
}

/// A `run_done` waiting to be sent (or sent again).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingEnd {
    pub token: String,
    pub outcome: RunOutcome,
    pub result: Value,
    pub due: Instant,
}

/// No lane was free for a run that needs one.
#[derive(Debug, PartialEq, Eq)]
pub struct NoLane;

/// One project's live runs and the ends not yet accepted.
pub struct Runs {
    project: String,
    live: Vec<LiveRun>,
    ends: Vec<PendingEnd>,
    locks: RunLocks,
}

impl Runs {
    pub fn new(project: &str) -> Self {
        Self {
            project: project.into(),
            live: Vec::new(),
            ends: Vec::new(),
            locks: RunLocks::default(),
        }
    }

    /// What still has to be said to the store: live runs, ends not yet accepted, and locks still held.
    pub fn held(&self) -> usize {
        self.live.len() + self.ends.len() + self.locks.count()
    }

    /// The handle a run's worker takes and drops store locks through; the beat renews them.
    pub fn locks(&self) -> RunLocks {
        self.locks.clone()
    }

    pub fn live(&self) -> &[LiveRun] {
        &self.live
    }

    /// Starts a run the store handed out. `build` and `fix` take a lane from `pool` (the piece's last one if free);
    /// with none free nothing starts and the caller ends the token as `interrupted`.
    pub fn start(
        &mut self,
        pool: &mut LanePool,
        token: &str,
        kind: RunKind,
        piece: &str,
        now: Instant,
    ) -> Result<(), NoLane> {
        let lane = if needs_lane(kind) {
            Some(pool.claim_run(&self.project, piece).ok_or(NoLane)?)
        } else {
            None
        };
        self.live.push(LiveRun {
            stop: RunStop::default(),
            token: token.into(),
            kind,
            piece: piece.into(),
            lane,
            processes: Vec::new(),
            started: now,
        });
        Ok(())
    }

    /// Adds a process tree to a run. False when the run is gone.
    pub fn add_process(&mut self, token: &str, tree: Arc<ProcessTree>) -> bool {
        match self.live.iter_mut().find(|r| r.token == token) {
            Some(run) => {
                // A tree whose worker let go of it has finished: cutting it later would signal a gone process group.
                run.processes.retain(|t| Arc::strong_count(t) > 1);
                run.processes.push(tree);
                true
            }
            None => false,
        }
    }

    /// Every live run's and held lock's token, for `run_beat`.
    pub fn beat_tokens(&self) -> Vec<String> {
        let mut tokens: Vec<String> = self.live.iter().map(|r| r.token.clone()).collect();
        tokens.extend(self.locks.tokens());
        tokens
    }

    /// Applies `run_beat`'s answer: a `stop` cuts that run's trees and ends it `interrupted`, or marks that lock lost.
    pub fn apply_beat(&mut self, pool: &mut LanePool, verdicts: &HashMap<String, BeatVerdict>, now: Instant) {
        self.locks.apply_beat(verdicts);
        let stopped: Vec<String> = self
            .live
            .iter()
            .filter(|r| verdicts.get(&r.token) == Some(&BeatVerdict::Stop))
            .map(|r| r.token.clone())
            .collect();
        for token in stopped {
            self.cut_and_end(pool, &token, RunOutcome::Interrupted, now);
        }
    }

    /// Ends every run within [`CEILING_MARGIN`] of its kind's ceiling, as `failed` naming the ceiling, and marks the
    /// locks within it lost. Returns
    /// their tokens.
    pub fn end_past_ceilings(&mut self, pool: &mut LanePool, now: Instant) -> Vec<String> {
        self.locks.mark_past_ceilings(now);
        let over: Vec<(String, Duration)> = self
            .live
            .iter()
            .filter(|r| now.saturating_duration_since(r.started) >= ceiling(r.kind).saturating_sub(CEILING_MARGIN))
            .map(|r| (r.token.clone(), ceiling(r.kind)))
            .collect();
        for (token, limit) in &over {
            let why = format!(
                "The run reached its {}-hour ceiling, so the agent ended it.",
                limit.as_secs() / 3600
            );
            self.cut_and_end(pool, token, RunOutcome::Failed(why), now);
        }
        over.into_iter().map(|(t, _)| t).collect()
    }

    fn cut_and_end(&mut self, pool: &mut LanePool, token: &str, outcome: RunOutcome, now: Instant) {
        if let Some(run) = self.live.iter().find(|r| r.token == token) {
            for tree in &run.processes {
                tree.cut();
                tree.kill_and_wait(KILL_LIMIT);
            }
        }
        self.end(pool, token, outcome, json!({}), now);
    }

    /// Ends a run: its lane goes back whatever the outcome, and its `run_done` is queued, due now. False when no
    /// such run is live.
    pub fn end(&mut self, pool: &mut LanePool, token: &str, outcome: RunOutcome, result: Value, now: Instant) -> bool {
        let Some(at) = self.live.iter().position(|r| r.token == token) else {
            return false;
        };
        let run = self.live.remove(at);
        run.stop.set();
        if let Some(lane) = run.lane {
            pool.release(
                lane,
                &Holder::Run {
                    project: self.project.clone(),
                    piece: run.piece,
                },
            );
        }
        self.ends.push(PendingEnd {
            token: run.token,
            outcome,
            result,
            due: now,
        });
        true
    }

    /// The ends to send now.
    pub fn due_ends(&self, now: Instant) -> Vec<&PendingEnd> {
        self.ends.iter().filter(|e| e.due <= now).collect()
    }

    /// What the store said to a `run_done`: accepted or `stale_run` drops it (a stale run's work is thrown away,
    /// nothing else happens); any other refusal is due again a minute later.
    pub fn end_sent(&mut self, token: &str, answer: Result<(), RunDoneError>, now: Instant) {
        match answer {
            Ok(()) | Err(RunDoneError::Stale) => self.ends.retain(|e| e.token != token),
            Err(RunDoneError::Other(_)) => {
                if let Some(e) = self.ends.iter_mut().find(|e| e.token == token) {
                    e.due = now + END_RETRY;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
