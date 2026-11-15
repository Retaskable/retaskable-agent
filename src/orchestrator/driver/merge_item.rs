//! Merge into main for an item (docs/specs/intent-rows.md section 4, `merge_requested { itemId }`; `mergePr` and
//! `mergeForPerson` in the TS agent): a person presses it on an item whose finished pull request waits for them. The
//! claim by name moves the item to this computer, `gh pr merge --squash` lands the pull request in the protected
//! branch (the head branch is deleted only when it is the slice's own), and `complete` tells the store, which counts
//! the slice. The TS agent merges directly here, not through the store's merge queue (`merge_enqueue`, which the
//! website's spec words), and so does this. The intent puller claims the row and the handler hands the item's id to
//! this loop (`intents/merge_item.rs`); the loop answers on its next pass, because it alone knows what the item is
//! doing. A paused project still merges (a person asked), and no session slot is used.
//!
//! The completion is what the TS agent sends for an item taken over from another computer: the implementer's own
//! `implDone` and `implRemaining` are kept only in its local record (`shared/itemDetails.ts`), so this computer has
//! neither, and it reports the slice as merged with what the store says is left (`built` false, no merge commit). The
//! store builds the item or puts it back in the queue from that. The item's suggested playtest, which the store does
//! keep, goes with it.
//!
//! Where the TS agent leaves an item it can't merge held as waiting for the person, this agent has no such stop to
//! hold it in (it never records a person's merge wait), so the claim is given back and the row refused.
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::sync::mpsc;

use super::Shared;
use super::drafts::claim_agent;
use super::project::ProjectLoop;
use crate::backlog::complete::{Completion, Playtest};
use crate::backlog::take_over::Did;
use crate::config::ProjectConfig;
use crate::drafts::land::{deletes_head, merge_pr, pr_number, workflow};
use crate::env::Env;
use crate::intents::merge_item::{Answer, Merge, MergeHandler};
use crate::intents::{IntentStore, Puller, StoreLink};
use crate::item_lifecycle::ItemState;

/// How often the store is asked for merge rows (docs/specs/intent-rows.md section 3.3).
const POLL_EVERY: Duration = Duration::from_secs(60);

const NOT_WAITING: &str = "It isn't waiting for you to merge it.";
const BUSY: &str = "A step is running for this item; try again in a moment.";

/// What merging needs from the code host. Faked in tests.
pub trait MergeHost: Send + Sync {
    /// The number of the open pull request for `branch`, if there is one.
    fn open_pr(&self, project: &ProjectConfig, branch: &str) -> Result<Option<u64>>;
    /// `gh pr merge <pr> --squash`, deleting the head branch when `delete_branch`.
    fn merge(&self, project: &ProjectConfig, pr: u64, delete_branch: bool) -> Result<()>;
}

/// The real thing: `gh`, or `RETASKABLE_GH`, run in the project's checkout.
pub struct GhMergeHost {
    pub env: Env,
}

impl GhMergeHost {
    fn gh(&self) -> std::path::PathBuf {
        self.env
            .get("RETASKABLE_GH")
            .map_or_else(|| "gh".into(), std::path::PathBuf::from)
    }
}

impl MergeHost for GhMergeHost {
    fn open_pr(&self, project: &ProjectConfig, branch: &str) -> Result<Option<u64>> {
        #[derive(Deserialize)]
        struct Listed {
            number: u64,
        }
        let args = ["pr", "list", "--head", branch, "--state", "open", "--json", "number"];
        let dir = std::path::Path::new(&project.repo_path);
        crate::host_call::guard("gh", dir, &args)?;
        let mut list = std::process::Command::new(self.gh());
        list.current_dir(dir).args(args);
        let listed = crate::implementer::publish::run(list, None, "gh pr list")?;
        let open: Vec<Listed> = serde_json::from_str(listed.trim()).context("reading gh pr list's answer")?;
        Ok(open.first().map(|p| p.number))
    }

    fn merge(&self, project: &ProjectConfig, pr: u64, delete_branch: bool) -> Result<()> {
        merge_pr(
            &self.gh(),
            std::path::Path::new(&project.repo_path),
            pr,
            "--squash",
            delete_branch,
        )
    }
}

/// GitHub's own words from a failed `gh` call, without gh's GraphQL prefix, at most 300 characters (`ghReason`).
fn reason(err: &anyhow::Error) -> String {
    let text = format!("{err:#}");
    let after = text
        .rfind("GraphQL: ")
        .map_or(text.as_str(), |at| &text[at + "GraphQL: ".len()..]);
    after.trim().chars().take(300).collect()
}

/// A project's merge requests: the channel its handler writes to, the puller (built the first time a pass runs) and
/// when the store was last asked.
pub(super) struct Merging {
    asks: mpsc::UnboundedSender<Merge>,
    waiting: mpsc::UnboundedReceiver<Merge>,
    puller: Option<Puller<()>>,
    polled: Option<Instant>,
    /// How long after one look at the store the next may be.
    pub(super) poll_every: Duration,
}

