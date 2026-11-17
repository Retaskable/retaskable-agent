//! What a project does with the tasks the store sends (docs/specs/pools.md; the TypeScript agent's `TaskRuns` in
//! `agent/src/poolTasks.ts`): which kind each task is, which are new, which run here, and how each ends. Pure: the
//! driver (`orchestrator::driver::pool`) does the store calls and starts the work. A task is an item's whole chain on
//! this computer, so it ends when the item's chain leaves the computer's sessions (`task_state`).
use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::backlog::pool::{TaskEnd, TaskInput, Tier};
use crate::item_lifecycle::ItemState;

/// The store refused for good (the item is gone, or another computer holds it): the task is ended, not tried again
/// (`REFUSED` in poolTasks.ts).
pub const REFUSED: [&str; 3] = ["conflict", "not_found", "held_by_owner"];

/// How long a task end the store has not taken is kept before it is given up on: by then the store has long since
/// freed the task (its lease runs out after a quarter of an hour offline).
pub const OWED_KEPT_SECS: i64 = 24 * 60 * 60;

/// The task's subject prefixes for the fixes the store makes from a report (`PLAYTEST_FIX`, `SMOKE_FIX`).
const PLAYTEST_FIX: &str = "fix_playtest:";
const SMOKE_FIX: &str = "fix_smoke:";

/// What a task is, from its subject (`start` in poolTasks.ts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// An item the store already claimed for this computer: claimed by name, then the item flow.
    Item(String),
    /// `chore:specs`: a spec draft with the task's words as its request.
    Specs,
    /// A job (`task.job`): a fixed command with no lane. The kind it names.
    Job(String),
    /// Every other chore: `chore:<kind>` and the fixes. The words for the log and the end.
    Chore(String),
}

/// The kind of `task`.
pub fn classify(task: &TaskInput) -> Kind {
    if let Some(job) = &task.job {
        return Kind::Job(job.kind.clone());
    }
    let subject = task.subject.as_str();
    if subject.starts_with(PLAYTEST_FIX) {
        return Kind::Chore("fix_playtest".into());
    }
    if subject.starts_with(SMOKE_FIX) {
        return Kind::Chore("fix_smoke".into());
    }
    match subject.strip_prefix("chore:") {
        None => Kind::Item(subject.to_string()),
        Some(rest) => match rest.split(':').next().unwrap_or_default() {
            "specs" => Kind::Specs,
            other => Kind::Chore(other.to_string()),
        },
    }
}

/// What an item's state says of the task that runs it (`taskState` in poolTasks.ts): open while it is queued or in a
/// lane's step, over once it is handed on (CI, the merge, a person's play), ended not ok when it stops for a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Open,
    Done,
    Skipped,
}

/// [`TaskState`] of an item in `state`. `AwaitingPlaytest` is a hand-on, so the task is done (the TypeScript agent has
/// no such state and would call it not ok).
pub fn task_state(state: ItemState) -> TaskState {
    use ItemState::*;
    match state {
        Queued | Implementing | Checking | Reviewing => TaskState::Open,
        AwaitingCi | Merging | AwaitingMerge | AwaitingPlaytest | Merged | Done => TaskState::Done,
        NeedsClarification | NeedsHuman => TaskState::Skipped,
    }
}

/// A task running here: the item it runs and the tier the store picked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolTask {
    pub item: String,
    #[serde(default = "smart")]
    pub tier: String,
}

fn smart() -> String {
    Tier::Smart.item_tier().to_string()
}

/// A `task_end` the store has not taken, kept in the state file so a restart sends it again (the owner's outage rule:
/// finish what it is doing, take nothing new).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OwedTaskEnd {
    pub end: TaskEnd,
    /// When it was first kept, in epoch seconds.
    pub since: i64,
}

impl OwedTaskEnd {
    /// Past [`OWED_KEPT_SECS`].
    pub fn stale(&self, now_secs: i64) -> bool {
        now_secs.saturating_sub(self.since) >= OWED_KEPT_SECS
    }
}

/// The tasks of one project: every id taken or ended in this run of the agent (so one sent again is not started twice)
/// and the tasks running, with their items.
#[derive(Debug, Default)]
pub struct TaskRuns {
    seen: HashSet<String>,
    running: BTreeMap<String, PoolTask>,
    /// Job tasks under way, by task id, with the kind each runs: reported running until they end. Not kept across a
    /// restart (a job's work is lost with it; the store's lease runs out and it sends the task again).
    jobs: BTreeMap<String, String>,
}

impl TaskRuns {
    /// The tasks a restart still runs, as the state file kept them: seen already, and reported running again.
    pub fn restoring(running: BTreeMap<String, PoolTask>) -> Self {
        Self {
            seen: running.keys().cloned().collect(),
            running,
            jobs: BTreeMap::new(),
        }
    }

    /// The tasks of an answer not seen before, marked seen (`receive` in poolTasks.ts).
    pub fn fresh<'a>(&mut self, tasks: &'a [TaskInput]) -> Vec<&'a TaskInput> {
        tasks.iter().filter(|t| self.seen.insert(t.id.clone())).collect()
    }

    /// A task that could not be claimed for now is forgotten, so the store's next answer (it sends it again) retries.
    pub fn forget(&mut self, id: &str) {
        self.seen.remove(id);
        self.running.remove(id);
    }

    /// `id` runs `item` here.
    pub fn start(&mut self, id: &str, item: &str, tier: Tier) {
        let task = PoolTask {
            item: item.to_string(),
            tier: tier.item_tier().to_string(),
        };
        self.running.insert(id.to_string(), task);
    }

    /// A job task is under way here.
    pub fn start_job(&mut self, id: &str, kind: &str) {
        self.jobs.insert(id.to_string(), kind.to_string());
    }

    /// A job task ended.
    pub fn end_job(&mut self, id: &str) {
        self.jobs.remove(id);
    }

    /// The ids of every task running here, items and jobs, as `health` reports them.
    pub fn running_ids(&self) -> Vec<String> {
        self.running.keys().chain(self.jobs.keys()).cloned().collect()
    }

    /// The tasks running, by id.
    pub fn running(&self) -> &BTreeMap<String, PoolTask> {
        &self.running
    }

    /// Tasks whose item has left the computer's sessions, taken off the running list, each with its end. `state_of` says
    /// where an item is: `None` for one the loop no longer has (given back, parked, or finished without being seen
    /// handed on), which ends its task not ok.
    pub fn ended(&mut self, state_of: impl Fn(&str) -> Option<TaskState>) -> Vec<TaskEnd> {
        let mut ends = Vec::new();
        for (id, task) in &self.running {
            match state_of(&task.item) {
                Some(TaskState::Open) => {}
                Some(TaskState::Done) => ends.push(TaskEnd::done(id, true)),
                Some(TaskState::Skipped) | None => ends.push(TaskEnd::done(id, false)),
            }
        }
        for end in &ends {
            self.running.remove(&end.id);
        }
        ends
    }
}

#[cfg(test)]
mod tests;
