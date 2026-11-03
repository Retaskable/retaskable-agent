//! A project's loop on runs (docs/specs/pieces.md section 3.1): drop the runs an earlier start left, ask for runs,
//! beat the live ones every minute, and end them. It is reached only through [`ProjectLoop::run_pass`]; the claim
//! path in `project.rs` is separate. What runs and how they are kept live is `crate::runs`; the work of a run
//! is the [`RunStarter`]'s.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::Shared;
use super::project::ProjectLoop;
use crate::backlog::runs::{Run, RunDoneError, RunKind, RunOutcome, RunPiece, RunTakeAgent, RunTakeAnswer, RunWant};
use crate::lane_pool::LanePool;
use crate::lanes::lane_path;
use crate::process_tree::{KILL_LIMIT, ProcessTree};
use crate::runs::{NoLane, Runs, needs_lane};
use crate::telemetry::{Event, event};

/// How often the live runs are beaten.
pub const BEAT_EVERY: Duration = Duration::from_secs(60);
/// The most runs one pass asks for, so a store that keeps handing out work can't hold the pass.
const MAX_ASKS: u32 = 16;

/// How a run ended, as its work reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct RunEnd {
    pub token: String,
    pub outcome: RunOutcome,
    pub result: Value,
}

/// The sending end of the run-end channel: cloned into every [`RunRequest`].
#[derive(Debug, Clone)]
pub struct RunEndSender(mpsc::UnboundedSender<RunEnd>);

impl RunEndSender {
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<RunEnd>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self(sender), receiver)
    }

    /// Reports a run's end. A loop that has stopped hears nothing, which is fine: nothing is left to end.
    pub fn send(&self, end: RunEnd) {
        let _ = self.0.send(end);
    }
}

/// Where a run's work registers each session's process tree, so a beat's `stop` or the ceiling can cut it.
#[derive(Debug, Clone)]
pub struct RunProcesses(mpsc::UnboundedSender<(String, Arc<ProcessTree>)>);

impl RunProcesses {
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<(String, Arc<ProcessTree>)>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self(sender), receiver)
    }

    /// Adds `tree` to the run `token`. Taken up on the loop's next pass; if the run has ended by then, the tree
    /// is cut at once.
    pub fn add(&self, token: &str, tree: Arc<ProcessTree>) {
        if let Err(e) = self.0.send((token.to_string(), tree)) {
            e.0.1.cut();
            e.0.1.kill_and_wait(KILL_LIMIT);
        }
    }
}

/// The id of a piece as the store sent it, or its title when it sent none.
pub fn piece_id(piece: &RunPiece) -> &str {
    piece.other.get("id").and_then(Value::as_str).unwrap_or(&piece.title)
}

/// A run the store handed out, for the starter to do.
#[derive(Debug, Clone)]
pub struct RunRequest {
    pub token: String,
    pub project: String,
    pub processes: RunProcesses,
    pub kind: RunKind,
    /// The piece's brief as the store sent it.
    pub piece: RunPiece,
    /// The folder of the lane the run holds; `None` for a kind that runs in none.
    pub lane: Option<PathBuf>,
    pub ends: RunEndSender,
}

/// Does the work of runs. Built by the pieces that run each kind.
pub trait RunStarter: Send + Sync {
    /// The kinds it can run now, so `run_take` asks for only those.
    fn kinds(&self) -> Vec<RunKind>;
    /// Starts the run and returns at once; it reports its end through `request.ends`. An `Err` gives the run back.
    fn start(&self, request: RunRequest) -> Result<(), String>;
}

/// One project's run state: the live runs and what the loop has done so far.
pub struct RunLoop {
    runs: Runs,
    /// `runs_drop` went through, so asking is allowed.
    dropped: bool,
    /// The store said it waits for something this computer lacks: no asking until `work_available`.
    waiting: bool,
    last_beat: Option<Instant>,
    ends_tx: RunEndSender,
    ends_rx: mpsc::UnboundedReceiver<RunEnd>,
    processes_tx: RunProcesses,
    processes_rx: mpsc::UnboundedReceiver<(String, Arc<ProcessTree>)>,
}

