//! Stage 2 of review: scouts (docs/specs/agent/06.3-review.md, "Stage 2: scouts"). Cheap,
//! read-only sessions that each check one concern over their share of a slice's diff, before the
//! one expensive final reviewer (stage 3, a separate, not-yet-built backlog item) verifies what
//! they found. Retaskable.com's `agent/src/scouts.ts` (planning) and the scout half of
//! `agent/src/review.ts` (running them and formatting their findings) are the ground truth.
//!
//! Split by responsibility, each piece with its own tests: `diff` gathers per-file diff line
//! counts to plan from, `plan` is the pure bin-packing and per-kind job list, `info` looks up
//! each scout's focus and instructions (`prompts/scouts.json`), `prompt` fills `prompts/scout.md`
//! for one job, `model` holds the session's structured result, and `run` builds and runs each
//! job's session and renders its findings. Nothing calls any of it yet: picking the model tier,
//! the range for a re-review, and wiring the formatted findings into the stage 3 reviewer's
//! prompt come with the orchestrator loop and that reviewer, neither built yet.
mod diff;
mod info;
mod model;
mod plan;
mod prompt;
mod run;

pub use diff::{DiffInfo, FileLines, diff_info};
pub use info::{ScoutInfo, parse_scout_info, scout_info};
pub use model::{Finding, ScoutResult, scout_result_schema};
pub use plan::{ScoutJob, ScoutKind, pack_files, plan_scouts};
pub use prompt::{ScoutContext, build_prompt};
pub use run::{Scouts, format_scout, run_scouts, session_spec};
