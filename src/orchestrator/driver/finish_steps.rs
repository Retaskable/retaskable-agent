//! Finish (docs/specs/intent-rows.md section 5, `finish_requested`): a person presses Done on an item that needs help
//! on this computer. With nothing on a branch to lose (no pull request, no pushed branch) the item is built at the
//! base branch's head, as "nothing left to build" makes it in the TS agent: the store gets the one `complete` write
//! with the base head as the merge commit, and the run, its record and its lease go. The intent puller claims the row
//! and the handler hands the item's id to this loop (`intents/finish_steps.rs`); the loop answers on its next pass,
//! because it alone knows what the item is doing.
//!
//! Not built yet, and refused in words rather than guessed: an item waiting for a person's steps (this agent never
//! records that stop), and a change the reviewer found right (its merge, as an approval makes it). An item with a
//! change or a branch is refused the same way the TS agent refuses it, so no change is ever closed from here.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::json;
use tokio::sync::mpsc;

use super::Shared;
use super::project::ProjectLoop;
use crate::backlog::complete::Completion;
use crate::intents::finish_steps::{Answer, Finish, FinishHandler};
use crate::intents::{IntentStore, Puller, StoreLink};
use crate::item_lifecycle::ItemState;

/// How often the store is asked for finish rows (docs/specs/intent-rows.md section 3.3).
const POLL_EVERY: Duration = Duration::from_secs(60);

/// What finishing needs from git and the code host. Faked in tests.
pub trait FinishHost: Send + Sync {
    /// The base branch's head on the remote, and whether the base branch is the project's default branch.
    fn base_head(&self, repo: PathBuf, github_repo: String, base: String)
    -> BoxFuture<'static, Result<(String, bool)>>;
}

/// The real thing: `git ls-remote`, and `gh` for the default branch.
pub struct GitFinishHost;

impl FinishHost for GitFinishHost {
    fn base_head(
        &self,
        repo: PathBuf,
        github_repo: String,
        base: String,
    ) -> BoxFuture<'static, Result<(String, bool)>> {
        async move {
            let remote = repo.clone();
            let branch = base.clone();
            let sha = tokio::task::spawn_blocking(move || -> Result<String> {
                let listed = std::process::Command::new("git")
                    .current_dir(&remote)
                    .args(["ls-remote", "origin", &format!("refs/heads/{branch}")])
                    .output()
                    .context("git ls-remote")?;
                anyhow::ensure!(listed.status.success(), "git ls-remote failed");
                String::from_utf8_lossy(&listed.stdout)
                    .split_whitespace()
                    .next()
                    .map(String::from)
                    .context("the base branch isn't on the remote")
            })
            .await??;
            let default = super::report::default_branch(&github_repo, &repo).await;
            Ok((sha, default.as_deref() == Some(base.as_str())))
        }
        .boxed()
    }
}

/// A project's finish requests: the channel its handler writes to, the puller (built the first time a pass runs) and
/// when the store was last asked.
pub(super) struct Finishing {
    asks: mpsc::UnboundedSender<Finish>,
    waiting: mpsc::UnboundedReceiver<Finish>,
    puller: Option<Puller<()>>,
    polled: Option<Instant>,
    /// How long after one look at the store the next may be.
    pub(super) poll_every: Duration,
}

impl Default for Finishing {
    fn default() -> Self {
        let (asks, waiting) = mpsc::unbounded_channel();
        Self {
            asks,
            waiting,
            puller: None,
            polled: None,
            poll_every: POLL_EVERY,
        }
    }
}

impl ProjectLoop {
    /// Answers the Done requests the puller handed over, then looks for new rows.
    pub(super) async fn finish_pass(&mut self, host: &Arc<dyn FinishHost>, shared: &Shared<'_>) {
        while let Ok(ask) = self.finishing.waiting.try_recv() {
            let answer = self.finish_item(&ask.id, host, shared).await;
            let _ = ask.reply.send(answer);
        }
        if shared.stopping {
            return;
        }
        let Some(repo) = self.repo else { return };
        if self
            .finishing
            .polled
            .is_some_and(|at| at.elapsed() < self.finishing.poll_every)
        {
            return;
        }
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        self.finishing.polled = Some(Instant::now());
        let link: Arc<dyn IntentStore> = Arc::new(StoreLink {
            client: self.client.clone(),
            repo,
            token,
            machine: shared.links.machine.clone(),
            failures: self.failures.clone(),
        });
        let handler = Arc::new(FinishHandler::new(self.finishing.asks.clone()));
        let puller = self
            .finishing
            .puller
            .get_or_insert_with(|| Puller::default().register(handler));
        puller.pass(&link, || ()).await;
    }

    /// Builds item `id` at the base branch's head, or says in words why it can't be.
    async fn finish_item(&mut self, id: &str, host: &Arc<dyn FinishHost>, shared: &Shared<'_>) -> Answer {
        let Some(run) = self.runs.get(id) else {
            return Err("It isn't on this computer.".into());
        };
        if run.state != ItemState::NeedsHuman {
            return Err("It isn't waiting for a person on this computer.".into());
        }
        let item = self.items.get(id).cloned();
        if item.is_some_and(|i| i.branch.is_some() || i.pr_url.is_some()) {
            return Err("This item isn't waiting for a person's steps, and has work on its branch.".into());
        }
        let Some(lease) = self.leases.get(id).cloned() else {
            return Err(
                "Nothing left to build, so mark it done: this computer holds no claim on it in the shared backlog."
                    .into(),
            );
        };
        let Some(repo) = self.repo else {
            return Err("The shared backlog isn't reachable from here yet. Try again in a minute.".into());
        };
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            _ => return Err("This computer isn't signed in to the shared backlog.".into()),
        };
        let base = self.project.base_branch.clone();
        let head = host
            .base_head(
                PathBuf::from(&self.project.repo_path),
                self.project.github_repo.clone(),
                base.clone(),
            )
            .await;
        let (sha, on_default) = match head {
            Ok(head) => head,
            Err(e) => {
                tracing::warn!("{}: finish {id}: {e:#}", self.id);
                return Err(format!("Nothing left to build, but {base}'s head couldn't be read."));
            }
        };
        let completion = Completion {
            built: true,
            merge_commit: Some(sha.clone()),
            on_default_branch: on_default,
            remaining: Some(String::new()),
            playtest: None,
        };
        if let Err(e) = self.client.complete(repo, Some(&token), id, &lease, &completion).await {
            self.store_error("complete", &e);
            return Err(format!("The shared backlog didn't take it: {e}"));
        }
        self.runs.remove(id);
        self.items.remove(id);
        self.leases.remove(id);
        self.records.remove(id);
        self.save_leases();
        tracing::info!("{}: finished {id} at {base} {sha}", self.id);
        Ok(json!({ "itemId": id, "sha": sha }))
    }
}
