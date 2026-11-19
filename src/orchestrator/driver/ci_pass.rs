//! The CI watcher's pass on a project's loop (docs/specs/ci-watch-through-store.md section 3, item 2):
//! at most every 2 minutes, on the computer the store names as the project's rollout runner and only with the shared
//! backlog linked, one pass over the open pull requests runs in the background (`crate::ci_pass`), one at a time.
//! A paused project still watches (like the TS agent's `maybeWatchCi`, called before its pause check).
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use tokio::task::JoinHandle;

use super::project::ProjectLoop;
use crate::backlog::BacklogError;
use crate::backlog::ci_result::CiVerdict;
use crate::ci_fix::FixTask;
use crate::ci_pass::{CiHost, EVERY, Fixer, Fixing, Judge, Pass, now_ms};
use crate::ci_pulls::LoginCache;
use crate::config::ProjectConfig;

/// The wait between two `ci_result` calls of one pass.
const PACE: Duration = Duration::from_millis(300);
/// How long the next pass waits after a `429` at the least, and at the most whatever its `Retry-After` says (as the run loop's hold).
const HOLD_LEAST: Duration = Duration::from_secs(60);
const HOLD_MOST: Duration = Duration::from_secs(15 * 60);

/// What the pass needs from this computer besides the store: the code host, the judge's session and the bell.
pub trait CiWatchHost: Send + Sync {
    /// The code host's reads and rerun for `project`.
    fn code_host(&self, project: &ProjectConfig) -> Arc<dyn CiHost>;
    /// The read-only session on a failing pull request: its verdict and why, `None` when it gave none.
    fn judge(
        &self,
        project: &ProjectConfig,
        pr: u64,
        title: String,
        branch: String,
        what: String,
    ) -> BoxFuture<'static, Option<(CiVerdict, String)>>;
    /// The write session that fixes a pull request's own red, run in `dir` (a worktree on the branch): true when it
    /// went ok. It commits and never pushes; the pass pushes.
    fn fix(
        &self,
        project: &ProjectConfig,
        pr: u64,
        title: String,
        branch: String,
        task: FixTask,
        dir: PathBuf,
    ) -> BoxFuture<'static, bool>;
    /// A bell line from the store, word for word.
    fn bell(&self, title: &str, body: &str);
}

/// [`CiWatchHost::judge`] and [`CiWatchHost::fix`] for one project, as the pass takes them.
pub(super) struct ProjectJudge {
    pub(super) host: Arc<dyn CiWatchHost>,
    pub(super) project: ProjectConfig,
}

impl Judge for ProjectJudge {
    fn judge<'a>(
        &'a self,
        pr: u64,
        title: &'a str,
        branch: &'a str,
        what: &'a str,
    ) -> BoxFuture<'a, Option<(CiVerdict, String)>> {
        self.host
            .judge(&self.project, pr, title.into(), branch.into(), what.into())
    }
}

impl Fixer for ProjectJudge {
    fn fix<'a>(
        &'a self,
        pr: u64,
        title: &'a str,
        branch: &'a str,
        task: &'a FixTask,
        dir: &'a Path,
    ) -> BoxFuture<'a, bool> {
        self.host.fix(
            &self.project,
            pr,
            title.into(),
            branch.into(),
            task.clone(),
            dir.to_path_buf(),
        )
    }
}

#[derive(Default)]
pub(super) struct State {
    looked: Option<Instant>,
    task: Option<JoinHandle<()>>,
    /// When each head was first seen pending (`pr@sha`, ms), kept between passes.
    since: Arc<Mutex<HashMap<String, u64>>>,
    /// The signed-in login, kept once read (a failed read is read again on the next pass).
    pub(super) login: Arc<LoginCache>,
    /// The heads a pass's or a press's fix is under way on.
    pub(super) fixing: Fixing,
    /// No pass until then: the store answered `429` (set by the pass, read by the loop).
    held: Arc<Mutex<Option<Instant>>>,
}

#[cfg(test)]
impl State {
    /// Waits for the pass in the background, if one runs.
    pub(super) async fn finish(&mut self) {
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl ProjectLoop {
    /// The branches of board items, whose pull requests the watcher leaves to their own work: every item the store
    /// listed with its pieces, whichever computer holds it, and the items held here.
    pub(super) fn owned_branches(&self) -> HashSet<String> {
        let held = self.items.values().filter_map(|i| i.branch.clone());
        held.chain(self.listed_branches.iter().cloned()).collect()
    }

    /// Starts a pass when one is due: this computer is the project's runner, it holds the backlog link (`token`) and
    /// knows the repo's id, no pass is still running and the last began 2 minutes ago. Not while stopping.
    /// `hold_writes`: the pass judges and reruns but starts no fix and pushes no merge of main.
    pub(super) fn ci_pass(
        &mut self,
        watch: &Arc<dyn CiWatchHost>,
        runner: bool,
        token: Option<String>,
        stopping: bool,
        hold_writes: bool,
        now: Instant,
    ) {
        let items = self.owned_branches();
        let state = &mut self.ci_watch;
        if state.task.take_if(|t| t.is_finished()).is_some() {
            // Its failures were logged where they happened.
        }
        let (Some(repo), Some(token)) = (self.repo, token) else {
            return;
        };
        // How long a head with no check is waited for is the store's `ciGraceMinutes`; unknown, no pass starts.
        let Some(grace) = self.project.tuning.store.map(|t| t.ci_grace_minutes) else {
            return;
        };
        let held = state.held.lock().unwrap().is_some_and(|t| now < t);
        if stopping
            || held
            || !runner
            || state.task.is_some()
            || state
                .looked
                .is_some_and(|t| now.duration_since(t) < EVERY * crate::github_budget::global().slow_factor())
        {
            return;
        }
        state.looked = Some(now);
        let (client, project, since, login, fixing, id, held) = (
            self.client.clone(),
            self.project.clone(),
            state.since.clone(),
            state.login.clone(),
            state.fixing.clone(),
            self.id.clone(),
            state.held.clone(),
        );
        let (host, watch) = (watch.code_host(&project), watch.clone());
        state.task = Some(tokio::spawn(async move {
            let judge = ProjectJudge {
                host: watch.clone(),
                project: project.clone(),
            };
            let pass = Pass {
                host,
                judge: &judge,
                fixer: &judge,
                client: &client,
                repo,
                token: Some(&token),
                items: &items,
                allow: &project.ci_watch_allow,
                fixing: &fixing,
                ci_grace_ms: u64::from(grace) * 60_000,
                hold_writes,
                logins: &login,
                bell: &|title, body| watch.bell(title, body),
                now: now_ms(),
                pace: PACE,
            };
            // The map is only held between awaits of this task, one pass at a time.
            let mut kept = std::mem::take(&mut *since.lock().unwrap());
            let found = pass.run(&mut kept).await;
            *since.lock().unwrap() = kept;
            match found {
                Ok(0) => {}
                Ok(n) => tracing::info!("{id}: ci watch: {n} new finding(s)"),
                Err(e) => {
                    tracing::warn!("{id}: ci watch: {e:#}");
                    // A 429 holds the next pass for the store's Retry-After, as the run loop does for its asks.
                    if let Some(BacklogError::BadAnswer { status: 429, .. }) = e.downcast_ref::<BacklogError>() {
                        let wait = e
                            .downcast_ref::<BacklogError>()
                            .and_then(BacklogError::retry_after)
                            .map_or(HOLD_LEAST, |w| w.clamp(HOLD_LEAST, HOLD_MOST));
                        *held.lock().unwrap() = Some(Instant::now() + wait);
                    }
                }
            }
        }));
    }
}
