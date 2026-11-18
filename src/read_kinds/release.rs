//! `release_status { part, id? }` (`agent/src/commands.ts`, `releaseNotes.ts`): the Releases tab's reads of the shared
//! backlog's saved notes. `last` is the last release (the newest saved note, else the host's newest release, else the
//! newest tag), `saved` the list of saved notes and `note` one note's Markdown. A store that can't be reached is the
//! reason in words, never a guessed list. Asked as the command the hub relays when no `release_last` fact answers it.
use serde_json::{Value, json};

use super::{ReadError, Reading};
use crate::kept_facts::last;
use crate::release_notes::SavedReleases;

/// What the reader is told when the store gave nothing.
const NOT_REACHED: &str = "Couldn't reach the shared backlog just now. Try again in a moment.";

pub async fn release_status(cx: &Reading, args: &Value) -> Result<Value, ReadError> {
    let saved = SavedReleases {
        client: &cx.client,
        repo_id: cx.repo,
        token: Some(&cx.token),
    };
    match args.get("part").and_then(Value::as_str) {
        Some("note") => {
            let Some(id) = args.get("id").and_then(Value::as_str) else {
                return Err(ReadError::Refused("That note isn't in a shared backlog.".into()));
            };
            match saved.get(id).await {
                Ok(note) => Ok(json!({ "markdown": note.body.unwrap_or_default() })),
                Err(e) => {
                    let why = format!("Couldn't read {id}: {e}");
                    Err(if e.code() == Some("not_found") {
                        ReadError::Refused(why)
                    } else {
                        ReadError::Failed(why)
                    })
                }
            }
        }
        Some("last") => {
            let repo = std::path::Path::new(&cx.project.repo_path);
            match last::find(&saved, &cx.gh, repo, &cx.project.github_repo, &cx.project.base_branch).await {
                Some(found) => Ok(json!({ "last": found })),
                None => Err(ReadError::Refused(NOT_REACHED.into())),
            }
        }
        Some("saved") => match saved.list().await {
            Some(rows) => Ok(Value::Array(
                rows.iter()
                    .map(|r| json!({ "path": r.id, "from": r.from, "to": r.to, "stored": true }))
                    .collect(),
            )),
            None => Err(ReadError::Refused(NOT_REACHED.into())),
        },
        _ => Err(ReadError::Refused(
            "release_status takes a part: last, saved or note.".into(),
        )),
    }
}

#[cfg(test)]
mod tests;
