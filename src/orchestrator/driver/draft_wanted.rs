//! A person's Add spec and Build this on a project's loop (docs/specs/intent-rows.md section 4, `draft_wanted`; the
//! handler is `intents/draft_wanted.rs`): at most every minute the intent puller lists the project's `draft_wanted`
//! rows and takes each one, in `claim` and in `runs` mode alike. A paused project, a computer signed out of Claude and a
//! computer already running two rows that start work take none: the row stays open for another computer. No session
//! slot is used here; the draft is queued in the store, and a drafter claims it from there.
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::project::ProjectLoop;
use super::signed_out::signed_out;
use super::{Links, Shared};
use crate::ask::pictures;
use crate::intents::draft_wanted::{WantedCx, WantedHandler};
use crate::intents::{Gates, IntentStore, Puller, StoreLink, Work};

/// How often the store is asked for them (docs/specs/intent-rows.md section 3.3).
const POLL_EVERY: Duration = Duration::from_secs(60);

/// A project's `draft_wanted` rows: the puller (built the first time a pass runs) and when the store was last asked.
pub(super) struct WantedRows {
    puller: Option<Puller<WantedCx>>,
    polled: Option<Instant>,
    /// How long after one look at the store the next may be.
    pub(super) poll_every: Duration,
}

impl Default for WantedRows {
    fn default() -> Self {
        Self {
            puller: None,
            polled: None,
            poll_every: POLL_EVERY,
        }
    }
}

impl ProjectLoop {
    /// Takes the `draft_wanted` rows the store lists for this project that nothing holds back. `paused` is this
    /// computer's or the store's pause; `work` counts the rows that start work running on this computer.
    pub(super) async fn wanted_pass(
        &mut self,
        paused: bool,
        work: &Work,
        links: &Links,
        stopping: bool,
        shared: &Shared<'_>,
    ) {
        if stopping {
            return;
        }
        let Some(repo) = self.repo else { return };
        if self
            .wanted
            .polled
            .is_some_and(|at| at.elapsed() < self.wanted.poll_every)
        {
            return;
        }
        let token = match links.store_token().await {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        self.wanted.polled = Some(Instant::now());
        let link: Arc<dyn IntentStore> = Arc::new(StoreLink {
            client: self.client.clone(),
            repo,
            token: token.clone(),
            machine: links.machine.clone(),
            failures: self.failures.clone(),
        });
        let gates = Gates {
            paused,
            signed_out: signed_out(shared),
            work: work.clone(),
        };
        let puller = (self.wanted.puller).get_or_insert_with(|| {
            Puller::default()
                .keeping(self.store.dir())
                .register(Arc::new(WantedHandler))
        });
        let client = self.client.clone();
        let cx = move || WantedCx {
            client,
            repo,
            token,
            pictures: pictures::folder(),
        };
        puller.pass_gated(&link, &gates, cx).await;
    }
}
