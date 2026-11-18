//! Advancing every non-terminal item one step (docs/specs/agent/07-orchestrator-loop.md: "advance every
//! non-terminal item one step (non-blocking; sessions are child processes)"; `Orchestrator.advance` and
//! `runStep` in Retaskable.com's `agent/src/orchestrator.ts`). The loop only schedules: each step runs as its
//! own task, at most one per item at a time, and hands back the item's run as the step left it. What a
//! step does (checks, a review, a merge) is the [`SessionStarter`]'s; an `implementing` item's step is its
//! session, which reports its end over the outcome channel instead.
//!
//! [`SessionStarter`]: super::driver::SessionStarter
use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use tokio::task::JoinSet;

use crate::item_lifecycle::{ItemRun, ItemState};
use crate::scouts::{DiffInfo, ScoutJob};
use crate::store::FlowRecord;

/// Whether an item in `state` has a step for the loop to run (the TS agent's step table in `advance`). A
/// `queued` item is started instead (`decide`), and the rest wait on a person or are finished.
pub fn has_step(state: ItemState) -> bool {
    use ItemState::*;
    matches!(
        state,
        Implementing | AwaitingCi | Checking | Reviewing | Merging | AwaitingMerge | NeedsHuman
    )
}

/// `needsLane` in `agent/src/steps.ts`: only these states run work in a lane (04-architecture.md section 4.1).
pub fn needs_lane(state: ItemState) -> bool {
    matches!(
        state,
        ItemState::Implementing | ItemState::Checking | ItemState::Reviewing
    )
}

/// The state's name in the event log (05-item-lifecycle.md's own spelling).
pub fn state_name(state: ItemState) -> &'static str {
    use ItemState::*;
    match state {
        Queued => "queued",
        Implementing => "implementing",
        NeedsClarification => "needs_clarification",
        AwaitingCi => "awaiting_ci",
        Checking => "checking",
        Reviewing => "reviewing",
        AwaitingPlaytest => "awaiting_playtest",
        AwaitingMerge => "awaiting_merge",
        Merging => "merging",
        Merged => "merged",
        Done => "done",
        NeedsHuman => "needs_human",
    }
}

/// What a step is given: the item, its run as the loop has it, the lane it holds and its lease token.
#[derive(Debug, Clone, PartialEq)]
pub struct StepRequest {
    pub project: String,
    pub item_id: String,
    pub run: ItemRun,
    pub lane: Option<u32>,
    pub token: String,
    /// The slice's branch, and what the chain after coding has kept for the item (the change it opened, the
    /// evidence the checks gathered).
    pub branch: Option<String>,
    pub flow: FlowRecord,
    /// The item as the store handed it out: its title, slice, specs and size. `None` when the loop has none.
    pub item: Option<crate::backlog::Item>,
    /// The project as the loop holds it now: the allowed commands with the store's numbers (`health`'s `tuning`) and
    /// settings laid over. The starter's own copy is the file's and has neither, and a step that needs a number of the
    /// store's waits for it (fail closed). `None` (tests only) steps with the starter's copy.
    pub config: Option<Box<crate::config::ProjectConfig>>,
}

/// How long one check took, as a step reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckTime {
    pub name: String,
    pub ok: bool,
    pub sec: f64,
}

/// What a step reports besides the run: the detail its `step_end` carries.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StepDetail {
    pub checks: Vec<CheckTime>,
    pub lines: Option<u64>,
    pub scouts: Option<u32>,
}

impl StepDetail {
    /// A review's detail: the diff lines it read and how many scouts ran (0 when the reviewer read a
    /// small diff alone).
    pub fn review(diff: &DiffInfo, jobs: &[ScoutJob]) -> Self {
        Self {
            lines: Some(u64::from(diff.total_lines)),
            scouts: Some(jobs.len() as u32),
            ..Default::default()
        }
    }
}

