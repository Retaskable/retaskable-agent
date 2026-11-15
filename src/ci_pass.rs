//! The CI watcher's pass (docs/specs/ci-watch-through-store.md section 3, item 2; `ciWatchPass` and `carryOut` in
//! the TS agent's `ciWatch.ts`). On the computer the store names as the project's runner it reads the open pull
//! requests, sends each head's raw facts to the store (`ci_result`) and runs the one instruction that comes back,
//! then says what it did and asks again, at most [`STEPS_MAX`] times per head. It keeps no verdict of its own: the
//! store decides and counts. `mayPush` is `may_push_to` with the signed-in login, and `baseHead` goes with a
//! conflicting head it may push to. `mergeMain` is run (a plain push to the pull request's branch, only where
//! `mayPush`), and so is `fix` (a write session in a throwaway worktree, then a plain push, only where `mayPush`).
//! Scheduling (the runner, the 2 minutes) is the project loop's (`driver/ci_pass.rs`).
//! The Fix button (`fixPullOnce`, `ciFixNow`) is [`Pass::press`] and [`press::Press`]: the same facts with `button`
//! set, and the same carry-out, run in the background once the store has agreed to it.
pub mod host;
pub mod press;
// The fake `gh` is a shell script.
#[cfg(all(test, unix))]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
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

/// The write session that fixes a pull request's own red.
pub trait Fixer: Send + Sync {
    /// Runs the session in `dir` (a worktree on the branch); true when it went ok.
    fn fix<'a>(&'a self, pr: u64, title: &'a str, branch: &'a str, what: &'a str, dir: &'a Path)
    -> BoxFuture<'a, bool>;
}

/// The heads a judge, fix or merge is under way on (`fixing:<pr>@<sha>` in the TS agent's watcher memory), held by a
/// pass's carry-out and by a Fix press until it ends. Shared by the pass and the presses, so neither runs a second one
/// on the same head: that would double the attempts and could push twice.
#[derive(Clone, Default)]
pub struct Fixing(Arc<Mutex<HashSet<String>>>, Arc<Mutex<HashSet<String>>>);

/// A head held in [`Fixing`]; let go when dropped, however the work ended.
pub struct FixHold {
    held: Fixing,
    key: String,
}

impl Drop for FixHold {
    fn drop(&mut self) {
        self.held.set().remove(&self.key);
    }
}

impl Fixing {
    fn set(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Holds `pr@sha`; `None` when something is under way on it already.
    pub fn hold(&self, pr: u64, sha: &str) -> Option<FixHold> {
        let key = key(pr, sha);
        self.set().insert(key.clone()).then(|| FixHold {
            held: self.clone(),
            key,
        })
    }

    /// True the first time it is asked of `pr@sha` (a held write step is said once per head, not every 2 minutes).
    fn first_held_note(&self, pr: u64, sha: &str) -> bool {
        self.1.lock().unwrap_or_else(|e| e.into_inner()).insert(key(pr, sha))
    }

    /// Forgets the held-write notes of heads that are not `live` any more.
    fn forget_held_notes(&self, live: &HashSet<String>) {
        self.1
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|k| live.contains(k));
    }

    /// Whether something is under way on `pr@sha`.
    pub fn holds(&self, pr: u64, sha: &str) -> bool {
        self.set().contains(&key(pr, sha))
    }
}

