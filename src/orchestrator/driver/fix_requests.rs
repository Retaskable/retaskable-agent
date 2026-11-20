//! The Fix button's rows on a project's loop (docs/specs/agent/06.6-ask-claude.md, `fix_requested`; the handler is
//! `intents/fix_requested.rs`): at most every minute, on the computer the store names as the project's rollout runner
//! and only with the shared backlog linked, the intent puller lists the project's fix requests and takes each one.
//! A paused project, a computer signed out of Claude and a computer already running two rows that start work take none:
//! the row stays open for another computer. Not the runner, nothing is even listed. No session slot is used here; the
//! fix itself is the CI watcher's, which keeps going after the row has ended.
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use super::ci_pass::ProjectJudge;
use super::project::ProjectLoop;
use super::signed_out::signed_out;
use super::{CiWatchHost, Shared};
use crate::ci_pass::press::Press;
use crate::intents::fix_requested::{FixCx, FixHandler, Presser};
use crate::intents::{Gates, IntentStore, Puller, StoreLink, Work};

/// How often the store is asked for fix requests (docs/specs/intent-rows.md section 3.3).
const POLL_EVERY: Duration = Duration::from_secs(60);

/// A project's fix requests: the puller (built the first time a pass runs) and when the store was last asked.
pub(super) struct FixRows {
    puller: Option<Puller<FixCx>>,
    polled: Option<Instant>,
    /// How long after one look at the store the next may be.
    pub(super) poll_every: Duration,
}

impl Default for FixRows {
    fn default() -> Self {
        Self {
            puller: None,
            polled: None,
            poll_every: POLL_EVERY,
        }
    }
}

impl ProjectLoop {
    /// Takes the fix requests the store lists for this project, when this computer is its runner and nothing holds the
    /// rows back. `paused` is this computer's or the store's pause; `work` counts the rows running on this computer.
    pub(super) async fn fix_pass(
        &mut self,
        watch: &Arc<dyn CiWatchHost>,
        runner: Arc<AtomicBool>,
        paused: bool,
        work: &Work,
        shared: &Shared<'_>,
    ) {
        if shared.stopping || !runner.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let Some(repo) = self.repo else { return };
        if self.fixes.polled.is_some_and(|at| at.elapsed() < self.fixes.poll_every) {
            return;
        }
        let token = match shared.links.store_token().await {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        self.fixes.polled = Some(Instant::now());
        let link: Arc<dyn IntentStore> = Arc::new(StoreLink {
            client: self.client.clone(),
            repo,
            token: token.clone(),
            machine: shared.links.machine.clone(),
            failures: self.failures.clone(),
        });
        let gates = Gates {
            paused,
            signed_out: signed_out(shared),
            out_of_time: super::signed_out::out_of_time(shared),
            report: false,
            work: work.clone(),
        };
        let items = self.owned_branches();
        let handler = Arc::new(FixHandler::new(runner.clone()));
        let puller = self
            .fixes
            .puller
            .get_or_insert_with(|| Puller::default().keeping(self.store.dir()).register(handler));
        // Built when a row is won: what the watcher looks at then, held on to by the fix that runs on.
        let (project, client) = (self.project.clone(), self.client.clone());
        let (fixing, logins) = (self.ci_watch.fixing.clone(), self.ci_watch.login.clone());
        let cx = move || -> FixCx {
            let judge = Arc::new(ProjectJudge {
                host: watch.clone(),
                project: project.clone(),
            });
            let bells = watch.clone();
            Some(Arc::new(Presser(Arc::new(Press {
                host: watch.code_host(&project),
                judge: judge.clone(),
                fixer: judge,
                client,
                repo,
                token,
                items,
                allow: project.ci_watch_allow.clone(),
                logins,
                bell: Arc::new(move |title, body| bells.bell(title, body)),
                fixing,
                runner,
            }))))
        };
        puller.pass_gated(&link, &gates, cx).await;
    }
}
