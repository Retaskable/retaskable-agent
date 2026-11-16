//! Deploy requests on the server (docs/specs/agent/deploy-requests.md): the website's TestFlight build, promote,
//! hosting and go-live-prepare asks are rows in the store. This computer lists the open ones and claims only those it
//! can run: its own checks of its own keys decide, never the server. A claimed request is beaten while it runs and ends
//! as `done`, `refused` (with the reason) or `failed`. The request carries no secret and the result only what the code
//! answered. A store that can't be reached means nothing is claimed (fail closed).
mod deps;
mod go_live;
mod live;
mod promote;
#[cfg(all(test, unix))]
mod promote_tests;
mod testflight;
#[cfg(test)]
mod testflight_tests;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::backlog::deploy::{DeployRequest, DeployStatus, Finish, checked_result};
use crate::owed_file::{KEPT_SECS, OwedFile, now_secs};
pub use deps::{Deps, Outcome, RunError};
pub use live::{Live, PORTED, SpawnArgs, spawn_all, waits_while_paused};

/// The file in the project's folder that holds the results the store hasn't taken, across a restart.
pub const FINISH_FILE: &str = "deploy_finish.pending.json";

/// A `deploy_finish` the store hasn't taken, as the file keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Owed {
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<Map<String, Value>>,
    /// When it was first listed, in epoch seconds.
    since: i64,
}

type Kept = OwedFile<Owed>;

/// How long to wait before listing again when nothing woke the loop: the list is read on a timer, and a push from the
/// store socket (`run`) only reads it earlier.
pub const WAIT: Duration = Duration::from_secs(5);
/// How often a running request tells the store it is alive (the store lets a claim go after 10 minutes without).
pub const BEAT_EVERY: Duration = Duration::from_secs(60);
/// A request that names another computer first is left to it this long, in milliseconds.
pub const PREFER_WAIT_MS: u64 = 5_000;
/// The longest reason sent with a refusal or a failure.
const WHY_MAX: usize = 500;

fn why(text: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("why".into(), json!(crate::text::truncate(text, WHY_MAX)));
    m
}

/// One project's deploy request loop.
pub struct DeployRequests<D: Deps> {
    deps: Arc<D>,
    /// The requests this computer holds now, so a request is never run twice here.
    running: Arc<Mutex<HashSet<String>>>,
    last_said: Option<String>,
    /// The results the store hasn't taken, in a file of this loop's own; `None` keeps them in memory only.
    keep: Option<Kept>,
    /// The results kept from before a start were sent again.
    resumed: bool,
}

impl<D: Deps + 'static> DeployRequests<D> {
    pub fn new(deps: D) -> Self {
        Self {
            deps: Arc::new(deps),
            running: Arc::default(),
            last_said: None,
            keep: None,
            resumed: false,
        }
    }

    /// Keeps a result the store hasn't taken in `dir` (the project's folder), so a restart sends it again.
    pub fn keeping(mut self, dir: &Path) -> Self {
        self.keep = Some(OwedFile::open(dir.join(FINISH_FILE)));
        self
    }

    /// The first round after a start sends again, at once, each result kept from before. Its request is still
    /// `running` in the store and is held here until the store answers, so it is neither run nor claimed again.
    fn resume(&mut self) {
        let Some(keep) = self.keep.clone() else {
            return;
        };
        for (id, owed) in keep.all() {
            let Some(status) = Finish::from_name(&owed.status) else {
                keep.remove(&id);
                continue;
            };
            let already = Duration::from_secs(now_secs().saturating_sub(owed.since).max(0) as u64);
            let result = owed.result.unwrap_or_default();
            self.running.lock().unwrap().insert(id.clone());
            let (deps, running, keep) = (self.deps.clone(), self.running.clone(), keep.clone());
            tokio::spawn(async move {
                finish(deps.as_ref(), &id, status, result, Some(&keep), already).await;
                running.lock().unwrap().remove(&id);
            });
        }
    }

    /// Says `line` unless it was the last thing said (one line per change, not one per poll).
    fn say(&mut self, line: String) {
        if self.last_said.as_deref() != Some(&line) {
            tracing::info!("deploys: {line}");
            self.last_said = Some(line);
        }
    }

    /// How many requests this computer is running now.
    pub fn running(&self) -> usize {
        self.running.lock().unwrap().len()
    }

    /// One round: lists the open requests and claims the ones this computer can run. Resolves once they are taken
    /// (not when they end: each runs on its own task). Returns how long to wait before the next round.
    pub async fn cycle(&mut self) -> Duration {
        if !std::mem::replace(&mut self.resumed, true) {
            self.resume();
        }
        // A deploy log row the store didn't take is sent again, whether or not the list can be read.
        self.deps.flush_logs().await;
        let rows = match self.deps.list().await {
            Ok(rows) => rows,
            Err(e) => {
                self.say(format!("couldn't list the deploy requests: {e}"));
                return WAIT;
            }
        };
        let now = self.deps.now_ms();
        let machine = self.deps.machine();
        for r in rows {
            if r.status != DeployStatus::Requested || self.running.lock().unwrap().contains(&r.id) {
                continue;
            }
            if !self.deps.can(&r).await {
                continue;
            }
            // Asked for on another computer: it has a few seconds to take it first.
            if r.args.prefer.as_deref().is_some_and(|p| p != machine) && now.saturating_sub(r.at) < PREFER_WAIT_MS {
                continue;
            }
            match self.deps.claim(&r.id).await {
                Ok(true) => {
                    self.running.lock().unwrap().insert(r.id.clone());
                    self.say(format!("took {} {}", r.kind.name(), r.id));
                    let (deps, running, keep) = (self.deps.clone(), self.running.clone(), self.keep.clone());
                    tokio::spawn(async move {
                        execute(deps.as_ref(), &r, keep.as_ref()).await;
                        running.lock().unwrap().remove(&r.id);
                    });
                }
                // Another computer won it: nothing to say.
                Ok(false) => {}
                Err(e) => self.say(format!("couldn't claim {}: {e}", r.id)),
            }
        }
        WAIT
    }
}

