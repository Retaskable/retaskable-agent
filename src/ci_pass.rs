//! The CI watcher's pass (docs/specs/ci-watch-through-store.md section 3, item 2; `ciWatchPass` and `carryOut` in
//! the TS agent's `ciWatch.ts`). On the computer the store names as the project's runner it reads the open pull
//! requests, sends each head's raw facts to the store (`ci_result`) and runs the one instruction that comes back,
//! then says what it did and asks again, at most [`STEPS_MAX`] times per head. It keeps no verdict of its own: the
//! store decides and counts. `mayPush` is `may_push_to` with the signed-in login, and `baseHead` goes with a
//! conflicting head it may push to. `mergeMain` is run (a plain push to the pull request's branch, only where
//! `mayPush`); a `fix` that comes is not run yet.
//! Scheduling (the runner, the 2 minutes) is the project loop's (`driver/ci_pass.rs`).
pub mod host;
// The fake `gh` is a shell script.
#[cfg(all(test, unix))]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;

use crate::backlog::Client;
use crate::backlog::ci_result::{CiAnswer, CiChecks, CiDid, CiFacts, CiNext, CiState, CiVerdict};
use crate::ci_judge;
use crate::ci_pulls::{LoginCache, OpenPull, may_push_to};
pub use host::{CiHost, GhCiHost, host_for};

/// How often the pull requests are looked at again.
pub const EVERY: Duration = Duration::from_secs(2 * 60);
/// The most instructions run for one head in one pass (judge, then a rerun, ...).
pub const STEPS_MAX: usize = 4;

/// The read-only session that says whose fault a red is.
pub trait Judge: Send + Sync {
    /// The verdict and its why; `None` when the session couldn't give one.
    fn judge<'a>(
        &'a self,
        pr: u64,
        title: &'a str,
        branch: &'a str,
        what: &'a str,
    ) -> BoxFuture<'a, Option<(CiVerdict, String)>>;
}

/// One pass over a project's open pull requests.
pub struct Pass<'a> {
    pub host: Arc<dyn CiHost>,
    pub judge: &'a dyn Judge,
    pub client: &'a Client,
    pub repo: u64,
    pub token: Option<&'a str>,
    /// Branches of the board's items: a `retaskable/` one among them is a `watch` run's.
    pub items: &'a HashSet<String>,
    /// The project's `ciWatchAllow`: authors whose pull requests it may push to.
    pub allow: &'a [String],
    /// The signed-in login, kept once read.
    pub logins: &'a LoginCache,
    /// A bell line (title, body), word for word.
    pub bell: &'a (dyn Fn(&str, &str) + Sync),
    /// Now, in milliseconds: when a head is first seen pending.
    pub now: u64,
}

/// A head's checks, read on a blocking thread (the host runs programs).
async fn checks_of(host: &Arc<dyn CiHost>, sha: String) -> Result<CiChecks> {
    let host = host.clone();
    tokio::task::spawn_blocking(move || host.checks(&sha)).await?
}

fn key(pr: u64, sha: &str) -> String {
    format!("{pr}@{sha}")
}

/// A judge, fix or merge for `pr@sha` is under way (`fixingKey`): the head is left alone meanwhile, so a second one in
/// the same worktree can't double the attempts or push twice. Kept in the same map as the pending times.
fn fixing_key(pr: u64, sha: &str) -> String {
    format!("fixing:{}", key(pr, sha))
}