/// One pass over a project's open pull requests.
pub struct Pass<'a> {
    pub host: Arc<dyn CiHost>,
    pub judge: &'a dyn Judge,
    pub fixer: &'a dyn Fixer,
    pub client: &'a Client,
    pub repo: u64,
    pub token: Option<&'a str>,
    /// Branches of the board's items: a `retaskable/` one among them is a `watch` run's.
    pub items: &'a HashSet<String>,
    /// The project's `ciWatchAllow`: authors whose pull requests it may push to.
    pub allow: &'a [String],
    /// The heads something is under way on, shared with the Fix presses.
    pub fixing: &'a Fixing,
    /// Hold the write steps (`fix`, `mergeMain`): the project is paused, Claude is signed out or the fix engine is at
    /// its limit. Judging and reruns go on. A deliberate divergence from the TS agent, whose pass has no such check
    /// (`ciWatch.ts` `carryOut`) and so pushes and starts sessions through a pause. A Fix press is the owner's own
    /// request and has its own gates (`fix_requests.rs`), so it never sets this.
    pub hold_writes: bool,
    /// The signed-in login, kept once read.
    pub logins: &'a LoginCache,
    /// A bell line (title, body), word for word.
    pub bell: &'a (dyn Fn(&str, &str) + Sync),
    /// Now, in milliseconds: when a head is first seen pending.
    pub now: u64,
}

/// Now, in milliseconds.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// A head's checks, read on a blocking thread (the host runs programs).
async fn checks_of(host: &Arc<dyn CiHost>, sha: String) -> Result<CiChecks> {
    let host = host.clone();
    tokio::task::spawn_blocking(move || host.checks(&sha)).await?
}

/// Whether a store error is about this one answer: refused (a 4xx) or unreadable (no status), so only that head is
/// skipped. A store that can't be reached or is failing (a 5xx) would fail every pull request the same way, so
/// that ends the pass.
fn is_bad_answer(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<crate::backlog::BacklogError>(),
        Some(crate::backlog::BacklogError::BadAnswer { status, .. }) if *status < 500
    )
}

