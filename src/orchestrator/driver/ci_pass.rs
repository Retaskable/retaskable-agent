//! The CI watcher's pass on a project's loop (docs/specs/ci-watch-through-store.md section 3, item 2):
//! at most every 2 minutes, on the computer the store names as the project's rollout runner and only with the shared
//! backlog linked, one pass over the open pull requests runs in the background (`crate::ci_pass`), one at a time.
//! A paused project still watches (like the TS agent's `maybeWatchCi`, called before its pause check).
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures_util::future::BoxFuture;
use tokio::task::JoinHandle;

use super::project::ProjectLoop;
use crate::backlog::ci_result::CiVerdict;
use crate::ci_pass::{CiHost, EVERY, Fixer, Fixing, Judge, Pass, now_ms};
use crate::ci_pulls::LoginCache;
use crate::config::ProjectConfig;

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
        what: String,
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
        what: &'a str,
        dir: &'a Path,
    ) -> BoxFuture<'a, bool> {
        self.host.fix(
            &self.project,
            pr,
            title.into(),
            branch.into(),
            what.into(),
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
    /// Starts a pass when one is due: this computer is the project's runner, it holds the backlog link (`token`) and
    /// knows the repo's id, no pass is still running and the last began 2 minutes ago. Not while stopping.
    pub(super) fn ci_pass(
        &mut self,
        watch: &Arc<dyn CiWatchHost>,
        runner: bool,
        token: Option<String>,
        stopping: bool,
        now: Instant,
    ) {
        let state = &mut self.ci_watch;
        if state.task.take_if(|t| t.is_finished()).is_some() {
            // Its failures were logged where they happened.
        }
        let (Some(repo), Some(token)) = (self.repo, token) else {
            return;
        };
        if stopping || !runner || state.task.is_some() || state.looked.is_some_and(|t| now.duration_since(t) < EVERY) {
            return;
        }
        state.looked = Some(now);
        let items: HashSet<String> = self.items.values().filter_map(|i| i.branch.clone()).collect();
        let (client, project, since, login, fixing, id) = (
            self.client.clone(),
            self.project.clone(),
            state.since.clone(),
            state.login.clone(),
            state.fixing.clone(),
            self.id.clone(),
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
                logins: &login,
                bell: &|title, body| watch.bell(title, body),
                now: now_ms(),
            };
            // The map is only held between awaits of this task, one pass at a time.
            let mut kept = std::mem::take(&mut *since.lock().unwrap());
            let found = pass.run(&mut kept).await;
            *since.lock().unwrap() = kept;
            match found {
                Ok(0) => {}
                Ok(n) => tracing::info!("{id}: ci watch: {n} new finding(s)"),
                Err(e) => tracing::warn!("{id}: ci watch: {e:#}"),
            }
        }));
    }
}
