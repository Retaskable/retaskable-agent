//! What the implementer's last answer said about the slice, for the steps after it (`sliceDescription`, `playtest`,
//! `itemDone` and `remaining` on the TypeScript agent's item record). The session writes it beside its transcripts when
//! it opens the change; the review reads the description and the try-out, the merge's `complete` the rest.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::implementer::ImplementerResult;
use crate::store::TryOut;

const DIR: &str = "answers";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::orchestrator) struct Answer {
    pub summary: String,
    pub slice_description: String,
    pub playtest: Option<TryOut>,
    /// What the implementer said of the whole item: built (`Some(true)`) or not, and what is left.
    pub item_done: Option<bool>,
    pub remaining: Option<String>,
}

/// Where an item's answer is kept: beside the project's state, not in the item's run folder of transcripts.
pub(in crate::orchestrator) fn path(data_dir: &Path, project: &str, item: &str) -> PathBuf {
    crate::paths::project_dir(data_dir, project)
        .join(DIR)
        .join(format!("{item}.json"))
}

/// Keeps `result` as the item's latest answer. A write that fails is logged: the steps carry on without it.
pub(in crate::orchestrator) fn save(data_dir: &Path, project: &str, item: &str, result: &ImplementerResult) {
    let answer = Answer {
        summary: result.summary.clone(),
        slice_description: result.slice_description.clone(),
        playtest: result.playtest.as_ref().map(|p| TryOut {
            what: p.what.clone(),
            steps: p.steps.clone(),
        }),
        item_done: result.item_done,
        remaining: result.remaining.clone(),
    };
    let file = path(data_dir, project, item);
    let written = std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")))
        .and_then(|()| std::fs::write(&file, serde_json::to_vec(&answer).unwrap_or_default()));
    if let Err(e) = written {
        tracing::warn!("keeping the implementer's answer in {}: {e}", file.display());
    }
}

/// The item's latest answer; none when it was never kept (the steps then go without a description).
pub(in crate::orchestrator) fn load(data_dir: &Path, project: &str, item: &str) -> Option<Answer> {
    serde_json::from_slice(&std::fs::read(path(data_dir, project, item)).ok()?).ok()
}
