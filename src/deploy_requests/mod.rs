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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::backlog::deploy::{DeployRequest, DeployStatus, Finish, checked_result};
pub use deps::{Deps, Outcome, RunError};
pub use live::{Live, PORTED, SpawnArgs, spawn_all, waits_while_paused};

/// How long to wait before listing again: the store has no push to this computer, so the list is read on a timer.
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
}

impl<D: Deps + 'static> DeployRequests<D> {
    pub fn new(deps: D) -> Self {
        Self {
            deps: Arc::new(deps),
            running: Arc::default(),
            last_said: None,
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
                    let (deps, running) = (self.deps.clone(), self.running.clone());
                    tokio::spawn(async move {
                        execute(deps.as_ref(), &r).await;
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

/// Runs one claimed request, beating while it runs, and reports how it ended. A refusal (the code's own, with its
/// reason) is `refused`; any other error is `failed` with the same `why`. Never panics on a store error: it is logged.
async fn execute<D: Deps>(deps: &D, r: &DeployRequest) {
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
    match deps.finish(&r.id, status, Some(result)).await {
        Ok(true) => {}
        // The claim went stale and another computer holds it: its answer is the one that counts.
        Ok(false) => tracing::info!(
            "deploys: {} ended after its claim expired; the store dropped the result",
            r.id
        ),
        Err(e) => tracing::warn!("deploys: couldn't report {}: {e}", r.id),
    }
}

/// A `done` that carries only what the code answered.
pub fn done(result: Map<String, Value>) -> Outcome {
    Outcome {
        status: Finish::Done,
        result,
    }
}
