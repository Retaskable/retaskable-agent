//! The saved release notes on the shared backlog (Retaskable.com's last-local-state.md section 3.5,
//! `agent/src/storeReleases.ts`): the store keeps them, one `releases` row a note, for every computer
//! and the site. This computer only reads them (`releases_get`): it offers the store nothing, reads no
//! `docs/releases` folder and keeps no file of its own. Saving a note (`release_set`) is the site's,
//! as a writer.
use serde::Deserialize;
use serde_json::json;

use crate::backlog::{BacklogError, Client};

/// One saved note as the store answers it: `releases_get` lists them without `body`, newest first;
/// with an `id` it answers that one with its Markdown.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreRelease {
    pub id: String,
    pub title: String,
    /// The range's days (`2026-09-23`), as the note's front matter says.
    pub from: String,
    pub to: String,
    /// The Markdown, front matter included; only when one note is asked for.
    #[serde(default)]
    pub body: Option<String>,
    /// When it was saved, in ms.
    #[serde(default)]
    pub created_at: u64,
    /// The login whose write it was.
    #[serde(default)]
    pub by: String,
    /// The computer that wrote it, when an agent did.
    #[serde(default)]
    pub machine: Option<String>,
}

/// One project's saved notes in the store (`StoreReleases`).
pub struct SavedReleases<'a> {
    pub client: &'a Client,
    pub repo_id: u64,
    pub token: Option<&'a str>,
}

impl SavedReleases<'_> {
    /// The saved notes without their Markdown, newest first, or `None` when the store can't give them
    /// right now (never a guessed or empty list: the store is the only place they're kept).
    pub async fn list(&self) -> Option<Vec<StoreRelease>> {
        match self
            .client
            .op(self.repo_id, self.token, "releases_get", json!({}), "saved notes")
            .await
        {
            Ok(rows) => Some(rows),
            Err(e) => {
                tracing::warn!("releases: couldn't read the saved release notes from the shared backlog: {e}");
                None
            }
        }
    }

    /// One saved note with its Markdown. A note the store hasn't got is its `not_found` refusal.
    pub async fn get(&self, id: &str) -> Result<StoreRelease, BacklogError> {
        self.client
            .op(
                self.repo_id,
                self.token,
                "releases_get",
                json!({ "id": id }),
                "saved note",
            )
            .await
    }
}

#[cfg(test)]
#[path = "saved_tests.rs"]
mod tests;
