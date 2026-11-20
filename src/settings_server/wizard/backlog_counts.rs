//! What a repo's backlog already holds, for the Projects and Done steps (docs/specs/setup-wizard.md section 3.9). The
//! store's backlog is keyed by the repo's GitHub id, so linking a repo that was worked on before brings its old queue back;
//! the page says so before and after the link and says what Resume starts. The agent only asks the store's `list` and
//! counts by state: the store decides what runs.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backlog::{Client, Item, ItemStatus};
use crate::link::TokenAnswer;
use crate::page_host::{bad_request, unauthorized};
use crate::settings_server::SettingsServer;
use crate::view::CloudToken;

/// The items of a backlog still to do, by what they are doing now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// `claimed`, `building`, `in_review` or `merging`.
    pub in_progress: usize,
    /// `held` or `todo`.
    pub waiting: usize,
    /// `needs_person`.
    pub needs_you: usize,
}

impl Counts {
    pub fn total(&self) -> usize {
        self.in_progress + self.waiting + self.needs_you
    }

    pub fn to_json(self, repo: &str) -> Value {
        json!({
            "repo": repo, "inProgress": self.in_progress, "waiting": self.waiting,
            "needsYou": self.needs_you, "total": self.total(),
        })
    }
}

/// Counts the items by state; built, in-production and skipped items are not counted.
pub fn count(items: &[Item]) -> Counts {
    let mut c = Counts::default();
    for i in items {
        match i.status {
            ItemStatus::Held | ItemStatus::Todo => c.waiting += 1,
            ItemStatus::Claimed | ItemStatus::Building | ItemStatus::InReview | ItemStatus::Merging => {
                c.in_progress += 1;
            }
            ItemStatus::NeedsPerson => c.needs_you += 1,
            ItemStatus::OnBranch | ItemStatus::OnMain | ItemStatus::InProduction | ItemStatus::Skipped => {}
        }
    }
    c
}

/// Where the counts come from: the store through the cloud link, or a made-up one in tests.
pub trait CountSource: Send + Sync {
    fn counts(&self, repo: String) -> BoxFuture<'static, Result<Counts, String>>;
}

/// The real source: the repo's id from `gh`, then the store's `list` with the link's access token.
pub struct StoreCounts {
    pub link: Arc<dyn CloudToken>,
}

impl CountSource for StoreCounts {
    fn counts(&self, repo: String) -> BoxFuture<'static, Result<Counts, String>> {
        let link = self.link.clone();
        Box::pin(async move {
            let (url, token) = match link.answer().await {
                TokenAnswer::Token { url, access_token } => (url, access_token),
                TokenAnswer::Idle => return Err("Link this agent to retaskable.com first.".into()),
                TokenAnswer::Unreachable => return Err("Couldn't reach retaskable.com: try again in a minute.".into()),
            };
            let id = crate::repo_id::lookup(&repo).await.map_err(|u| u.why)?;
            let items = Client::new(url.trim_end_matches('/').to_string())
                .list(id, Some(&token))
                .await
                .map_err(|e| format!("Couldn't read the backlog of {repo}: {e}"))?;
            Ok(count(&items))
        })
    }
}

#[derive(Deserialize)]
pub(in crate::settings_server) struct RepoAsk {
    #[serde(default)]
    repo: String,
}

impl SettingsServer {
    /// Replaces where the counts come from (tests).
    pub fn set_wizard_counts(&self, source: Arc<dyn CountSource>) {
        let _ = self.wizard.counts.set(source);
    }

    fn count_source(&self) -> Option<Arc<dyn CountSource>> {
        if let Some(s) = self.wizard.counts.get() {
            return Some(s.clone());
        }
        let link = self.agent_view()?.item_commands.cloud.get()?;
        Some(Arc::new(StoreCounts { link }))
    }
}

/// `GET /api/wizard/backlog?repo=owner/name`: what that repo's backlog already holds, or why it can't be told.
pub(in crate::settings_server) async fn get_backlog(
    State(settings): State<Arc<SettingsServer>>,
    ConnectInfo(_peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(ask): Query<RepoAsk>,
) -> Response {
    if settings.authenticate(&headers).is_none() {
        return unauthorized("Sign in first.");
    }
    let repo = ask.repo.trim().to_string();
    if crate::add_project::parse_repo(&repo).is_none() {
        return bad_request("Name a repo as owner/name.");
    }
    let answer = match settings.count_source() {
        Some(source) => match source.counts(repo.clone()).await {
            Ok(c) => c.to_json(&repo),
            Err(why) => json!({ "repo": repo, "error": why }),
        },
        None => json!({ "repo": repo, "error": "Link this agent to retaskable.com first." }),
    };
    Json(answer).into_response()
}