fn key(pr: u64, sha: &str) -> String {
    format!("{pr}@{sha}")
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
        // Pull requests of this repository whose checks couldn't be read, and the first reason.
        let (mut own, mut unread, mut why) = (0_usize, 0_usize, None);
        for p in pulls {
            if p.branch.starts_with("retaskable/") && self.items.contains(&p.branch) {
                continue;
            }
            own += usize::from(!p.cross_repository);
            // A fork's pull request, or a token without the scope, can't be read: that one is skipped, not the pass.
            let checks = match checks_of(&self.host, p.head.clone()).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::info!("ci watch: pull request #{} skipped: {e:#}", p.number);
                    if !p.cross_repository {
                        unread += 1;
                        why.get_or_insert(format!("#{}: {e:#}", p.number));
                    }
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
        // Every pull request of this repository unreadable is an outage (a token without the scope, `gh` signed out), not
        // quiet: the pass ends with it said, so the loop's warning shows it. The TS agent skips each one at info level.
        if let Some(why) = why.filter(|_| own > 0 && unread == own) {
            anyhow::bail!("the checks of none of the {own} open pull request(s) could be read (first: {why})");
        }
        // Heads that moved on or closed are forgotten.
        let live: HashSet<String> = read.iter().map(|(p, ..)| key(p.number, &p.head)).collect();
        since.retain(|k, _| live.contains(k));
        self.fixing.forget_held_notes(&live);
        // Many red in one pass is the machine, not their changes: a fact for the store to judge with.
        let red_now = read.iter().filter(|(_, c, _)| c.state == CiState::Fail).count() as u32;
        let mut found = 0;
        for (p, checks, pending_since) in read {
            // Something is under way on this head: the pass leaves it alone.
            if self.fixing.holds(p.number, &p.head) {
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
            let answer = match self.send(&facts).await {
                Ok(a) => a,
                // One answer this agent can't read is that pull request's: the others are still looked at. (The TS
                // agent ends the pass, so one such answer blocks every pull request.) An unreachable store ends it.
                Err(e) if is_bad_answer(&e) => {
                    tracing::warn!("ci watch: pull request #{} skipped: {e:#}", p.number);
                    continue;
                }
                Err(e) => return Err(e),
            };
            found += u32::from(answer.found);
            let busy = matches!(
                answer.next,
                Some(CiNext::Judge { .. } | CiNext::Fix | CiNext::MergeMain)
            );
            if !busy {
                self.carried(&facts, answer, since).await?;
                continue;
            }
            // `withFixing`: the head is held for the carry-out and let go after it, a failed one too. A press that took
            // it since the store answered is the fix under way on this head, so the pass leaves it to that.
            let Some(_hold) = self.fixing.hold(p.number, &p.head) else {
                tracing::info!("ci watch: pull request #{} has a fix under way, left alone", p.number);
                continue;
            };
            self.carried(&facts, answer, since).await?;
        }
        Ok(found)
    }

    /// Whether the agent may push to the pull request's branch (`may_push_to`). The login is read (on a blocking
    /// thread) only when the pull request lists an author and none is kept yet; a failed read is no own account.
    async fn may_push(&self, p: &OpenPull) -> bool {
        // A fork's is never pushed to (`may_push_to`), so its login need not be read.
        let own = if p.author.is_some() && !p.cross_repository {
            self.login().await
        } else {
            None
        };
        may_push_to(
            &p.branch,
            p.author.as_deref(),
            own.as_deref(),
            self.allow,
            p.cross_repository,
        )
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

    /// [`Self::carry_out`] for one head: a bad answer ends that head's carry-out only, an unreachable store the pass.
    async fn carried(&self, facts: &CiFacts, first: CiAnswer, since: &mut HashMap<String, u64>) -> Result<()> {
        match self.carry_out(facts, first, since).await {
            Err(e) if is_bad_answer(&e) => {
                tracing::warn!("ci watch: pull request #{} left as it is: {e:#}", facts.pr);
                Ok(())
            }
            done => done,
        }
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

    /// A fix session in a throwaway worktree on the branch and, when it committed cleanly, one plain push (`fixOnBranch`
    /// in the TS agent's `git.ts`). False for a failed session, uncommitted work, no commit, a refused push or any failure.
    async fn fix(&self, facts: &CiFacts) -> bool {
        let host = self.host.clone();
        let (pr, branch, sha) = (facts.pr, facts.branch.clone(), facts.sha.clone());
        let opened = tokio::task::spawn_blocking(move || host.fix_tree(pr, &branch, &sha)).await;
        let tree = match opened {
            Ok(Ok(tree)) => tree,
            Ok(Err(e)) => {
                tracing::info!("ci watch: no worktree to fix pull request #{pr}: {e:#}");
                return false;
            }
            Err(_) => return false,
        };
        let ok = self
            .fixer
            .fix(pr, &facts.title, &facts.branch, &facts.checks.what, &tree.dir)
            .await;
        let branch = facts.branch.clone();
        // The tree is dropped (removed) with the thread's closure.
        match tokio::task::spawn_blocking(move || tree.push(&branch, ok)).await {
            Ok(Ok(pushed)) => pushed,
            Ok(Err(e)) => {
                tracing::info!("ci watch: pushing the fix for pull request #{pr} failed: {e:#}");
                false
            }
            Err(_) => false,
        }
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
            // Paused, signed out or at the limit: nothing is pushed or started and nothing said; the store asks again.
            CiNext::Fix | CiNext::MergeMain if self.hold_writes => {
                if self.fixing.first_held_note(facts.pr, &facts.sha) {
                    tracing::info!(
                        "ci watch: pull request #{} waits: the project is paused, Claude is signed out or the engine is at its limit",
                        facts.pr
                    );
                }
                None
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
            // A `do` this agent doesn't know: nothing is run and nothing said, the carry-out for this head ends.
            CiNext::Unknown => {
                tracing::warn!(
                    "ci watch: the store said something to do for pull request #{} that this agent doesn't know, left alone",
                    facts.pr
                );
                None
            }
            // Never run where the computer may not push, as for `mergeMain`.
            CiNext::Fix if !facts.may_push => {
                tracing::warn!(
                    "ci watch: the store said fix for pull request #{}, which this computer may not push to",
                    facts.pr
                );
                None
            }
            CiNext::Fix => Some(CiDid::Fix {
                pushed: self.fix(facts).await,
            }),
        }
    }
}
