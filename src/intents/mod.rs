//! The intent puller (docs/specs/intent-rows.md section 3.3; `intentPuller.ts` in Retaskable.com's agent). The website
//! writes what a person wants as a row in the store; this computer lists the open rows, takes the ones it has a
//! handler for and can run now, keeps its claim fresh while the handler runs, and ends the row as `done` with the
//! handler's derived result, `refused` with its reason in plain words, or `failed`.
//!
//! Nothing is kept on disk. After a restart the rows are listed again: one this computer held is `claimed` under its own
//! agent id, and claiming it again keeps the claim (the store lets a claim go after a few minutes without a beat).
//!
//! A kind is added by registering one [`Handler`]; the puller itself knows nothing of any kind.
pub mod finish_steps;
pub mod take_over;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::backlog::failures::Failures;
use crate::backlog::intents::{Claim, Ending, IntentRow};
use crate::backlog::{BacklogError, Client};

/// How often a running row tells the store it is alive (the store lets an `ask` claim go after 3 minutes without).
pub const BEAT_EVERY: Duration = Duration::from_secs(60);
/// How long to wait before sending a finish the store didn't take again (three tries).
const FINISH_RETRY: Duration = Duration::from_secs(2);

/// How a handler ended a row it ran.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// `done`, with the derived result (at most 4 KB, never a secret).
    Done(Value),
    /// `refused`: the computer understood and said no, in plain words (at most 280 characters).
    Refused(String),
    /// `failed`: it broke or stopped, in plain words.
    Failed(String),
    /// The computer was cut (a restart) before it finished: no word to the store, which opens the row again once the
    /// claim goes stale.
    Abandoned,
}

/// What this computer does for one kind of row. `C` is whatever the loop knows at the moment it takes a row.
pub trait Handler<C>: Send + Sync {
    /// The row kind it runs (`ask`).
    fn kind(&self) -> &'static str;
    /// Whether it runs this row of its kind (an `ask` that is a code question); a row it doesn't take is left for
    /// another computer or another handler.
    fn takes(&self, _row: &IntentRow) -> bool {
        true
    }
    /// Runs the claimed row.
    fn run(&self, row: IntentRow, cx: &C) -> BoxFuture<'static, Outcome>;
}

/// The store's intent ops for one project and this computer. Faked in tests.
pub trait IntentStore: Send + Sync {
    fn list(&self, kinds: &[&str]) -> BoxFuture<'static, Result<Vec<IntentRow>, BacklogError>>;
    fn claim(&self, id: &str) -> BoxFuture<'static, Result<Claim, BacklogError>>;
    fn beat(&self, id: &str) -> BoxFuture<'static, Result<bool, BacklogError>>;
    fn finish(
        &self,
        id: &str,
        ending: Ending,
        result: Option<Value>,
        why: Option<String>,
    ) -> BoxFuture<'static, Result<bool, BacklogError>>;
}

/// The real store: one project's repo, this machine's token and name.
#[derive(Clone)]
pub struct StoreLink {
    pub client: Arc<Client>,
    pub repo: u64,
    pub token: String,
    /// What the store shows as who holds a claim.
    pub machine: String,
    /// Where a finish the store refuses is kept for the loop to log.
    pub failures: Failures,
}

impl IntentStore for StoreLink {
    fn list(&self, kinds: &[&str]) -> BoxFuture<'static, Result<Vec<IntentRow>, BacklogError>> {
        let me = self.clone();
        let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
        async move {
            let kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
            me.client.intent_list(me.repo, Some(&me.token), &kinds).await
        }
        .boxed()
    }

    fn claim(&self, id: &str) -> BoxFuture<'static, Result<Claim, BacklogError>> {
        let (me, id) = (self.clone(), id.to_string());
        async move { me.client.intent_claim(me.repo, Some(&me.token), &id, &me.machine).await }.boxed()
    }

    fn beat(&self, id: &str) -> BoxFuture<'static, Result<bool, BacklogError>> {
        let (me, id) = (self.clone(), id.to_string());
        async move { me.client.intent_beat(me.repo, Some(&me.token), &id).await }.boxed()
    }

    fn finish(
        &self,
        id: &str,
        ending: Ending,
        result: Option<Value>,
        why: Option<String>,
    ) -> BoxFuture<'static, Result<bool, BacklogError>> {
        let (me, id) = (self.clone(), id.to_string());
        async move {
            let sent = me
                .client
                .intent_finish(me.repo, Some(&me.token), &id, ending, result.as_ref(), why.as_deref())
                .await;
            if let Err(e) = &sent {
                me.failures.add("intent_finish", &id, e);
            }
            sent
        }
        .boxed()
    }
}

/// A project's puller: the handlers registered and the rows this computer is running or has dealt with.
pub struct Puller<C> {
    handlers: Vec<Arc<dyn Handler<C>>>,
    /// Rows this computer took and must not run again: running now, or ended (a row whose finish the store never took
    /// stays claimed, and is not run twice). Forgotten once the store no longer lists them.
    taken: Arc<Mutex<BTreeSet<String>>>,
    tasks: Vec<JoinHandle<()>>,
    beat_every: Duration,
    retry_every: Duration,
}

