//! One project's backlog read of a pass, and its `settings_get`.
use std::sync::Arc;

use crate::backlog::access::{self, AccessTokens};
use crate::backlog::facts::ProjectFacts;
use crate::backlog::shown::Shown;
use crate::backlog::{BacklogError, Client, Item};
use crate::repo_id::lookup;

/// Why a project's items weren't read this pass.
pub(super) enum Fail {
    /// Before the store could be asked (no repo id, no link): the page hears of it, no `store_error` event.
    Cant(String),
    Store(BacklogError),
}

/// A project's backlog read: the repo id it learned and the items or why not.
pub(super) struct Read {
    pub(super) repo: Option<u64>,
    pub(super) items: Result<Vec<Item>, Fail>,
    /// The project's settings, read every pass; an error keeps what runs.
    pub(super) settings: Option<Result<crate::backlog::settings::SettingsAnswer, BacklogError>>,
    /// The project's facts (its pause), read once the items are read.
    pub(super) facts: Option<Result<ProjectFacts, BacklogError>>,
    /// The store's smoke and vet facts, read once the items are read; an error keeps what was shown.
    pub(super) shown: Option<Result<Shown, BacklogError>>,
}

/// Reads one project's items. Owns what it needs, so it runs as its own task: a panic in it is that
/// project's failed read and nothing else's.
#[allow(clippy::too_many_arguments)]
pub(super) async fn read(
    client: Arc<Client>,
    id: String,
    github_repo: String,
    cached: Option<u64>,
    access: Arc<dyn AccessTokens>,
) -> Read {
    let found = match cached {
        Some(repo) => Ok(repo),
        None => lookup(&github_repo).await,
    };
    let repo_id = match found {
        Ok(repo_id) => repo_id,
        Err(unknown) => {
            let why = unknown.why;
            tracing::warn!("{id}: dry tick skipped: {why}");
            let items = Err(Fail::Cant(why));
            return Read {
                repo: None,
                items,
                settings: None,
                facts: None,
                shown: None,
            };
        }
    };
    let repo = Some(repo_id);
    let token = match access::current(&*access).await {
        Ok(token) => token,
        Err(e) => {
            tracing::warn!("{id}: dry tick skipped: reading the backlog link: {e:#}");
            let items = Err(Fail::Cant(format!("reading the backlog link: {e:#}")));
            return Read {
                repo,
                items,
                settings: None,
                facts: None,
                shown: None,
            };
        }
    };
    let items = match token {
        None => Err(Fail::Store(BacklogError::BadAnswer {
            status: 401,
            code: Some("unauthorized".into()),
            message: "this computer holds no link to the backlog store".into(),
        })),
        Some(ref token) => (client.list(repo_id, Some(token)).await)
            .map(|mut items| {
                // Expert sessions are the experts module's, never planned. One whose change is open goes on as any item
                // does, so the loop that holds its lease sees it again after a restart.
                items.retain(|i| !crate::expert::held_by_experts(&i.id, i.kind.as_deref(), i.detail.as_deref()));
                items
            })
            .map_err(Fail::Store),
    };
    let settings = match &token {
        Some(token) => Some(client.settings_get(repo_id, Some(token)).await),
        None => None,
    };
    let facts = match &token {
        Some(token) if items.is_ok() => Some(client.project_facts(repo_id, Some(token)).await),
        _ => None,
    };
    let shown = match &token {
        Some(token) if items.is_ok() => Some(client.shown(repo_id, Some(token)).await),
        _ => None,
    };
    Read {
        repo,
        items,
        settings,
        facts,
        shown,
    }
}