impl RunLoop {
    pub fn new(project: &str) -> Self {
        let (ends_tx, ends_rx) = RunEndSender::channel();
        let (processes_tx, processes_rx) = RunProcesses::channel();
        Self {
            runs: Runs::new(project),
            dropped: false,
            waiting: false,
            last_beat: None,
            ends_tx,
            ends_rx,
            processes_tx,
            processes_rx,
        }
    }

    pub fn runs(&self) -> &Runs {
        &self.runs
    }
}

impl ProjectLoop {
    /// One pass of the run loop at `now`: `runs_drop` first (once; if it fails nothing is taken), then the
    /// ends that came back, the ceilings, the minute's beat, the ends to send, and asking for runs.
    pub async fn run_pass(&mut self, pool: &mut LanePool, starter: &dyn RunStarter, shared: &Shared<'_>, now: Instant) {
        let Some((repo, token)) = self.run_access(shared).await else {
            return;
        };
        let token = Some(token.as_str());
        if shared.links.work.signal(&self.id).notified().now_or_never().is_some() {
            self.run_loop.waiting = false;
        }
        while let Ok((run, tree)) = self.run_loop.processes_rx.try_recv() {
            if !self.run_loop.runs.add_process(&run, tree.clone()) {
                tree.cut();
                tree.kill_and_wait(KILL_LIMIT);
            }
        }
        while let Ok(end) = self.run_loop.ends_rx.try_recv() {
            self.run_loop.runs.end(pool, &end.token, end.outcome, end.result, now);
        }
        self.run_loop.runs.end_past_ceilings(pool, now);
        if self
            .run_loop
            .last_beat
            .is_none_or(|t| now.duration_since(t) >= BEAT_EVERY)
        {
            self.run_loop.last_beat = Some(now);
            let tokens = self.run_loop.runs.beat_tokens();
            if !tokens.is_empty() {
                match self.client.run_beat(repo, token, &tokens).await {
                    Ok(verdicts) => self.run_loop.runs.apply_beat(pool, &verdicts, now),
                    Err(e) => self.store_error("run_beat", &e),
                }
            }
        }
        let due: Vec<_> = self.run_loop.runs.due_ends(now).into_iter().cloned().collect();
        for end in due {
            let answer = self
                .client
                .run_done(repo, token, &end.token, &end.outcome, end.result)
                .await;
            if let Err(RunDoneError::Other(e)) = &answer {
                self.store_error("run_done", e);
            }
            self.run_loop.runs.end_sent(&end.token, answer, now);
        }
        for _ in 0..MAX_ASKS {
            if shared.stopping || self.run_loop.waiting || !self.ask_once(repo, token, pool, starter, shared, now).await
            {
                break;
            }
        }
        crate::runs::report_held(&self.id, self.run_loop.runs.held());
    }

