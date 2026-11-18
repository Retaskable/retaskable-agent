//! The backlog store as a job reaches it: one project's repo id and a fresh access token for each call (a job can
//! outlast a token). `None` from [`StoreAccess::token`] is a computer not linked to the store: nothing is asked.
use std::sync::Arc;

use super::lighthouse_reports::{PutError, ReportStore};
use super::vet::Known;
use crate::backlog::access::{self, AccessTokens};
use crate::backlog::{BacklogError, Client, Draft, Item};
use crate::ideas::{IdeaState, Ideas};

#[derive(Clone)]
pub struct StoreAccess {
    pub client: Arc<Client>,
    pub repo: u64,
    pub access: Arc<dyn AccessTokens>,
}

fn unreachable(e: impl std::fmt::Display) -> BacklogError {
    BacklogError::Unreachable(e.to_string())
}

impl StoreAccess {
    async fn token(&self) -> Result<Option<String>, BacklogError> {
        access::current(&*self.access).await.map_err(unreachable)
    }

    /// The shared list's items.
    pub async fn items(&self) -> Result<Vec<Item>, BacklogError> {
        let token = self.token().await?;
        self.client.list(self.repo, token.as_deref()).await
    }

    /// The project's spec drafts.
    pub async fn drafts(&self) -> Result<Vec<Draft>, BacklogError> {
        let token = self.token().await?;
        self.client.drafts(self.repo, token.as_deref()).await
    }

    /// What the vet session must know the project has: the titles of its items and of its waiting ideas
    /// (`realDeps().known`). A store that can't be read leaves the lists empty: the session then can't call
    /// anything a duplicate, which is never wrong, only less.
    pub async fn known_titles(&self) -> Known {
        let token = match self.token().await {
            Ok(t) => t,
            Err(_) => return Known::default(),
        };
        let items = self.client.list(self.repo, token.as_deref()).await.unwrap_or_default();
        let ideas = Ideas {
            client: &self.client,
            repo_id: self.repo,
            token: token.as_deref(),
        }
        .list()
        .await
        .map(|l| l.ideas)
        .unwrap_or_default();
        Known {
            items: items.into_iter().map(|i| i.title).collect(),
            ideas: ideas
                .into_iter()
                .filter(|i| i.state == IdeaState::Waiting)
                .map(|i| i.title)
                .collect(),
        }
    }
}

impl ReportStore for StoreAccess {
    async fn put(&self, day: &str, file: &str, bytes: Vec<u8>) -> Result<String, PutError> {
        let as_put = |e: BacklogError| PutError {
            status: match &e {
                BacklogError::BadAnswer { status, .. } => Some(*status),
                BacklogError::Unreachable(_) => None,
            },
            message: e.to_string(),
        };
        let token = self.token().await.map_err(as_put)?;
        self.client
            .upload_report(self.repo, token.as_deref(), day, file, bytes)
            .await
            .map_err(as_put)
    }
}
