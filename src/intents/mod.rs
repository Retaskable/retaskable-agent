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
pub mod fix_requested;
pub mod main_pr;
pub mod merge_item;
pub mod sync;
pub mod take_over;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU32, Ordering};
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
    /// `done`, with the derived result (at most 4 KB, never a secret); `Null` is no result.
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
    /// Whether a row of this kind must never run twice (`NOT_TWICE` in the TS agent's `intentPuller.ts`: merge, sync,
    /// main PR). A row this computer held before a restart is then failed, not run again.
    fn not_twice(&self) -> bool {
        false
    }
    /// Whether a row of this kind starts a session or a run (`STARTS_WORK` in the TS agent's `intentPuller.ts`, less
    /// `ask`, which has its own gate): a paused project, or a computer already running [`MAX_WORK_ROWS`] such rows,
    /// leaves it open for another computer.
    fn starts_work(&self) -> bool {
        false
    }
    /// Whether a row of this kind runs a Claude session (`RUNS_SESSION`): a computer signed out of Claude leaves it open.
    fn runs_session(&self) -> bool {
        false
    }
    /// Runs the claimed row.
    fn run(&self, row: IntentRow, cx: &C) -> BoxFuture<'static, Outcome>;
}

/// Rows that start work run this many at a time on a computer (`MAX_WORK_ROWS`); the rest wait, open, to be taken.
pub const MAX_WORK_ROWS: u32 = 2;

/// How many rows that start work this computer is running, over every project and kind: a clone is the same count.
#[derive(Clone, Default)]
pub struct Work(Arc<AtomicU32>);

impl Work {
    /// Rows running now.
    pub fn running(&self) -> u32 {
        self.0.load(Ordering::SeqCst)
    }

    fn start(&self) -> WorkGuard {
        self.0.fetch_add(1, Ordering::SeqCst);
        WorkGuard(self.0.clone())
    }
}

/// One row that starts work, counted until it ends (or is dropped).
struct WorkGuard(Arc<AtomicU32>);

impl Drop for WorkGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What this computer and the project say about taking rows that start work: the loop's own state, never the store's
/// say-so (`intentCan`). The default holds nothing back, for a kind that starts no work.
#[derive(Clone, Default)]
pub struct Gates {
    /// This computer's or the store's pause on the project.
    pub paused: bool,
    /// Claude Code was found signed out here.
    pub signed_out: bool,
    /// The rows that start work running on this computer.
    pub work: Work,
}

impl Gates {
    /// Whether a row of a kind with these flags is left open for now.
    fn holds(&self, starts_work: bool, runs_session: bool) -> bool {
        (starts_work && (self.paused || self.work.running() >= MAX_WORK_ROWS)) || (runs_session && self.signed_out)
    }
}

/// Why a row of a not-twice kind held from before a restart ends `failed` (word for word as the TS agent says it).
const RESTARTED: &str = "This computer restarted before it finished; check, then ask again";

/// Why a row whose reply the loop dropped ends `failed`: the loop stopped or panicked, and what it was doing (a merge)
/// may have gone through, so the row must say something.
const STOPPED: &str = "This computer stopped before it answered; check, then ask again";

/// What a handler answers when the loop drops its reply (or its ask can't be sent). A restart or Quit in progress
/// cuts the row quietly (the store opens it again once the claim is stale); anything else, a panic in the loop's
/// pass, fails it in words, as silence is worst for a merge that may have gone through.
pub(crate) fn dropped() -> Outcome {
    if crate::process_tree::stopping() || crate::runs::winding_down() {
        Outcome::Abandoned
    } else {
        Outcome::Failed(STOPPED.into())
    }
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
        self.pass_gated(store, &Gates::default(), cx).await
    }

    /// [`Puller::pass`] that leaves open the rows of a kind that starts work while `gates` hold it back: before the
    /// claim, so another computer can take it, and a row it already runs counts toward [`MAX_WORK_ROWS`].
    pub async fn pass_gated(&mut self, store: &Arc<dyn IntentStore>, gates: &Gates, cx: impl FnOnce() -> C) -> u32 {
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
            if gates.holds(handler.starts_work(), handler.runs_session()) {
                continue;
            }
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
            // Not in `taken` yet but listed as claimed or running: this computer held it before a restart (another
            // computer's claim would have lost above). A kind that must not run twice ends here, unrun (TS `checkNow`).
            if matches!(row.status.as_str(), "claimed" | "running") && handler.not_twice() {
                tracing::info!("intents: not running {} {} again", row.kind, row.id);
                let (store, retry) = (store.clone(), self.retry_every);
                let id = row.id.clone();
                let counted = handler.starts_work().then(|| gates.work.start());
                self.tasks.push(tokio::spawn(async move {
                    let _counted = counted;
                    finish(&store, &id, Ending::Failed, None, Some(RESTARTED.into()), retry).await
                }));
                started += 1;
                continue;
            }
            let cx = built.get_or_insert_with(|| (make.take().expect("built once"))());
            tracing::info!("intents: running {} {}", row.kind, row.id);
            let id = row.id.clone();
            let running = handler.run(row, cx);
            let (store, beat, retry) = (store.clone(), self.beat_every, self.retry_every);
            let counted = handler.starts_work().then(|| gates.work.start());
            self.tasks.push(tokio::spawn(async move {
                let _counted = counted;
                run_row(store, id, running, beat, retry).await
            }));
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
        // A handler with nothing to say (a merge) ends the row with no result at all, not a null one.
        Outcome::Done(Value::Null) => (Ending::Done, None, None),
        Outcome::Done(v) => (Ending::Done, Some(v), None),
        Outcome::Refused(why) => (Ending::Refused, None, Some(why)),
        Outcome::Failed(why) => (Ending::Failed, None, Some(why)),
        Outcome::Abandoned => return,
    };
    finish(&store, &id, ending, result, why, retry_every).await;
}

/// The finish, tried 3 times: a store that can't be reached is retried, then the row stays claimed and is not run again.
async fn finish(
    store: &Arc<dyn IntentStore>,
    id: &str,
    ending: Ending,
    result: Option<Value>,
    why: Option<String>,
    retry_every: Duration,
) {
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(retry_every).await;
        }
        match store.finish(id, ending, result.clone(), why.clone()).await {
            Ok(_) => return,
            Err(e) => tracing::warn!("intents: {id}: the result didn't reach the store: {e}"),
        }
    }
    tracing::error!("intents: {id}: the result never reached the store; it isn't run again");
}

#[cfg(test)]
mod tests;