    /// The repo and token the store is asked with, after `runs_drop` went through (once; if it fails nothing is
    /// taken). `None` when this project can't ask yet.
    async fn run_access(&mut self, shared: &Shared<'_>) -> Option<(u64, String)> {
        let repo = self.repo?;
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return None,
            Err(e) => {
                self.cant_start(format!("reading the backlog link: {e:#}"));
                return None;
            }
        };
        if !self.run_loop.dropped {
            match self.client.runs_drop(repo, Some(&token)).await {
                Ok(()) => self.run_loop.dropped = true,
                Err(e) => {
                    self.store_error("runs_drop", &e);
                    return None;
                }
            }
        }
        Some((repo, token))
    }

    /// Asks for one piece by name (`run_take { agent, piece }`), for a person's action on this computer, and starts
    /// the run it gets the same way a kind's run starts. True when a run started.
    pub async fn run_take_piece(
        &mut self,
        pool: &mut LanePool,
        starter: &dyn RunStarter,
        shared: &Shared<'_>,
        piece: &str,
        now: Instant,
    ) -> bool {
        if shared.stopping {
            return false;
        }
        let Some((repo, token)) = self.run_access(shared).await else {
            return false;
        };
        self.ask(repo, Some(&token), pool, starter, shared, RunWant::Piece(piece), now)
            .await
    }

    /// One `run_take`, with its `claim` event. Whether to ask again: a run was handed out and started.
    async fn ask_once(
        &mut self,
        repo: u64,
        token: Option<&str>,
        pool: &mut LanePool,
        starter: &dyn RunStarter,
        shared: &Shared<'_>,
        now: Instant,
    ) -> bool {
        let lane_free = pool.claim_lane(&pool.run_want(&self.id, "")).is_some();
        let kinds: Vec<RunKind> = starter
            .kinds()
            .into_iter()
            .filter(|k| lane_free || !needs_lane(*k))
            .collect();
        if kinds.is_empty() {
            return false;
        }
        self.ask(repo, token, pool, starter, shared, RunWant::Kinds(&kinds), now)
            .await
    }

    /// One `run_take` for `want`, with its `claim` event. Whether a run was handed out and started.
    #[allow(clippy::too_many_arguments)]
    async fn ask(
        &mut self,
        repo: u64,
        token: Option<&str>,
        pool: &mut LanePool,
        starter: &dyn RunStarter,
        shared: &Shared<'_>,
        want: RunWant<'_>,
        now: Instant,
    ) -> bool {
        let agent = RunTakeAgent {
            machine: shared.links.machine.clone(),
            environment: shared.links.environment.clone(),
            base_branch: self.project.base_branch.clone(),
            tags: shared.links.tags.clone(),
        };
        let (answer, more) = match self.client.run_take(repo, token, &agent, want).await {
            Err(e) => {
                self.store_error("run_take", &e);
                (json!("failed"), false)
            }
            Ok(RunTakeAnswer::Nothing) => (json!("none"), false),
            Ok(RunTakeAnswer::WaitingFor(waiting)) => {
                self.run_loop.waiting = true;
                (json!({ "waitingFor": waiting }), false)
            }
            Ok(RunTakeAnswer::Run(run)) => {
                let answer = json!(run.piece.title);
                let more = self.begin(repo, token, pool, starter, *run, now).await;
                (answer, more)
            }
        };
        self.events.add_in(
            &self.id,
            Event::Claim(event::Claim {
                answer: Some(answer),
                ..Default::default()
            }),
        );
        more
    }

    /// Starts a run the store handed out. False when it didn't start, and the store has it back as `interrupted`.
    async fn begin(
        &mut self,
        repo: u64,
        token: Option<&str>,
        pool: &mut LanePool,
        starter: &dyn RunStarter,
        run: Run,
        now: Instant,
    ) -> bool {
        let piece_id = piece_id(&run.piece).to_string();
        if let Err(NoLane) = self.run_loop.runs.start(pool, &run.token, run.kind, &piece_id, now) {
            // Not queued: the run never held anything here, so its end is sent now, once.
            let ended = self
                .client
                .run_done(repo, token, &run.token, &RunOutcome::Interrupted, json!({}))
                .await;
            if let Err(RunDoneError::Other(e)) = &ended {
                self.store_error("run_done", e);
            }
            return false;
        }
        let lane = self
            .run_loop
            .runs
            .live()
            .iter()
            .find(|r| r.token == run.token)
            .and_then(|r| r.lane)
            .map(|k| lane_path(Path::new(&self.project.worktrees_dir), k));
        let request = RunRequest {
            token: run.token.clone(),
            project: self.id.clone(),
            processes: self.run_loop.processes_tx.clone(),
            kind: run.kind,
            piece: run.piece,
            lane,
            ends: self.run_loop.ends_tx.clone(),
        };
        if let Err(why) = starter.start(request) {
            tracing::warn!("{}: the run {} didn't start: {why}", self.id, run.token);
            self.run_loop
                .runs
                .end(pool, &run.token, RunOutcome::Interrupted, json!({}), now);
            return false;
        }
        true
    }
}