impl Default for Merging {
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
    /// Answers the merge requests the puller handed over, then looks for new rows.
    pub(super) async fn merge_pass(&mut self, host: &Arc<dyn MergeHost>, shared: &Shared<'_>) {
        while let Ok(ask) = self.merging.waiting.try_recv() {
            let answer = self.merge_item(&ask.id, host, shared).await;
            let _ = ask.reply.send(answer);
        }
        if shared.stopping {
            return;
        }
        let Some(repo) = self.repo else { return };
        if self
            .merging
            .polled
            .is_some_and(|at| at.elapsed() < self.merging.poll_every)
        {
            return;
        }
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        self.merging.polled = Some(Instant::now());
        let link: Arc<dyn IntentStore> = Arc::new(StoreLink {
            client: self.client.clone(),
            repo,
            token,
            machine: shared.links.machine.clone(),
            failures: self.failures.clone(),
        });
        let handler = Arc::new(MergeHandler::new(self.merging.asks.clone()));
        let puller = self
            .merging
            .puller
            .get_or_insert_with(|| Puller::default().register(handler));
        puller.pass(&link, || ()).await;
    }

    /// Claims item `id` by name, squash-merges its pull request and completes it, or says in words why it can't.
    async fn merge_item(&mut self, id: &str, host: &Arc<dyn MergeHost>, shared: &Shared<'_>) -> Answer {
        // Only a project whose person merges into its protected branch has a Merge into main (`mergeInto`).
        if workflow(&self.project).person_merges_into.is_none() {
            return Err(NOT_WAITING.into());
        }
        if let Some(run) = self.runs.get(id) {
            use ItemState::*;
            // Held here, so a step on it may be running; one parked or stopped here isn't waiting for a merge.
            return Err(match run.state {
                Queued | Implementing | AwaitingCi | Checking | Reviewing | Merging => BUSY,
                _ => NOT_WAITING,
            }
            .into());
        }
        let Some(repo) = self.repo else {
            return Err("The shared backlog isn't reachable from here yet. Try again in a minute.".into());
        };
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            _ => return Err("This computer isn't signed in to the shared backlog.".into()),
        };
        let mut agent = claim_agent(shared.links, &self.project);
        agent.week = self.week_use();
        let claim = self
            .client
            .take_over(repo, Some(&token), id, &agent, Did::Merged)
            .await?;
        let (item, lease) = (claim.item, claim.token);
        // Only a finished change that waits for a person's merge is merged (`canMergeItem`); anything else is given
        // straight back, as the store leaves nothing of its wait to carry on from.
        let (waiting, url) = (claim.was_waiting.as_deref() == Some("merge"), item.pr_url.clone());
        let Some(url) = url.filter(|_| waiting) else {
            self.let_go(repo, &token, id, &lease).await;
            return Err(NOT_WAITING.into());
        };
        let project = self.project.clone();
        let branch = item.branch.clone().unwrap_or_default();
        let found = match pr_number(&url) {
            Some(n) => Some(n),
            None if branch.is_empty() => None,
            None => {
                let (host, project, branch) = (host.clone(), project.clone(), branch.clone());
                tokio::task::spawn_blocking(move || host.open_pr(&project, &branch))
                    .await
                    .ok()
                    .and_then(|r| r.ok().flatten())
            }
        };
        let Some(pr) = found else {
            self.let_go(repo, &token, id, &lease).await;
            return Err(
                "Lost track of this pull request: no PR number on record, and none found open for its branch.".into(),
            );
        };
        let (merging, delete) = (host.clone(), deletes_head(&branch));
        let merged = {
            let project = project.clone();
            tokio::task::spawn_blocking(move || merging.merge(&project, pr, delete)).await
        };
        match merged {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                self.let_go(repo, &token, id, &lease).await;
                return Err(format!("Couldn't merge #{pr}: {}", reason(&e)));
            }
            Err(e) => {
                self.let_go(repo, &token, id, &lease).await;
                return Err(format!("Couldn't merge #{pr}: {e}"));
            }
        }
        let playtest = match self.client.details_get(repo, Some(&token), id).await {
            Ok(record) => record
                .and_then(|r| r.details.get("implPlaytest").cloned())
                .and_then(|p| serde_json::from_value::<Playtest>(p).ok()),
            Err(e) => {
                self.store_error("details_get", &e);
                None
            }
        };
        let completion = Completion {
            built: false,
            merge_commit: None,
            on_default_branch: false,
            remaining: Some(item.remaining.clone().unwrap_or_default()),
            playtest,
        };
        if let Err(e) = self.client.complete(repo, Some(&token), id, &lease, &completion).await {
            self.store_error("complete", &e);
            return Err(format!(
                "Merged #{pr}, but the shared backlog didn't take it: {e}. Its claim stays with this computer."
            ));
        }
        tracing::info!("{}: merged #{pr} for {id}", self.id);
        Ok(())
    }

    /// Lets go of a claim taken for a merge that can't be carried out.
    async fn let_go(&mut self, repo: u64, token: &str, id: &str, lease: &str) {
        if let Err(e) = self.client.release(repo, Some(token), id, lease).await {
            tracing::warn!("{}: giving {id} back failed: {e}", self.id);
            self.store_error("release", &e);
        }
    }
}