/// How a merge step's turn ended, for the store's merge queue (`merge_result`) and the item's completion.
#[derive(Debug, Clone, PartialEq)]
pub enum MergeEnd {
    /// The change landed; `commit` is the merge commit, when the host said.
    Merged {
        commit: Option<String>,
    },
    /// The branch was brought up to date with the base and goes to CI again.
    Updated {
        head: String,
    },
    /// GitHub's auto-merge or queue lands it; the turn is kept.
    HandedOff,
    /// The change was closed, or went to a person to land.
    Closed,
    SentBack {
        reason: String,
    },
    NeedsPerson {
        reason: String,
    },
}

/// What a step hands back: the item's run as it left it, and its detail.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StepDone {
    pub run: ItemRun,
    pub detail: StepDetail,
    /// Lines for the item's timeline (the TS agent's `store.note` and `update`'s note).
    pub notes: Vec<String>,
    /// The item's flow record as the step left it; `None` leaves it as it was.
    pub flow: Option<FlowRecord>,
    /// The item goes back to its implementer (`run.state` is `implementing`): this is the message its session is
    /// resumed with (`sendBack`).
    pub send_back: Option<String>,
    /// A merge step's end, for the store's merge queue; `None` for every other step and for a merge that waits.
    pub merge: Option<MergeEnd>,
}

impl StepDone {
    /// This end with what the step reports besides the run.
    pub fn with_detail(mut self, detail: StepDetail) -> Self {
        self.detail = detail;
        self
    }
}

impl From<ItemRun> for StepDone {
    fn from(run: ItemRun) -> Self {
        Self {
            run,
            ..Default::default()
        }
    }
}

/// One project's steps still running, by item.
#[derive(Default)]
pub struct Running {
    busy: BTreeSet<String>,
    tasks: JoinSet<(String, Option<StepDone>, Duration)>,
    /// How long each finished step's own work took, until [`took`](Self::took) reads it.
    took: BTreeMap<String, Duration>,
    /// Raised when a step ends in a state other than the one it began in, so the loop's next pass steps the new
    /// state at once (the TS `runStep`'s `setImmediate(tick)`). A step that got nowhere waits for the next poll.
    kick: Option<Arc<tokio::sync::Notify>>,
}

impl Running {
    /// Whether a step for `item` is still running.
    pub fn busy(&self, item: &str) -> bool {
        self.busy.contains(item)
    }

    /// Makes a step that moves its item on raise `kick` (the project loop's wake).
    pub fn kicked_by(&mut self, kick: Arc<tokio::sync::Notify>) {
        self.kick = Some(kick);
    }

    /// Runs `step` for `item`, which was in `before`, as its own task. A panic in it is caught: the item keeps the
    /// run it had. The task times its own work, and wakes the loop when the item moved on or the step panicked.
    pub fn spawn(&mut self, item: String, before: ItemState, step: BoxFuture<'static, StepDone>) {
        self.busy.insert(item.clone());
        let kick = self.kick.clone();
        self.tasks.spawn(async move {
            let began = Instant::now();
            let run = AssertUnwindSafe(step).catch_unwind().await.ok();
            let took = began.elapsed();
            if run.as_ref().is_none_or(|d| d.run.state != before)
                && let Some(kick) = kick
            {
                kick.notify_one();
            }
            (item, run, took)
        });
    }

    /// How long the step that finished for `item` worked, read once: not the time until the loop took it in.
    pub fn took(&mut self, item: &str) -> Option<Duration> {
        self.took.remove(item)
    }

