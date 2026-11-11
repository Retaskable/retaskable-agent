//! One project's backlog read of a pass, and its `settings_get`.
use std::sync::Arc;

use super::machine::repo_id;
use crate::backlog::asks::ProjectFacts;
use crate::backlog::settings::StoreSettings;
use crate::backlog::{BacklogError, Client, Item};
use crate::link::TokenStore;

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
    pub(super) settings: Option<Result<Option<StoreSettings>, BacklogError>>,
    /// The project's facts (its pause), read only with `obey-shared-pause` On and the items read.
    pub(super) facts: Option<Result<ProjectFacts, BacklogError>>,
}

/// Reads one project's items. Owns what it needs, so it runs as its own task: a panic in it is that
/// project's failed read and nothing else's.
#[allow(clippy::too_many_arguments)]
pub(super) async fn read(
    client: Arc<Client>,
    id: String,
    github_repo: String,
    cached: Option<u64>,
    backlog_url: String,
    keychain: Arc<dyn TokenStore>,
    want_facts: bool,
) -> Read {
    let repo = match cached {
        Some(repo) => Some(repo),
        None => repo_id(&github_repo).await,
    };
    let Some(repo_id) = repo else {
        tracing::warn!("{id}: dry tick skipped: gh couldn't say the id of {github_repo}");
        let items = Err(Fail::Cant(format!("gh couldn't say the id of {github_repo}")));
        return Read {
            repo,
            items,
            settings: None,
            facts: None,
        };
    };
    let token = match keychain.get(&backlog_url) {
        Ok(token) => token,
        Err(e) => {
            tracing::warn!("{id}: dry tick skipped: reading the backlog link: {e:#}");
            let items = Err(Fail::Cant(format!("reading the backlog link: {e:#}")));
            return Read {
                repo,
                items,
                settings: None,
                facts: None,
            };
        }
    };
    let items = match token {
        None => Err(Fail::Store(BacklogError::BadAnswer {
            status: 401,
            code: Some("unauthorized".into()),
            message: "this computer holds no link to the backlog store".into(),
        })),
        Some(ref token) => client.list(repo_id, Some(token)).await.map_err(Fail::Store),
    };
    let settings = match &token {
        Some(token) => Some(client.settings_get(repo_id, Some(token)).await),
        None => None,
    };
    let facts = match &token {
        Some(token) if want_facts && items.is_ok() => Some(client.project_facts(repo_id, Some(token)).await),
        _ => None,
    };
    Read {
        repo,
        items,
        settings,
        facts,
    }
}
