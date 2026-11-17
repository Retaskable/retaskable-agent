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
//! store for its tasks (`health`, then a claim by name: `pool.rs`). A store that can't be reached, or answers something unreadable, is a
//! `store_error` event and a skipped tick, never an empty backlog (docs/specs/agent/02-principles.md: fail closed).
//!
//! Split by responsibility: [`project`] is one project's loop (the mode switch, claiming and starting work
//! for that project alone); [`machine`] is the computer's shared state (the lane pool across every project)
//! and what starts the whole loop running; [`report`] is what the minute's fetch tells the store; [`settle`]
//! is how work the loop started comes back to it; [`playtest_round`] is a project's try-out grouping round,
//! started in the background after each full pass for the live, unpaused projects whose read succeeded and
//! whose `playtests` isn't `off`, one round at a time per project, holding no lane; [`item_commands`] carries out
//! the site's item commands (Continue) that the connector queued; [`drafts`] is a project's spec draft, one at a time,
//! claimed ahead of the queue (it still counts toward `concurrency`) and never while paused; [`asks`] answers the chat's `ask` commands the same way (no queue, counts toward `concurrency`, waits while paused, over budget or at the usage limit); [`trial`] queues a new project's trial plan after asks and drafts and before items are planned.
mod asks;
mod attached;
mod decisions;
mod deprecations;
mod design_press;
mod draft_owed;
mod draft_wanted;
mod drafts;
mod experts;
mod finish_steps;
mod fix_main;
mod fix_requests;
mod flow;
mod held_items;
mod host_calls;
mod item_commands;
mod item_merge;
mod land;
mod lane_play;
mod lane_resumes;
mod main_pr;
mod main_sync;
mod merge_item;
mod outbox;
mod parking;
mod person_passes;
mod readopt;
mod reopening;
mod ship_turn;
mod take_overs;
pub use lane_play::LanePlay;
mod chores;
mod ci_coverage_look;
mod ci_pass;
mod fact_keeping;
mod machine;
mod mode;
mod panic;
mod playtest_owed;
mod playtest_round;
mod pool;
mod project;
mod read;
mod report;
mod resume;
mod revise;
mod run_starters;
mod runs;
mod settle;
mod signed_out;
mod spawn;
mod start_gate;
mod step_events;
mod store_errors;
mod sync_now;
mod triage_bug;
mod triage_facts;
mod triage_run;
mod trial;
mod usage_recheck;

pub use asks::{AskHost, AskJob, AskStore};
pub use ci_pass::CiWatchHost;
pub use drafts::{DraftHost, DraftJob};
pub use experts::{ExpertHost, ExpertJob, HelperJob, Opened, ShipJob, TurnEnd, WorkRead};
pub use machine::{Links, Machine, Shared};
use project::Launch;
pub use project::{
    Abandon, MODE_VAR, OrchestratorMode, ProjectLoop, SessionStarter, StartRequest, Step, effective_mode,
};
pub use runs::{
    BEAT_EVERY, FallOver, RunContext, RunEnd, RunEndSender, RunEventSender, RunLoop, RunProcesses, RunRequest,
    RunStarter, piece_id,
};
pub use settle::{OutcomeSender, SessionOutcome, SessionResult};
pub use spawn::{SpawnArgs, spawn_all};
pub use triage_run::{LookFailure, SecondLookEnd, SecondLookRequest, TriageEnd, TriageRequest};

#[cfg(test)]
mod tests;
