//! The Fix button (docs/specs/ci-watch-through-store.md section 3, item 4; `fixPullOnce` and `ciFixNow` in the TS
//! agent's `ciWatch.ts`). The owner's word that one pull request at one head is to be fixed once. [`Pass::press`] runs
//! the checks in the order the TS agent does and sends the head's facts with `button` set: the store counts the attempt
//! and answers `judge`, or refuses in its own words. What came back is carried out in the background by [`Press::begin`],
//! which answers at once with "Trying a fix for pull request #N." and leaves the rest to the pull request's log.
//!
//! The rule that never pushes to a fork holds for the button too: a press on a pull request from a fork is refused
//! before the store is asked (TS sends `mayPush: true` for any press and would push by branch name), so no attempt is
//! counted and no session runs.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::task::JoinHandle;

use super::{FixHold, Fixer, Fixing, Judge, Pass, checks_of, now_ms};
use crate::backlog::Client;
use crate::backlog::ci_result::{CiAnswer, CiFacts, CiState};
use crate::ci_pulls::LoginCache;

/// Why a press didn't start a fix: the computer said no in plain words, or something broke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PressError {
    Refused(String),
    Failed(String),
}

/// The words of the refusals, as the TS agent says them.
pub const NOT_WATCHING: &str = "This computer isn't watching pull requests.";
const NOT_THE_ONE: &str = "This computer isn't the one watching this project's pull requests.";

fn already(pr: u64) -> String {
    format!("A fix is already running for pull request #{pr}.")
}

fn no_longer_open(pr: u64) -> String {
    format!("Pull request #{pr} is no longer open at that commit.")
}

fn from_a_fork(pr: u64) -> String {
    format!("Pull request #{pr} comes from a fork, so this computer won't push a fix to it.")
}

fn not_fixable(pr: u64) -> String {
    format!("Pull request #{pr} isn't one the watcher can fix.")
}

/// A bell line (title, body), word for word.
pub type Bell = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// What a press that the store agreed to carries on with.
pub struct Pressed {
    pub says: String,
    facts: CiFacts,
    answer: CiAnswer,
    /// The head stays held until the carry-out ends.
    hold: FixHold,
}

fn broke(e: impl std::fmt::Display) -> PressError {
    PressError::Failed(format!("{e:#}"))
}

impl Pass<'_> {
    /// The checks of a press, in the TS agent's order: nothing already running on `pr@sha`, this computer the
    /// runner (`runner`), the pull request open at exactly `sha`, then the store's word on the head's facts.
    pub async fn press(&self, pr: u64, sha: &str, runner: bool) -> Result<Pressed, PressError> {
        let Some(hold) = self.fixing.hold(pr, sha) else {
            return Err(PressError::Refused(already(pr)));
        };
        if !runner {
            return Err(PressError::Refused(NOT_THE_ONE.into()));
        }
        let host = self.host.clone();
        let pulls = tokio::task::spawn_blocking(move || host.open_pulls())
            .await
            .map_err(broke)?
            .map_err(broke)?;
        let Some(p) = pulls.into_iter().find(|p| p.number == pr && p.head == sha) else {
            return Err(PressError::Refused(no_longer_open(pr)));
        };
        // Never a fork's: refused before the store is asked, so no attempt is counted and nothing runs.
        if p.cross_repository {
            return Err(PressError::Refused(from_a_fork(pr)));
        }
        let checks = checks_of(&self.host, p.head.clone()).await.map_err(broke)?;
        // Anyone's pull request in this repository, wherever its author stands on `ciWatchAllow`.
        let may_push = true;
        let conflicting = p.conflicting();
        let base_head = if conflicting && may_push {
            let host = self.host.clone();
            tokio::task::spawn_blocking(move || host.base_head())
                .await
                .map_err(broke)?
        } else {
            None
        };
        let facts = CiFacts {
            pr,
            sha: p.head.clone(),
            title: p.title.clone(),
            branch: p.branch.clone(),
            may_push,
            button: true,
            conflicting,
            pending_since: (checks.state == CiState::Pending).then_some(self.now),
            checks,
            red_now: 0,
            base_head,
            did: None,
        };
        let answer = self.send(&facts).await.map_err(broke)?;
        if !answer.ok {
            return Err(PressError::Refused(answer.refused.unwrap_or_else(|| not_fixable(pr))));
        }
        Ok(Pressed {
            says: format!("Trying a fix for pull request #{pr}."),
            facts,
            answer,
            hold,
        })
    }

    /// Carries out what the store answered a press, then lets go of the head. The pending times are not the press's:
    /// a rerun it never asks for (a press is a red head) has nothing to forget.
    pub async fn finish_press(&self, pressed: Pressed) -> anyhow::Result<()> {
        let Pressed {
            facts, answer, hold, ..
        } = pressed;
        let carried = self.carry_out(&facts, answer, &mut HashMap::new()).await;
        drop(hold);
        carried
    }
}

/// Everything a press needs, owned: the carry-out runs on after the row ended, so nothing here is borrowed from a pass.
pub struct Press {
    pub host: Arc<dyn super::CiHost>,
    pub judge: Arc<dyn Judge>,
    pub fixer: Arc<dyn Fixer>,
    pub client: Arc<Client>,
    pub repo: u64,
    pub token: String,
    /// Branches of the board's items.
    pub items: HashSet<String>,
    pub allow: Vec<String>,
    pub logins: Arc<LoginCache>,
    pub bell: Bell,
    pub fixing: Fixing,
    /// Whether the store last named this computer the project's runner.
    pub runner: Arc<AtomicBool>,
}

/// A press that went ahead: what to say at once, and the fix going on behind it.
pub struct Started {
    pub says: String,
    pub done: JoinHandle<()>,
}

impl Press {
    fn pass(&self) -> Pass<'_> {
        Pass {
            host: self.host.clone(),
            judge: &*self.judge,
            fixer: &*self.fixer,
            client: &self.client,
            repo: self.repo,
            token: Some(&self.token),
            items: &self.items,
            allow: &self.allow,
            fixing: &self.fixing,
            logins: &self.logins,
            bell: &*self.bell,
            now: now_ms(),
        }
    }

    /// Checks the press, then runs the fix on in the background (`ciFixNow`): its failures are logged, not answered.
    pub async fn begin(self: &Arc<Self>, pr: u64, sha: &str) -> Result<Started, PressError> {
        let runner = self.runner.load(Ordering::SeqCst);
        let pressed = self.pass().press(pr, sha, runner).await?;
        let says = pressed.says.clone();
        let me = self.clone();
        let done = tokio::spawn(async move {
            if let Err(e) = me.pass().finish_press(pressed).await {
                tracing::info!("ci fix: {e:#}");
            }
        });
        Ok(Started { says, done })
    }
}
