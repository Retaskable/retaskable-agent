//! The `go_live_prepare` request (`prepare` in `agent/src/goLive.ts`, go-live.md section 3): Use this adds the backlog
//! item that prepares the repo for the provider chosen. Neither deploys: the provider builds from the GitHub repo
//! itself, and no token or password is asked for.
use serde_json::{Map, json};

use super::{Outcome, RunError, done};
use crate::backlog::{BacklogError, Client};
use crate::hosting::{prepare_item, project_name, provider_of};

/// Adds the item for `provider` (an id) to the project's backlog and answers `{ id, title, already }`. An unknown
/// provider is the code's own refusal, and so is the store's (its message, as the TypeScript's `HttpError`). An item
/// chosen before is on the list already, so that is `done` with `already: true` and the walkthrough carries on. A
/// store that can't be reached is a failure, not a refusal: nothing was said no to.
pub async fn prepare(
    client: &Client,
    repo_id: u64,
    token: &str,
    github_repo: &str,
    provider: Option<&str>,
) -> Result<Outcome, RunError> {
    let provider = provider_of(provider).map_err(RunError::Refused)?;
    let item = prepare_item(provider, project_name(github_repo));
    let already = match client.add_site_item(repo_id, Some(token), &item).await {
        Ok(()) => false,
        Err(e) if e.code() == Some("conflict") => true,
        Err(BacklogError::BadAnswer { message, .. }) => return Err(RunError::Refused(message)),
        Err(e) => return Err(RunError::Failed(e.to_string())),
    };
    let mut m = Map::new();
    for (key, value) in [
        ("id", json!(item.id)),
        ("title", json!(item.title)),
        ("already", json!(already)),
    ] {
        m.insert(key.into(), value);
    }
    Ok(done(m))
}
