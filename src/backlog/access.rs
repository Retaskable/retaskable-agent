//! The bearer every store call carries: a short-lived access token, never the stored refresh token (link.ts
//! `DeviceTokens`, storeLink.ts, backlogStore.ts `send`; the store verifies a signed access token and knows a refresh
//! token only at `/v1/device/token`). `DeviceTokens` swaps the refresh token for one and keeps it until a minute before
//! it runs out. `Client` asks again once, with a fresh one, when a call is refused (`Client::with_access`).
use std::sync::atomic::Ordering;

use anyhow::{Result, anyhow};
use futures_util::future::BoxFuture;

use crate::link::{DeviceTokens, TokenAnswer};

/// Where a store call's access token comes from.
pub trait AccessTokens: Send + Sync {
    /// A token with time left, or a new one when `stale` (the one the store just refused) is what's in hand.
    fn access_token<'a>(&'a self, stale: Option<&'a str>) -> BoxFuture<'a, TokenAnswer>;

    /// Whether this computer holds a link to the store.
    fn linked(&self) -> bool;
}

impl AccessTokens for DeviceTokens {
    fn access_token<'a>(&'a self, stale: Option<&'a str>) -> BoxFuture<'a, TokenAnswer> {
        Box::pin(async move {
            let answer = DeviceTokens::access_token(self, stale).await;
            if answer == TokenAnswer::Idle && self.unlinked_on_site() {
                // The store refused the refresh token (unlinked on the website, or reused): fail closed until the
                // person links again, and don't start a new link by itself meanwhile (`backlog::auto_link`).
                super::UNLINKED_ON_SITE.store(true, Ordering::SeqCst);
            }
            answer
        })
    }

    fn linked(&self) -> bool {
        DeviceTokens::linked(self)
    }
}

/// The access token to send now: `None` when this computer isn't linked, an error when the store couldn't be reached
/// to get one (a call made without it would only be refused).
pub async fn current(access: &dyn AccessTokens) -> Result<Option<String>> {
    match access.access_token(None).await {
        TokenAnswer::Token { access_token, .. } => Ok(Some(access_token)),
        TokenAnswer::Idle => Ok(None),
        TokenAnswer::Unreachable => Err(anyhow!("the backlog store couldn't be reached to get an access token")),
    }
}

/// The stored token handed out as the access token, for tests whose fake store takes any bearer.
#[cfg(test)]
pub(crate) struct StoredAccess(pub std::sync::Arc<dyn crate::link::TokenStore>, pub String);

#[cfg(test)]
impl AccessTokens for StoredAccess {
    fn access_token<'a>(&'a self, _stale: Option<&'a str>) -> BoxFuture<'a, TokenAnswer> {
        Box::pin(async move {
            match self.0.get(&self.1) {
                Ok(Some(access_token)) => TokenAnswer::Token {
                    url: self.1.clone(),
                    access_token,
                },
                _ => TokenAnswer::Idle,
            }
        })
    }

    fn linked(&self) -> bool {
        matches!(self.0.get(&self.1), Ok(Some(_)))
    }
}

#[cfg(test)]
mod tests;
