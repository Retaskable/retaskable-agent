//! Sync now (docs/specs/intent-rows.md section 4, `sync_requested`; `syncNow` in the TS agent's `teamBranchActions.ts`):
//! a person presses it on a project working on a team branch and the main sync starts at once. The intent puller claims
//! the row and the handler asks this loop (`intents/sync.rs`); the loop answers on its next pass, because it alone owns
//! the sync (`main_sync.rs`, `start_sync_now`). Like the TS agent it asks the store to make the chore (`behind_main`, a person's
//! ask, even with `syncMain` off); on the integration-branch pattern it rebases at once instead, here. A paused project
//! still syncs (the person asked). Runs mode only, where a main sync exists.
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::Shared;
use super::project::ProjectLoop;
use crate::intents::sync::{SyncHandler, SyncNow};
use crate::intents::{IntentStore, Puller, StoreLink};

/// How often the store is asked for sync rows (docs/specs/intent-rows.md section 3.3).
const POLL_EVERY: Duration = Duration::from_secs(60);

/// A project's Sync now requests: the channel its handler writes to, the puller (built the first time a pass runs)
/// and when the store was last asked.
pub(super) struct Syncing {
    asks: mpsc::UnboundedSender<SyncNow>,
    waiting: mpsc::UnboundedReceiver<SyncNow>,
    puller: Option<Puller<()>>,
    polled: Option<Instant>,
    /// How long after one look at the store the next may be.
    pub(super) poll_every: Duration,
}

impl Default for Syncing {
    fn default() -> Self {
        let (asks, waiting) = mpsc::unbounded_channel();
        Self {
            asks,
            waiting,
            puller: None,
            polled: None,
            poll_every: POLL_EVERY,
        }
    }
}

impl ProjectLoop {
    /// Answers the Sync now requests the puller handed over, then looks for new rows. `stopping` is the computer
    /// going down, not a pause.
    pub(super) async fn sync_now_pass(&mut self, shared: &Shared<'_>) {
        if shared.stopping {
            return;
        }
        let Some(repo) = self.repo else { return };
        let token = match shared.links.store_token().await {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        while let Ok(ask) = self.syncing.waiting.try_recv() {
            let answer = self.start_sync_now(repo, Some(&token), &shared.links.machine).await;
            if let Ok(id) = &answer {
                tracing::info!("{}: a sync of main started by hand: {id}", self.id);
            }
            let _ = ask.reply.send(answer);
        }
        if (self.syncing.polled).is_some_and(|at| at.elapsed() < self.syncing.poll_every) {
            return;
        }
        self.syncing.polled = Some(Instant::now());
        let link: std::sync::Arc<dyn IntentStore> = std::sync::Arc::new(StoreLink {
            client: self.client.clone(),
            repo,
            token,
            machine: shared.links.machine.clone(),
            failures: self.failures.clone(),
        });
        let handler = std::sync::Arc::new(SyncHandler::new(self.syncing.asks.clone()));
        let puller =
            (self.syncing.puller).get_or_insert_with(|| Puller::default().keeping(self.store.dir()).register(handler));
        puller.pass(&link, || ()).await;
    }
}