impl Pass<'_> {
    /// One look at the open pull requests. `since` is when each head was first seen pending, kept between passes.
    /// Answers how many new findings. A store that can't be reached ends the pass (fail closed); one pull
    /// request whose checks can't be read is skipped.
    pub async fn run(&self, since: &mut HashMap<String, u64>) -> Result<u32> {
        let host = self.host.clone();
        let pulls = tokio::task::spawn_blocking(move || host.open_pulls())
            .await?
            .context("listing the open pull requests")?;
        let mut read: Vec<(OpenPull, CiChecks, Option<u64>)> = Vec::new();
        for p in pulls {
            if p.branch.starts_with("retaskable/") && self.items.contains(&p.branch) {
                continue;
            }
            // A fork's pull request, or a token without the scope, can't be read: that one is skipped, not the pass.
            let checks = match checks_of(&self.host, p.head.clone()).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::info!("ci watch: pull request #{} skipped: {e:#}", p.number);
                    continue;
                }
            };
            let k = key(p.number, &p.head);
            let pending_since = match checks.state {
                CiState::Pending => Some(*since.entry(k).or_insert(self.now)),
                CiState::Pass => {
                    since.remove(&k);
                    None
                }
                CiState::Fail => None,
            };
            read.push((p, checks, pending_since));
        }
        // Heads that moved on or closed are forgotten.
        let live: HashSet<String> = read.iter().map(|(p, ..)| key(p.number, &p.head)).collect();
        since.retain(|k, _| k.starts_with("fixing:") || live.contains(k));
        // Many red in one pass is the machine, not their changes: a fact for the store to judge with.
        let red_now = read.iter().filter(|(_, c, _)| c.state == CiState::Fail).count() as u32;
        let mut found = 0;
        for (p, checks, pending_since) in read {
            // Something is under way on this head: the pass leaves it alone.
            if since.contains_key(&fixing_key(p.number, &p.head)) {
                continue;
            }
            let may_push = self.may_push(&p).await;
            let conflicting = p.conflicting();
            // Only a conflicting head the computer may push to: the sha the store tells a moved main by.
            let base_head = if conflicting && may_push {
                let host = self.host.clone();
                tokio::task::spawn_blocking(move || host.base_head()).await?
            } else {
                None
            };
            let facts = CiFacts {
                pr: p.number,
                sha: p.head.clone(),
                title: p.title.clone(),
                branch: p.branch.clone(),
                may_push,
                button: false,
                conflicting,
                checks,
                pending_since,
                red_now,
                base_head,
                did: None,
            };
            let answer = self.send(&facts).await?;
            found += u32::from(answer.found);
            let busy = matches!(
                answer.next,
                Some(CiNext::Judge { .. } | CiNext::Fix | CiNext::MergeMain)
            );
            if !busy {
                self.carry_out(&facts, answer, since).await?;
                continue;
            }
            // `withFixing`: the key is held for the carry-out and dropped after it, a failed one too.
            let held = fixing_key(p.number, &p.head);
            since.insert(held.clone(), self.now);
            let carried = self.carry_out(&facts, answer, since).await;
            since.remove(&held);
            carried?;
        }
        Ok(found)
    }

    /// Whether the agent may push to the pull request's branch (`may_push_to`). The login is read (on a blocking
    /// thread) only when the pull request lists an author and none is kept yet; a failed read is no own account.
    async fn may_push(&self, p: &OpenPull) -> bool {
        let own = if p.author.is_some() { self.login().await } else { None };
        may_push_to(&p.branch, p.author.as_deref(), own.as_deref(), self.allow)
    }

    async fn login(&self) -> Option<String> {
        if let Some(known) = self.logins.get(|| None) {
            return Some(known);
        }
        let host = self.host.clone();
        let read = tokio::task::spawn_blocking(move || host.login()).await.ok().flatten();
        self.logins.get(|| read)
    }

    async fn send(&self, facts: &CiFacts) -> Result<CiAnswer> {
        self.client
            .ci_result(self.repo, self.token, facts)
            .await
            .with_context(|| format!("ci_result for pull request #{}", facts.pr))
    }

    /// Writes what the store worded, runs what it said to run and says what happened, until it has nothing more.
    async fn carry_out(&self, facts: &CiFacts, first: CiAnswer, since: &mut HashMap<String, u64>) -> Result<()> {
        let mut answer = first;
        for step in 0.. {
            for entry in &answer.logs {
                self.client
                    .log_add(self.repo, self.token, "ci", entry)
                    .await
                    .context("log_add")?;
            }
            for b in &answer.bells {
                (self.bell)(&b.title, &b.body);
            }
            let Some(next) = answer.next.filter(|_| step < STEPS_MAX) else {
                break;
            };
            let Some(did) = self.instruct(facts, next, since).await else {
                break;
            };
            answer = self
                .send(&CiFacts {
                    did: Some(did),
                    ..facts.clone()
                })
                .await?;
        }
        Ok(())
    }

    /// Runs one instruction and says what happened; `None` for one this slice doesn't run.
    async fn instruct(&self, facts: &CiFacts, next: CiNext, since: &mut HashMap<String, u64>) -> Option<CiDid> {
        match next {
            CiNext::Judge { report } => {
                let what = &facts.checks.what;
                let judged = self.judge.judge(facts.pr, &facts.title, &facts.branch, what).await;
                Some(ci_judge::did(judged.as_ref(), report))
            }
            CiNext::Rerun { rerun, hung } => {
                let host = self.host.clone();
                let pr = facts.pr;
                let ok = tokio::task::spawn_blocking(move || host.rerun(pr, hung))
                    .await
                    .unwrap_or(false);
                if ok {
                    since.remove(&key(facts.pr, &facts.sha));
                }
                Some(CiDid::Rerun { rerun, ok })
            }
            // The store never asks for this where it may not push, so a head it may not push to is refused here too
            // (the TS agent doesn't check): nothing is run and nothing said, the carry-out ends.
            CiNext::MergeMain if !facts.may_push => {
                tracing::warn!(
                    "ci watch: the store said mergeMain for pull request #{}, which this computer may not push to",
                    facts.pr
                );
                None
            }
            CiNext::MergeMain => {
                let host = self.host.clone();
                let (pr, branch) = (facts.pr, facts.branch.clone());
                let pushed = tokio::task::spawn_blocking(move || host.merge_main(pr, &branch))
                    .await
                    .unwrap_or(false);
                Some(CiDid::MergeMain { pushed })
            }
            // Fixing is a later slice: the head is left for the store to ask about again.
            CiNext::Fix => {
                tracing::warn!(
                    "ci watch: the store said {next:?} for pull request #{}, which this computer doesn't run yet",
                    facts.pr
                );
                None
            }
        }
    }
}
