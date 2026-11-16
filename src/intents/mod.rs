//! The intent puller (docs/specs/intent-rows.md section 3.3; `intentPuller.ts` in Retaskable.com's agent). The website
//! writes what a person wants as a row in the store; this computer lists the open rows, takes the ones it has a
//! handler for and can run now, keeps its claim fresh while the handler runs, and ends the row as `done` with the
//! handler's derived result, `refused` with its reason in plain words, or `failed`.
//!
//! A row still running when the process dies is not kept: after a restart the rows are listed again, and one this
//! computer held is `claimed` under its own agent id, so claiming it again keeps the claim (the store lets a claim go
//! after a few minutes without a beat). A finish the store hasn't taken is kept in a file of the puller's own
//! (`keep.rs`) and sent again after a restart.
//!
//! A kind is added by registering one [`Handler`]; the puller itself knows nothing of any kind.
pub mod draft_wanted;
pub mod finish_steps;
pub mod fix_requested;
pub mod keep;
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
/// How long to wait before sending a finish the store didn't take again (the first few tries).
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
    /// Where finishes the store hasn't taken are kept across a restart, once [`Puller::keeping`] says where.
    keep_in: Option<std::path::PathBuf>,
    keep: Option<keep::Keep>,
}

impl<C> Default for Puller<C> {
    fn default() -> Self {
        Self {
            handlers: Vec::new(),
            taken: Default::default(),
            tasks: Vec::new(),
            beat_every: BEAT_EVERY,
            retry_every: FINISH_RETRY,
            keep_in: None,
            keep: None,
        }
    }
}

impl<C> Puller<C> {
    /// Registers the handler for a kind of row: the one line that adds a kind.
    pub fn register(mut self, handler: Arc<dyn Handler<C>>) -> Self {
        self.handlers.push(handler);
        self
    }

    /// Keeps the finishes the store hasn't taken in `dir` (the project's folder), so a restart sends them again.
    pub fn keeping(mut self, dir: &std::path::Path) -> Self {
        self.keep_in = Some(dir.to_path_buf());
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

    fn sending(&self) -> Sending {
        Sending {
            keep: self.keep.clone(),
            already: Duration::ZERO,
            retry_every: self.retry_every,
        }
    }

    fn kinds(&self) -> Vec<&'static str> {
        let mut kinds: Vec<&'static str> = self.handlers.iter().map(|h| h.kind()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        kinds
    }

    /// The first pass after a start sends again, at once, each finish kept from before; its row is `taken`, so it is
    /// never run again meanwhile. Whatever the store answers ends it; the day it is kept for counts from when it was first
    /// listed.
    fn resend_kept(&mut self, store: &Arc<dyn IntentStore>) {
        let Some(dir) = self.keep_in.take() else {
            return;
        };
        let keep = keep::Keep::open(&dir, &self.kinds());
        for (id, owed) in keep.owed() {
            let Some(ending) = owed.ending() else {
                keep.done(&id);
                continue;
            };
            let already = Duration::from_secs(keep::now_secs().saturating_sub(owed.since).max(0) as u64);
            self.taken().insert(id.clone());
            let (store, retry, keep) = (store.clone(), self.retry_every, keep.clone());
            self.tasks.push(tokio::spawn(async move {
                let sending = Sending {
                    keep: Some(keep),
                    already,
                    retry_every: retry,
                };
                finish(&store, &id, ending, owed.result, owed.why, sending).await
            }));
        }
        self.keep = Some(keep);
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
        self.resend_kept(store);
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
                let sending = self.sending();
                let store = store.clone();
                let id = row.id.clone();
                let counted = handler.starts_work().then(|| gates.work.start());
                self.tasks.push(tokio::spawn(async move {
                    let _counted = counted;
                    finish(&store, &id, Ending::Failed, None, Some(RESTARTED.into()), sending).await
                }));
                started += 1;
                continue;
            }
            let cx = built.get_or_insert_with(|| (make.take().expect("built once"))());
            tracing::info!("intents: running {} {}", row.kind, row.id);
            let id = row.id.clone();
            let running = handler.run(row, cx);
            let (store, beat, sending) = (store.clone(), self.beat_every, self.sending());
            let counted = handler.starts_work().then(|| gates.work.start());
            self.tasks.push(tokio::spawn(async move {
                let _counted = counted;
                run_row(store, id, running, beat, sending).await
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
    sending: Sending,
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
    finish(&store, &id, ending, result, why, sending).await;
}

/// How long a finish the store doesn't take is kept and sent again before it is given up on.
const FINISH_KEPT: Duration = Duration::from_secs(24 * 60 * 60);
/// The wait between tries once the first few quick ones are used.
const FINISH_SLOW: Duration = Duration::from_secs(30);

/// How a finish is sent: where it is kept, how long it has been owed already (a finish read back from the file), and
/// the wait between the first few tries.
struct Sending {
    keep: Option<keep::Keep>,
    already: Duration,
    retry_every: Duration,
}

/// The finish, sent again until the store answers (the owner's outage rule, 2026-11-16: "it should be able to finish
/// what it's doing but it can't pick up the next thing till the store is back."): a few quick tries, then one every
/// [`FINISH_SLOW`] for up to [`FINISH_KEPT`]. The row stays in `taken` and its task stays counted meanwhile, so
/// nothing runs it again, and with a [`keep::Keep`] it is in the puller's file before the first send, so a restart
/// sends it again. The store takes the finish of a claim that lapsed while no other computer claimed the row; any
/// answer, `ok: false` (another computer has it) included, ends the tries and removes it from the file. An improvement
/// over the TypeScript agent (`intentPuller.ts`), which tries three times and then leaves the row claimed.
async fn finish(
    store: &Arc<dyn IntentStore>,
    id: &str,
    ending: Ending,
    result: Option<Value>,
    why: Option<String>,
    sending: Sending,
) {
    let Sending {
        keep,
        already,
        retry_every,
    } = sending;
    if let Some(keep) = &keep {
        keep.add(id, ending, &result, &why);
    }
    let mut waited = already;
    for attempt in 0u32.. {
        if attempt > 0 {
            let pause = if attempt < 3 { retry_every } else { FINISH_SLOW };
            tokio::time::sleep(pause).await;
            waited += pause;
        }
        match store.finish(id, ending, result.clone(), why.clone()).await {
            Ok(_) => {}
            Err(e) if waited >= FINISH_KEPT => {
                tracing::error!("intents: {id}: the result never reached the store in 24 hours; giving it up: {e}");
            }
            Err(e) => {
                tracing::warn!("intents: {id}: the result didn't reach the store, keeping it: {e}");
                continue;
            }
        }
        if let Some(keep) = &keep {
            keep.done(id);
        }
        return;
    }
}

#[cfg(test)]
mod tests;
