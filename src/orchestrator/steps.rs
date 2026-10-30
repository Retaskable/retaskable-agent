//! Advancing every non-terminal item one step (docs/specs/agent/07-orchestrator-loop.md: "advance every
//! non-terminal item one step (non-blocking; sessions are child processes)"; `Orchestrator.advance` and
//! `runStep` in Retaskable.com's `agent/src/orchestrator.ts`). The loop only schedules: each step runs as its
//! own task, at most one per item at a time, and hands back the item's run as the step left it. What a
//! step does (an implementer session, checks, a review, a merge) is the [`SessionStarter`]'s.
//!
//! [`SessionStarter`]: super::driver::SessionStarter
use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use tokio::task::JoinSet;

use crate::item_lifecycle::{ItemRun, ItemState};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRequest {
    pub project: String,
    pub item_id: String,
    pub run: ItemRun,
    pub lane: Option<u32>,
    pub token: String,
}

/// One project's steps still running, by item.
#[derive(Default)]
pub struct Running {
    busy: BTreeSet<String>,
    tasks: JoinSet<(String, Option<ItemRun>)>,
}

impl Running {
    /// Whether a step for `item` is still running.
    pub fn busy(&self, item: &str) -> bool {
        self.busy.contains(item)
    }

    /// Runs `step` for `item` as its own task. A panic in it is caught: the item keeps the run it had.
    pub fn spawn(&mut self, item: String, step: BoxFuture<'static, ItemRun>) {
        self.busy.insert(item.clone());
        self.tasks.spawn(async move {
            let run = AssertUnwindSafe(step).catch_unwind().await.ok();
            (item, run)
        });
    }

    /// The steps that ended since last asked, without waiting: each item and its run after the step,
    /// `None` when the step panicked.
    pub fn finished(&mut self) -> Vec<(String, Option<ItemRun>)> {
        let mut done = Vec::new();
        while let Some(joined) = self.tasks.try_join_next() {
            if let Ok((item, run)) = joined {
                self.busy.remove(&item);
                done.push((item, run));
            }
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            async move {
                rx.await.ok();
                after
            }
            .boxed(),
        );
        running.spawn("b".into(), async { panic!("a step that goes wrong") }.boxed());
        tokio::task::yield_now().await;
        assert!(running.busy("a"));
        tx.send(()).unwrap();
        let mut done = Vec::new();
        while done.len() < 2 {
            done.extend(running.finished());
            tokio::task::yield_now().await;
        }
        done.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(done, vec![("a".into(), Some(after)), ("b".into(), None)]);
        assert!(!running.busy("a") && !running.busy("b"));
    }
}
