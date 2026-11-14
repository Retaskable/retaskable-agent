//! Take over (docs/specs/intent-rows.md section 4, `take_over_requested`): an item that needs help on another
//! computer, whose owner has been offline over 2 hours, becomes this computer's. The intent puller claims the row and
//! the store's own claim moves the item (`intents/take_over.rs`); here the loop makes the item a run of its own, in
//! `needs_human` with no lane and the lease the store gave, so Continue, Start over and Skip work on it as on any item
//! that stopped here. What the other computer knew of it (its questions and notes) isn't rebuilt yet: that waits for
//! the item's decision facts to live in the store.
//!
//! The row is looked for every minute, and never while the agent is stopping. A paused project still takes an item
//! over (nothing starts), and no session slot is used.
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;

use super::Shared;
use super::project::ProjectLoop;
use crate::backlog::take_over::PersonClaim;
use crate::backlog::{ClaimNextAgent, Client};
use crate::intents::take_over::{TakeOverHandler, TakeOvers, Taker};
use crate::intents::{IntentStore, Puller, StoreLink};
use crate::item_lifecycle::ItemState;

/// How often the store is asked for take-over rows (docs/specs/intent-rows.md section 3.3).
const POLL_EVERY: Duration = Duration::from_secs(60);

/// The real claim by name for one project and this computer.
struct StoreTaker {
    client: Arc<Client>,
    repo: u64,
    token: String,
    agent: ClaimNextAgent,
}

impl Taker for StoreTaker {
    fn take(&self, id: &str) -> BoxFuture<'static, Result<PersonClaim, String>> {
        let (client, repo, token, agent, id) = (
            self.client.clone(),
            self.repo,
            self.token.clone(),
            self.agent.clone(),
            id.to_string(),
        );
        async move { client.take_over(repo, Some(&token), &id, &agent).await }.boxed()
    }

    fn give_back(&self, id: &str, lease: &str) -> BoxFuture<'static, ()> {
        let (client, repo, token, id, lease) = (
            self.client.clone(),
            self.repo,
            self.token.clone(),
            id.to_string(),
            lease.to_string(),
        );
        async move {
            if let Err(e) = client.release(repo, Some(&token), &id, &lease).await {
                tracing::warn!("take over: giving {id} back failed: {e}");
            }
        }
        .boxed()
    }
}

/// A project's take-overs: the puller, built the first time a pass runs, and what it shares with the loop.
pub(super) struct Pulling {
    shared: Arc<TakeOvers>,
    puller: Option<Puller<Arc<dyn Taker>>>,
    polled: Option<Instant>,
    /// How long after one look at the store the next may be.
    pub(super) poll_every: Duration,
}

impl Default for Pulling {
    fn default() -> Self {
        Self {
            shared: Default::default(),
            puller: None,
            polled: None,
            poll_every: POLL_EVERY,
        }
    }
}

impl ProjectLoop {
    /// Makes each item taken over since the last pass a run of its own, then looks for new rows.
    pub(super) async fn take_over_pass(&mut self, shared: &Shared<'_>) {
        for taken in self.take_overs.shared.drain() {
            let id = taken.item.id.clone();
            if self.runs.contains_key(&id) {
                tracing::warn!("{}: took over {id}, which this computer has already", self.id);
                continue;
            }
            tracing::info!("{}: took over {id}", self.id);
            self.take_lease(&taken.item, taken.token, ItemState::NeedsHuman);
            self.remember(&id, None, None, None);
            self.save_leases();
        }
        self.take_overs.shared.set_held(self.runs.keys().cloned());
        if shared.stopping {
            return;
        }
        let Some(repo) = self.repo else { return };
        if self
            .take_overs
            .polled
            .is_some_and(|at| at.elapsed() < self.take_overs.poll_every)
        {
            return;
        }
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        self.take_overs.polled = Some(Instant::now());
        let link: Arc<dyn IntentStore> = Arc::new(StoreLink {
            client: self.client.clone(),
            repo,
            token: token.clone(),
            machine: shared.links.machine.clone(),
            failures: self.failures.clone(),
        });
        let taker: Arc<dyn Taker> = Arc::new(StoreTaker {
            client: self.client.clone(),
            repo,
            token,
            agent: ClaimNextAgent {
                machine: shared.links.machine.clone(),
                environment: shared.links.environment.clone(),
                base_branch: self.project.base_branch.clone(),
                tags: shared.links.tags.get(),
            },
        });
        let handler = Arc::new(TakeOverHandler::new(self.take_overs.shared.clone()));
        let puller = self
            .take_overs
            .puller
            .get_or_insert_with(|| Puller::default().register(handler));
        puller.pass(&link, || taker).await;
    }
}
