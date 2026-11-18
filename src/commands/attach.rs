//! What `continue`, `retry` and `playtest` carry besides the item: `notes`, and `pictureIds` (up to four ids the store
//! gave the pictures; the TS agent's `withPictures` in `commands.ts` fetches them into its cache and adds their paths
//! after the notes). The ids are checked here; the loop, which holds the store link, fetches them
//! (`orchestrator::driver::attached`).
use serde_json::Value;

use super::Command;
use crate::view::Attached;

/// The notes, trimmed; `None` when blank (`notes()` in the TS agent's `repoFiles.ts`).
pub(super) fn notes_of(args: &Value) -> Option<String> {
    let notes = args.get("notes").and_then(Value::as_str).unwrap_or("").trim();
    (!notes.is_empty()).then(|| notes.to_string())
}

/// The command's `pictureIds`, checked, for the loop to fetch; `None` when it has none. An id that isn't of the shape
/// the store gives is refused.
pub(super) fn attached(cmd: &Command) -> Result<Option<Attached>, String> {
    let ids = super::expert::picture_ids(cmd)?;
    Ok((!ids.is_empty()).then(|| Attached {
        command_id: cmd.id.clone(),
        ids,
    }))
}
