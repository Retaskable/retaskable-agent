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
//! is how work the loop started comes back to it.
mod machine;
mod playtest_round;
mod project;
mod report;
mod settle;

pub use machine::{Links, Machine, Shared, SpawnArgs, spawn_all};
pub use project::{MODE_VAR, OrchestratorMode, ProjectLoop, SessionStarter, StartRequest, Step, effective_mode};
pub use settle::{OutcomeSender, SessionOutcome, SessionResult};

#[cfg(test)]
mod tests;