impl<C> Default for Puller<C> {
    fn default() -> Self {
        Self {
            handlers: Vec::new(),
            taken: Default::default(),
            tasks: Vec::new(),
            beat_every: BEAT_EVERY,
            retry_every: FINISH_RETRY,
        }
    }
}

impl<C> Puller<C> {
    /// Registers the handler for a kind of row: the one line that adds a kind.
    pub fn register(mut self, handler: Arc<dyn Handler<C>>) -> Self {
        self.handlers.push(handler);
        self
    }

    /// How often a running row beats, and how long a finish waits before its next try.
    pub fn timing(mut self, beat_every: Duration, retry_every: Duration) -> Self {
        (self.beat_every, self.retry_every) = (beat_every, retry_every);
        self
    }

    /// Rows running now; ended ones are forgotten.
    pub fn active(&mut self) -> u32 {
        self.tasks.retain(|t| !t.is_finished());
        self.tasks.len() as u32
    }

    fn kinds(&self) -> Vec<&'static str> {
        let mut kinds: Vec<&'static str> = self.handlers.iter().map(|h| h.kind()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        kinds
    }

    fn taken(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.taken.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Lists the open rows and takes each one a handler runs, oldest first. `cx` is built once, and only if a row is
    /// won. Returns how many rows it started. A store that can't be read or a claim that fails changes nothing: the
    /// rows stay as they were and the next pass tries again. The caller gates it (paused, signed out, over a limit):
    /// this takes every row it is asked to.
    pub async fn pass(&mut self, store: &Arc<dyn IntentStore>, cx: impl FnOnce() -> C) -> u32 {
        if self.handlers.is_empty() {
            return 0;
        }
        let rows = match store.list(&self.kinds()).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("intents: couldn't list the requests: {e}");
                return 0;
            }
        };
        self.taken().retain(|id| rows.iter().any(|r| &r.id == id));
        let mut make = Some(cx);
        let mut built: Option<C> = None;
        let mut started = 0;
        for row in rows {
            if !row.open() || self.taken().contains(&row.id) {
                continue;
            }
            let Some(handler) = self
                .handlers
                .iter()
                .find(|h| h.kind() == row.kind && h.takes(&row))
                .cloned()
            else {
                continue;
            };
            match store.claim(&row.id).await {
                Ok(Claim { won: true, .. }) => {}
                // Lost to another computer, or held back by the board's queue: not an error.
                Ok(_) => continue,
                Err(e) => {
                    tracing::warn!("intents: couldn't claim {}: {e}", row.id);
                    continue;
                }
            }
            self.taken().insert(row.id.clone());
            let cx = built.get_or_insert_with(|| (make.take().expect("built once"))());
            tracing::info!("intents: running {} {}", row.kind, row.id);
            let id = row.id.clone();
            let running = handler.run(row, cx);
            let (store, beat, retry) = (store.clone(), self.beat_every, self.retry_every);
            self.tasks.push(tokio::spawn(run_row(store, id, running, beat, retry)));
            started += 1;
        }
        started
    }
}

/// One beat: a lost claim or an unreachable store is logged, never fatal.
async fn beat(store: &Arc<dyn IntentStore>, id: &str) {
    match store.beat(id).await {
        Ok(true) => {}
        Ok(false) => tracing::warn!("intents: {id}: the claim is gone, finishing anyway"),
        Err(e) => tracing::warn!("intents: {id}: the beat didn't reach the store: {e}"),
    }
}

/// One claimed row from its first beat to its finish.
async fn run_row(
    store: Arc<dyn IntentStore>,
    id: String,
    running: BoxFuture<'static, Outcome>,
    beat_every: Duration,
    retry_every: Duration,
) {
    tokio::pin!(running);
    // The first beat goes before anything else: it makes the row `running`. Then one every `beat_every`.
    beat(&store, &id).await;
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + beat_every, beat_every);
    let outcome = loop {
        tokio::select! {
            o = &mut running => break o,
            _ = tick.tick() => beat(&store, &id).await,
        }
    };
    let (ending, result, why) = match outcome {
        Outcome::Done(v) => (Ending::Done, Some(v), None),
        Outcome::Refused(why) => (Ending::Refused, None, Some(why)),
        Outcome::Failed(why) => (Ending::Failed, None, Some(why)),
        Outcome::Abandoned => return,
    };
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(retry_every).await;
        }
        match store.finish(&id, ending, result.clone(), why.clone()).await {
            Ok(_) => return,
            Err(e) => tracing::warn!("intents: {id}: the result didn't reach the store: {e}"),
        }
    }
    tracing::error!("intents: {id}: the result never reached the store; it isn't run again");
}

#[cfg(test)]
mod tests;
