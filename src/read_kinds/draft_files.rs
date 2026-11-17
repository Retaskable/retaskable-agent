//! `draft_files { draftId }` (`draft_files` in the TS agent's `commands.ts`, `keepDraftFiles`, `branchFiles`): a plan's changed files
//! with their contents and patches, which the snapshot leaves out. The hub answers it from the store's kept fact when this computer
//! has written one (`kept_facts`, so no computer is asked), and **relays it here when the store has none yet** (`hubStoreCommands.ts`),
//! so this answers instead of leaving the page empty, in the TS agent's order:
//!
//! 1. the plan's worktree on this computer (`draft-<id>`): its files against the base branch, which are also kept as the fact now;
//! 2. the files this computer owes the store for it (listed but not yet delivered);
//! 3. a plan another computer wrote: its branch, from the store's draft row, fetched here and diffed against the base with no
//!    checkout ("That plan has no branch yet, so there are no files to show.", "No draft called <id>.").
//!
//! The answer is `{ files }`, each `{ path, isNew, content, patch?, … }`.
use serde_json::{Value, json};

use super::{ReadError, Reading};
use crate::drafts::files::{DraftFile, branch_files, draft_files as worktree_files};
use crate::{draft_worktree, lanes};

pub async fn draft_files(cx: &Reading, args: &Value) -> Result<Value, ReadError> {
    let id = args
        .get("draftId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
        return Err(ReadError::Refused("draft_files needs the draft's id.".into()));
    }
    let (repo, worktrees, base) = (
        std::path::PathBuf::from(&cx.project.repo_path),
        std::path::PathBuf::from(&cx.project.worktrees_dir),
        cx.project.base_branch.clone(),
    );
    let dir = draft_worktree::path(&worktrees, id);
    if lanes::is_worktree(&dir) {
        let files = blocking(move || worktree_files(&dir, &base)).await?;
        if let Some(facts) = &cx.facts {
            facts.keep_drafted_files(id, &files);
        }
        return Ok(json!({ "files": files }));
    }
    if let Some(owed) = cx
        .facts
        .as_ref()
        .and_then(|f| f.owed(&format!("draft_files/{id}")))
        .filter(Value::is_array)
    {
        return Ok(json!({ "files": owed }));
    }
    let drafts = cx
        .client
        .drafts(cx.repo, Some(&cx.token))
        .await
        .map_err(|e| ReadError::Failed(format!("Couldn't read the plans from the shared backlog: {e}")))?;
    let Some(draft) = drafts.into_iter().find(|d| d.id == id) else {
        return Err(ReadError::Refused(format!("No draft called {id}.")));
    };
    let Some(branch) = draft.branch.filter(|b| !b.is_empty()) else {
        return Err(ReadError::Refused(
            "That plan has no branch yet, so there are no files to show.".into(),
        ));
    };
    let files = blocking(move || branch_files(&repo, &base, &branch)).await?;
    Ok(json!({ "files": files }))
}

/// Git waits, so it runs on a blocking thread; what broke is the reason in words.
async fn blocking(
    read: impl FnOnce() -> anyhow::Result<Vec<DraftFile>> + Send + 'static,
) -> Result<Vec<DraftFile>, ReadError> {
    match tokio::task::spawn_blocking(read).await {
        Ok(Ok(files)) => Ok(files),
        Ok(Err(e)) => Err(ReadError::Failed(format!("{e:#}"))),
        Err(e) => Err(ReadError::Failed(e.to_string())),
    }
}

#[cfg(test)]
mod tests;
