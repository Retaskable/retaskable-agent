//! The item flow after coding, for a leased item in `claim` mode and for a pool task (docs/specs/agent/05-item-lifecycle.md,
//! 06.3-review.md; `checkStep`, `reviewStep`, `pollCi`, `mergeStep` in Retaskable.com's `agent/src/steps.ts`,
//! `stepsCi.ts`, `review.ts`, `merge.ts`). Each state's step is a future the loop runs as the item's own task
//! ([`crate::orchestrator::steps::Running`]); it hands back the item's run as the step left it, the flow record, notes for
//! the item's history and, when the item goes back to its implementer, the message that session is resumed with.
//! The loop (`driver/flow.rs`) applies all of that and tells the store. State transitions are
//! [`crate::item_lifecycle`]'s plain code; nothing here asks a model where an item goes next.
//!
//! A step reads the lane and the code host, never the store: what the store hears is the loop's (`report`, `park`,
//! `complete`), so a synthetic chore item (no store item) takes the same steps and the loop leaves its store writes out.
use std::path::{Path, PathBuf};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;

use super::steps::{StepDone, StepRequest};
use crate::config::{CursorModels, EngineChoiceConfig, ProjectConfig};
use crate::env::Env;
use crate::item_lifecycle::ItemState;
use crate::local_model::LocalModel;
use crate::orchestrator::live::sessions::Sessions;

pub(in crate::orchestrator) mod answer;
mod checks;
#[cfg(test)]
mod checks_tests;
mod ci;
#[cfg(test)]
mod ci_tests;
pub(super) mod host;
mod review;
#[cfg(test)]
mod review_tests;
#[cfg(test)]
mod test_rig;

/// What a step needs of this computer: the project, and the `gh` program.
#[derive(Clone)]
pub(in crate::orchestrator) struct Ctx {
    pub project: ProjectConfig,
    pub gh: PathBuf,
    pub data_dir: PathBuf,
    pub choice: EngineChoiceConfig,
    pub env: Env,
    pub cursor_models: CursorModels,
    pub local: LocalModel,
    pub sessions: Sessions,
}

impl Ctx {
    /// The lane's folder.
    pub fn lane_dir(&self, lane: u32) -> PathBuf {
        crate::lanes::lane_path(Path::new(&self.project.worktrees_dir), lane)
    }
}

/// The step for `request`'s state, `None` for a state that has none built (the item waits there).
pub(super) fn advance(ctx: Ctx, request: StepRequest) -> Option<BoxFuture<'static, StepDone>> {
    match request.run.state {
        ItemState::AwaitingCi => Some(ci::step(ctx, request).boxed()),
        ItemState::Checking => Some(checks::step(ctx, request).boxed()),
        ItemState::Reviewing => Some(review::step(ctx, request).boxed()),
        _ => None,
    }
}
