//! The agent's own cloud, for what only it may ask of retaskable.com (`cloudLink` in the TS agent's `commands.ts`): its address
//! and an access token, from the link of the first cloud this agent connects to (the primary, `clouds.primary()`). A command
//! that arrives over another cloud's connection never gets it.
use std::sync::{Arc, OnceLock};

use futures_util::future::BoxFuture;

use crate::link::{DeviceTokens, TokenAnswer};

/// A cloud's address and the access token for it.
pub trait CloudToken: Send + Sync {
    fn url(&self) -> &str;
    /// A token in hand, else a fresh one; `Idle` when this agent isn't linked, `Unreachable` when the cloud can't be asked.
    fn answer(&self) -> BoxFuture<'_, TokenAnswer>;
}

impl CloudToken for DeviceTokens {
    fn url(&self) -> &str {
        DeviceTokens::url(self)
    }

    fn answer(&self) -> BoxFuture<'_, TokenAnswer> {
        Box::pin(self.access_token(None))
    }
}

/// The primary cloud's link, filled in once by the first connector.
#[derive(Clone, Default)]
pub struct CloudLinkSlot(Arc<OnceLock<Arc<dyn CloudToken>>>);

impl CloudLinkSlot {
    /// Takes `cloud` as the primary if none is yet: the first cloud the agent connects to.
    pub fn set_primary(&self, cloud: Arc<dyn CloudToken>) {
        let _ = self.0.set(cloud);
    }

    pub fn get(&self) -> Option<Arc<dyn CloudToken>> {
        self.0.get().cloned()
    }

    /// Whether `url` is the primary cloud's.
    pub fn is_primary(&self, url: &str) -> bool {
        self.0.get().is_some_and(|c| c.url() == url)
    }
}

impl std::fmt::Debug for CloudLinkSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CloudLinkSlot")
    }
}

/// A queued command carries the link it was given: two are the same when they are the same one.
#[derive(Clone)]
pub struct Link(pub Arc<dyn CloudToken>);

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Link")
    }
}

impl PartialEq for Link {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Link {}