    /// The steps that ended since last asked, without waiting: each item and what it handed back,
    /// `None` when the step panicked.
    pub fn finished(&mut self) -> Vec<(String, Option<StepDone>)> {
        let mut done = Vec::new();
        while let Some(joined) = self.tasks.try_join_next() {
            if let Ok((item, run, took)) = joined {
                self.busy.remove(&item);
                self.took.insert(item.clone(), took);
                done.push((item, run));
            }
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scouts::{FileLines, plan_scouts};

    #[test]
    fn a_review_detail_counts_the_scouts_plan_scouts_plans() {
        let files = vec![
            FileLines {
                file: "a.rs".into(),
                lines: 300,
            },
            FileLines {
                file: "b.rs".into(),
                lines: 200,
            },
        ];
        let diff = |total| DiffInfo {
            stat: String::new(),
            files: files.clone(),
            backlog_changed: false,
            total_lines: total,
        };
        let small = diff(40);
        let d = StepDetail::review(&small, &plan_scouts(&small, 400, 100));
        assert_eq!((d.lines, d.scouts), (Some(40), Some(0)));
        let big = diff(500);
        let jobs = plan_scouts(&big, 400, 100);
        assert!(!jobs.is_empty());
        let d = StepDetail::review(&big, &jobs);
        assert_eq!((d.lines, d.scouts), (Some(500), Some(jobs.len() as u32)));
    }

    #[test]
    fn the_step_table_and_the_lane_holders_match_the_ts_agent() {
        use ItemState::*;
        let stepped = [
            Implementing,
            AwaitingCi,
            Checking,
            Reviewing,
            Merging,
            AwaitingMerge,
            NeedsHuman,
        ];
        let waiting = [Queued, NeedsClarification, AwaitingPlaytest, Merged, Done];
        assert!(stepped.iter().all(|&s| has_step(s)));
        assert!(waiting.iter().all(|&s| !has_step(s)));
        let in_lane: Vec<_> = stepped.into_iter().chain(waiting).filter(|&s| needs_lane(s)).collect();
        assert_eq!(in_lane, [Implementing, Checking, Reviewing]);
    }

    #[tokio::test]
    async fn a_step_is_busy_until_it_ends_and_a_panic_hands_back_none() {
        let mut running = Running::default();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let mut after = ItemRun::new();
        after.state = ItemState::Checking;
        running.spawn(
            "a".into(),
            ItemState::Checking,
            async move {
                rx.await.ok();
                StepDone::from(after)
            }
            .boxed(),
        );
        running.spawn(
            "b".into(),
            ItemState::Checking,
            async { panic!("a step that goes wrong") }.boxed(),
        );
        tokio::task::yield_now().await;
        assert!(running.busy("a"));
        tx.send(()).unwrap();
        let mut done = Vec::new();
        while done.len() < 2 {
            done.extend(running.finished());
            tokio::task::yield_now().await;
        }
        done.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(done, vec![("a".into(), Some(after.into())), ("b".into(), None)]);
        assert!(!running.busy("a") && !running.busy("b"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_step_that_moves_its_item_wakes_the_loop_and_times_only_its_own_work() {
        let kick = Arc::new(tokio::sync::Notify::new());
        let mut running = Running::default();
        running.kicked_by(kick.clone());
        let mut moved = ItemRun::new();
        moved.state = ItemState::Reviewing;
        let stays = ItemRun {
            state: ItemState::Checking,
            ..ItemRun::new()
        };
        running.spawn(
            "stays".into(),
            ItemState::Checking,
            async move { StepDone::from(stays) }.boxed(),
        );
        while running.finished().is_empty() {
            tokio::task::yield_now().await;
        }
        assert!(
            kick.notified().now_or_never().is_none(),
            "a step that got nowhere waits for the poll"
        );
        running.spawn(
            "moves".into(),
            ItemState::Checking,
            async move {
                tokio::time::sleep(Duration::from_millis(40)).await;
                StepDone::from(moved)
            }
            .boxed(),
        );
        // The paused clock moves on only while the test waits, here for the kick.
        kick.notified().await;
        while running.finished().is_empty() {
            tokio::task::yield_now().await;
        }
        assert_eq!(running.took("moves"), Some(Duration::from_millis(40)));
        assert!(running.took("moves").is_none(), "read once");
    }
}
