//! The `draft_wanted` rows on a project's loop (docs/specs/intent-rows.md section 4; the handler is
//! `intents/draft_wanted.rs`): at most every minute the intent puller lists the project's rows and takes each one it may.
//! A paused project, a computer signed out of Claude and a computer already running two rows that start work take none:
//! the row stays open for another computer. The handler asks this loop to queue an idea's draft (`idea_accept.rs`), and
//! the loop answers on its next pass, because it alone holds the store link.
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::Shared;
use super::project::ProjectLoop;
use super::signed_out::signed_out;
use crate::intents::draft_wanted::{AcceptIdea, DraftWantedHandler};
use crate::intents::{Gates, IntentStore, Puller, StoreLink, Work};

/// How often the store is asked for these rows (docs/specs/intent-rows.md section 3.3).
const POLL_EVERY: Duration = Duration::from_secs(60);

/// A project's `draft_wanted` rows: the channel its handler writes to, the puller (built the first time a pass runs) and
/// when the store was last asked.
pub(super) struct Wanting {
    pub(super) asks: mpsc::UnboundedSender<AcceptIdea>,
    pub(super) waiting: mpsc::UnboundedReceiver<AcceptIdea>,
    puller: Option<Puller<()>>,
    polled: Option<Instant>,
}

impl Default for Wanting {
    fn default() -> Self {
        let (asks, waiting) = mpsc::unbounded_channel();
        Self {
            asks,
            waiting,
            puller: None,
            polled: None,
        }
    }
}

impl ProjectLoop {
    /// Takes the `draft_wanted` rows the store lists for this project, when nothing holds them back. `paused` is this
    /// computer's or the store's pause; `work` counts the rows running on this computer.
    pub(super) async fn draft_wanted_pass(&mut self, paused: bool, work: &Work, shared: &Shared<'_>) {
        if shared.stopping {
            return;
        }
        let Some(repo) = self.repo else { return };
        if (self.wanting.polled).is_some_and(|at| at.elapsed() < POLL_EVERY) {
            return;
        }
        let token = match shared.links.store_token().await {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        self.wanting.polled = Some(Instant::now());
        let link: std::sync::Arc<dyn IntentStore> = std::sync::Arc::new(StoreLink {
            client: self.client.clone(),
            repo,
            token,
            machine: shared.links.machine.clone(),
            failures: self.failures.clone(),
        });
        let gates = Gates {
            paused,
            signed_out: signed_out(shared),
            work: work.clone(),
        };
        let handler = std::sync::Arc::new(DraftWantedHandler::new(self.wanting.asks.clone()));
        let puller = self
            .wanting
            .puller
            .get_or_insert_with(|| Puller::default().keeping(self.store.dir()).register(handler));
        puller.pass_gated(&link, &gates, || ()).await;
    }
}