/// The loop: a round, then [`WAIT`] or until `wake` (the store socket's `facts_changed` naming `deploy_requests`,
/// backlog_socket/wake.rs), whichever is first. With the socket down it is the 5-second read alone.
pub async fn run<D: Deps + 'static>(mut requests: DeployRequests<D>, wake: Arc<tokio::sync::Notify>) {
    loop {
        let wait = requests.cycle().await;
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = wake.notified() => {}
        }
    }
}

/// Runs one claimed request, beating while it runs, and reports how it ended. A refusal (the code's own, with its
/// reason) is `refused`; any other error is `failed` with the same `why`. Never panics on a store error: it is logged.
async fn execute<D: Deps>(deps: &D, r: &DeployRequest, keep: Option<&Kept>) {
    // The first beat marks it `running`.
    let _ = deps.beat(&r.id).await;
    let out = {
        let run = deps.run(r);
        tokio::pin!(run);
        loop {
            tokio::select! {
                out = &mut run => break out,
                _ = tokio::time::sleep(BEAT_EVERY) => {
                    let _ = deps.beat(&r.id).await;
                }
            }
        }
    };
    let (status, result) = match out {
        Ok(Outcome { status, result }) => match checked_result(result) {
            Ok(result) => (status, result),
            Err(e) => (Finish::Failed, why(&e)),
        },
        Err(RunError::Refused(reason)) => (Finish::Refused, why(&reason)),
        Err(RunError::Failed(reason)) => (Finish::Failed, why(&reason)),
    };
    finish(deps, &r.id, status, result, keep, Duration::ZERO).await;
}

/// How long a result the store doesn't take is kept and sent again before it is given up on: by then the store has
/// long since reopened the request for another computer.
const FINISH_KEPT: Duration = Duration::from_secs(KEPT_SECS as u64);

/// Sends the result, and again every [`WAIT`] while the store can't be reached or doesn't take it (the owner's outage
/// rule, 2026-11-16: "it should be able to finish what it's doing but it can't pick up the next thing till the
/// store is back."). Until it answers the request stays in `running`, so it is neither run again nor claimed anew,
/// and with a file to keep it in it is listed there before the first send, so a restart sends it again (`already` is
/// how long it was owed before). The store takes the finish of a claim that lapsed while no other computer claimed the
/// request since; `ok: false` says another computer has it, and is the only answer that drops the result besides
/// [`FINISH_KEPT`]. An improvement over the TypeScript agent, which sends `deploy_finish` once and loses the result
/// when that fails.
async fn finish<D: Deps>(
    deps: &D,
    id: &str,
    status: Finish,
    result: Map<String, Value>,
    keep: Option<&Kept>,
    already: Duration,
) {
    if let Some(keep) = keep {
        let since = keep.get(id).map_or_else(now_secs, |o| o.since);
        keep.put(
            id,
            Owed {
                status: status.name().into(),
                result: Some(result.clone()),
                since,
            },
        );
    }
    let mut waited = already;
    loop {
        match deps.finish(id, status, Some(result.clone())).await {
            Ok(true) => break,
            // The claim went stale and another computer holds it: its answer is the one that counts.
            Ok(false) => {
                tracing::info!("deploys: {id} ended after its claim moved on; the store dropped the result");
                break;
            }
            Err(e) if waited >= FINISH_KEPT => {
                tracing::error!("deploys: {id}: the result never reached the store in 24 hours, giving it up: {e}");
                break;
            }
            Err(e) => {
                if waited.is_zero() {
                    tracing::warn!("deploys: couldn't report {id}, keeping the result until the store takes it: {e}");
                }
                tokio::time::sleep(WAIT).await;
                waited += WAIT;
            }
        }
    }
    if let Some(keep) = keep {
        keep.remove(id);
    }
}

/// A `done` that carries only what the code answered.
pub fn done(result: Map<String, Value>) -> Outcome {
    Outcome {
        status: Finish::Done,
        result,
    }
}
