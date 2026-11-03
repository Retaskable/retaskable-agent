//! The loop that owns the computer's orchestrator state (one shared lane pool, each project's state) and calls `orchestrator::decide` on a clock
//! (`Pool.tick` in Retaskable.com's `agent/src/pool.ts`), behind a safety switch,
//! `RETASKABLE_ORCHESTRATOR`. It wakes every `pollIntervalSec` (the TS agent's 10 second `step()`); once a
//! minute that wake is a full pass (`minute()`): a `git fetch` of each live project's repo, a read of the
//! project's items from the shared backlog, a decision (`decide` is pure) and a `tick` event saying what it
//! would do or did. In `dry` mode, or with no [`SessionStarter`], that's all: it never starts a session,
//! takes a lane, asks the store for work or writes to it. In `claim` mode with a starter, every wake also
//! takes in the sessions and steps that ended (`settle`: freeing, parking or releasing their lanes and
//! leases), and, unless paused, advances every non-terminal item one step (one at a time per item, see
//! `steps`) and, while `concurrency` session slots and a lane are free, starts queued items and asks the
//! store for more (`claim_next`). A store that can't be reached, or answers something unreadable, is a
//! `store_error` event and a skipped tick, never an empty backlog (docs/specs/agent/02-principles.md: fail closed).
//!
//! Split by responsibility: [`project`] is one project's loop (the mode switch, claiming and starting work
//! for that project alone); [`machine`] is the computer's shared state (the lane pool across every project)
//! and what starts the whole loop running; [`report`] is what the minute's fetch tells the store; [`settle`]
//! is how work the loop started comes back to it; [`playtest_round`] is a project's try-out grouping round,
//! started in the background after each full pass for the live, unpaused projects whose read succeeded and
//! whose `playtests` isn't `off`, one round at a time per project, holding no lane; [`item_commands`] carries out
//! the site's item commands (Continue) that the connector queued; [`drafts`] is a project's spec draft, one at a time,
//! claimed ahead of the queue (it still counts toward `concurrency`) and never while paused; [`asks`] answers the chat's `ask` commands the same way (no queue, counts toward `concurrency`, waits while paused, over budget or at the usage limit); [`trial`] queues a new project's trial plan after asks and drafts and before items are planned, behind the `trial-plan-draft` flag.
mod asks;
mod drafts;
mod idea_accept;
mod item_commands;
mod land;
mod lane_play;
pub use lane_play::LanePlay;
mod ideas_daily;
mod ideas_round;
mod machine;
mod playtest_round;
mod project;
mod report;
mod runs;
mod settle;
mod signed_out;
mod store_errors;
mod triage_bug;
mod triage_facts;
mod triage_run;
mod trial;

pub use asks::{AskHost, AskJob, AskReply, AskStore, WAITING};
pub use drafts::{DraftHost, DraftJob};
pub use machine::{Links, Machine, Shared, SpawnArgs, spawn_all};
use project::Launch;
pub use project::{
    Abandon, MODE_VAR, OrchestratorMode, ProjectLoop, SessionStarter, StartRequest, Step, effective_mode,
};
pub use runs::{BEAT_EVERY, RunContext, RunEnd, RunEndSender, RunLoop, RunProcesses, RunRequest, RunStarter, piece_id};
pub use settle::{OutcomeSender, SessionOutcome, SessionResult};
pub use triage_run::{LookFailure, SecondLookEnd, SecondLookRequest, TriageEnd, TriageRequest};

#[cfg(test)]
mod tests;
