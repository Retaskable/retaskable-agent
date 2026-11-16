//! Merge into main for an item (docs/specs/intent-rows.md section 4, `merge_requested { itemId }`; `mergePr` and
//! `mergeForPerson` in the TS agent): a person presses it on an item whose finished pull request waits for them. The
//! claim by name moves the item to this computer, `gh pr merge --squash` lands the pull request in the protected
//! branch (the head branch is deleted only when it is the slice's own), and `complete` tells the store, which counts
//! the slice. The TS agent merges directly here, not through the store's merge queue (`merge_enqueue`, which the
//! website's spec words), and so does this. The intent puller claims the row and the handler hands the item's id to
//! this loop (`intents/merge_item.rs`), or, for a spec draft, queues the draft's press with the row's answer on it, and
//! the draft landing (`land.rs`) answers; the loop answers an item on its next pass, because it alone knows what the item is
//! doing. A paused project still merges (a person asked), and no session slot is used.
//!
//! A row naming `{ ship: true }` is the other half: the merge control into main for a team branch (`ship_to_main`,
//! `team_branch/ship.rs`). It claims nothing; the project's own pull request is merged and the dev branch started again.
//!
//! The completion is what the TS agent sends for an item taken over from another computer: the implementer's own
//! `implDone` and `implRemaining` are kept only in its local record (`shared/itemDetails.ts`), so this computer has
//! neither, and it reports the slice as merged with what the store says is left (`built` false, no merge commit). The
//! store builds the item or puts it back in the queue from that. The item's suggested playtest, which the store does
//! keep, goes with it.
//!
//! Where the TS agent leaves an item it can't merge held in its wait, this agent has no such stop to hold it in (it
//! never records a person's merge wait), so the claim is undone with the store's `park` for what the item waited for,
//! never `release`, which would send a finished change back to `todo`. Only an item that wasn't waiting is released.
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::sync::mpsc;

use super::Shared;
use super::drafts::claim_agent;
use super::main_sync::SyncLock;
use super::outbox::Sent;
use super::project::ProjectLoop;
use crate::backlog::Item;
use crate::backlog::complete::{Completion, Playtest};
use crate::backlog::take_over::Did;
use crate::config::ProjectConfig;
use crate::drafts::land::{deletes_head, merge_pr, pr_head, pr_number, workflow};
use crate::env::Env;
use crate::intents::main_pr::Why;
use crate::intents::merge_item::{Answer, Merge, MergeHandler, Ship, ShipAnswer};
use crate::intents::{IntentStore, Puller, StoreLink};
use crate::item_lifecycle::ItemState;
use crate::team_branch::ship::{Shipped, Shipping, Stop, merge_for_ship, read_for_ship};
use crate::team_branch::{gh_reason, held_refusal};
use crate::view::{DraftCommand, Reply};

/// How often the store is asked for merge rows (docs/specs/intent-rows.md section 3.3).
const POLL_EVERY: Duration = Duration::from_secs(60);

const NOT_WAITING: &str = "It isn't waiting for you to merge it.";
const BUSY: &str = "A step is running for this item; try again in a moment.";

/// What merging needs from the code host. Faked in tests.
pub trait MergeHost: Send + Sync {
    /// The number of the open pull request for `branch`, if there is one.
    fn open_pr(&self, project: &ProjectConfig, branch: &str) -> Result<Option<u64>>;
    /// The head commit of pull request `pr` now.
    fn head(&self, project: &ProjectConfig, pr: u64) -> Result<String>;
    /// `gh pr merge <pr> --squash --match-head-commit <head>`, deleting the head branch when `delete_branch`.
    fn merge(&self, project: &ProjectConfig, pr: u64, delete_branch: bool, head: &str) -> Result<()>;
    /// The merge control into main, first half: reads the dev branch's pull request into `main` and refuses what can't
    /// go (`team_branch::ship::read_for_ship`). Changes nothing. `items` are the store's.
    fn ship_read(&self, project: &ProjectConfig, main: &str, items: &[Item]) -> Result<Shipping, Stop>;
    /// Its second half, with the store's merge turn held: squashes the pull request in as read and starts the dev
    /// branch again from `main` (`team_branch::ship::merge_for_ship`).
    fn ship_merge(&self, project: &ProjectConfig, main: &str, shipping: &Shipping) -> Result<Shipped, Stop>;
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

    fn head(&self, project: &ProjectConfig, pr: u64) -> Result<String> {
        pr_head(&self.gh(), std::path::Path::new(&project.repo_path), pr)
    }

    fn merge(&self, project: &ProjectConfig, pr: u64, delete_branch: bool, head: &str) -> Result<()> {
        merge_pr(
            &self.gh(),
            std::path::Path::new(&project.repo_path),
            pr,
            "--squash",
            delete_branch,
            Some(head),
        )
    }

    fn ship_read(&self, p: &ProjectConfig, main: &str, items: &[Item]) -> Result<Shipping, Stop> {
        let dir = std::path::Path::new(&p.repo_path);
        read_for_ship(&self.gh(), dir, &p.github_repo, &p.base_branch, main, items)
    }

    fn ship_merge(&self, p: &ProjectConfig, main: &str, shipping: &Shipping) -> Result<Shipped, Stop> {
        let dir = std::path::Path::new(&p.repo_path);
        merge_for_ship(&self.gh(), dir, &p.github_repo, &p.base_branch, main, shipping)
    }
}

/// A project's merge requests: the channel its handler writes to, the puller (built the first time a pass runs) and
/// when the store was last asked.
pub(super) struct Merging {
    asks: mpsc::UnboundedSender<Merge>,
    waiting: mpsc::UnboundedReceiver<Merge>,
    ships: mpsc::UnboundedSender<Ship>,
    ship_waiting: mpsc::UnboundedReceiver<Ship>,
    puller: Option<Puller<()>>,
    /// The token of the store's merge turn a ship holds right now (`ship_turn.rs`): no piece's merge is asked for meanwhile.
    pub(super) ship_turn: Option<String>,
    /// A ship's leftover entry was withdrawn after this start (`ship_turn.rs`).
    pub(super) swept: bool,
    polled: Option<Instant>,
    /// How long after one look at the store the next may be.
    pub(super) poll_every: Duration,
}

impl Default for Merging {
    fn default() -> Self {
        let (asks, waiting) = mpsc::unbounded_channel();
        let (ships, ship_waiting) = mpsc::unbounded_channel();
        Self {
            asks,
            waiting,
            ships,
            ship_waiting,
            puller: None,
            ship_turn: None,
            swept: false,
            polled: None,
            poll_every: POLL_EVERY,
        }
    }
}

impl ProjectLoop {
    /// Answers the merge requests the puller handed over, then looks for new rows.
    pub(super) async fn merge_pass(&mut self, host: &Arc<dyn MergeHost>, shared: &Shared<'_>) {
        while let Ok(ask) = self.merging.waiting.try_recv() {
            if ask.draft {
                // A spec draft is landed by the draft path (`land.rs`), which answers the row when it is done.
                let (project, draft, reply) = (self.id.clone(), ask.id, Reply::new(ask.reply));
                let press = DraftCommand::MergeFor { project, draft, reply };
                shared.links.item_commands.push_draft(press);
                continue;
            }
            let answer = self.merge_item(&ask.id, host, shared).await;
            let _ = ask.reply.send(answer);
        }
        while let Ok(ask) = self.merging.ship_waiting.try_recv() {
            let answer = self.ship_to_main(host, shared).await;
            let _ = ask.reply.send(answer);
        }
        if shared.stopping {
            return;
        }
        self.sweep_ship_turn(shared).await;
        let Some(repo) = self.repo else { return };
        if self
            .merging
            .polled
            .is_some_and(|at| at.elapsed() < self.merging.poll_every)
        {
            return;
        }
        let token = match shared.links.store_token().await {
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
        let mut handler = MergeHandler::new(self.merging.asks.clone(), self.merging.ships.clone());
        if self.mode != super::OrchestratorMode::Claim {
            // No item is held here, so one a person merges is left for a computer that claims.
            handler = handler.without_items();
        }
        let handler = Arc::new(handler);
        let puller = self
            .merging
            .puller
            .get_or_insert_with(|| Puller::default().register(handler));
        puller.pass(&link, || ()).await;
    }

    /// The merge control into main: refuses or merges the dev branch's pull request and starts the dev branch again,
    /// on a blocking thread (git and `gh` both wait). No claim and no item: it is the project's own pull request.
    async fn ship_to_main(&mut self, host: &Arc<dyn MergeHost>, shared: &Shared<'_>) -> ShipAnswer {
        let (main, items) = self.team_request(shared).await?;
        let base = self.project.base_branch.clone();
        // What the store's items can't show in runs mode: a piece merging into the dev branch is a merge turn this
        // loop holds, and a main sync is a run here or the store's `sync_main-<base>` lock (`held_in` sees only items).
        // Squashing then resetting the dev branch across either would lose or strand what lands (`teamBranchActions.ts:88`).
        let lock = self.hold_sync_lock(shared).await?;
        let busy = matches!(lock, SyncLock::Busy);
        if let Some(why) = held_refusal(busy, self.run_loop.runs().merging(), &base, &main) {
            if let SyncLock::Mine(lock) = &lock {
                self.release_sync_lock(shared, lock).await;
            }
            return Err(Why::Refused(why));
        }
        let SyncLock::Mine(lock) = lock else {
            unreachable!("busy was refused above")
        };
        // The store's merge turn for the dev branch too, so no piece from any computer lands on it meanwhile.
        let shipped = self.ship_under_turn(host, shared, &main, items).await;
        self.release_sync_lock(shared, &lock).await;
        match shipped {
            Ok(done) => {
                tracing::info!("{}: shipped the dev branch into main ({done:?})", self.id);
                Ok(done.warning)
            }
            Err(Stop::Refused(why)) => Err(Why::Refused(why)),
            Err(Stop::Failed(e)) => Err(Why::Failed(format!("{e:#}"))),
        }
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
        let token = match shared.links.store_token().await {
            Ok(Some(token)) => token,
            _ => return Err("This computer isn't signed in to the shared backlog.".into()),
        };
        let mut agent = claim_agent(shared.links, &self.project);
        agent.week = self.week_use();
        let claim = self
            .client
            .take_over(repo, Some(&token), id, &agent, Did::Merged)
            .await?;
        let (item, lease, was) = (claim.item, claim.token, claim.was_waiting);
        // Only a finished change that waits for a person's merge is merged (`canMergeItem`); anything else is put
        // back as it was (`put_back`).
        let (waiting, url) = (was.as_deref() == Some("merge"), item.pr_url.clone());
        let Some(url) = url.filter(|_| waiting) else {
            self.put_back(repo, &token, id, &lease, was.as_deref()).await;
            return Err(NOT_WAITING.into());
        };
        let project = self.project.clone();
        let branch = item.branch.clone().unwrap_or_default();
        let found = match pr_number(&url) {
            Some(n) => Some(n),
            None if branch.is_empty() => None,
            None => {
                let (host, project, branch) = (host.clone(), project.clone(), branch.clone());
                let listed = tokio::task::spawn_blocking(move || host.open_pr(&project, &branch)).await;
                match listed {
                    Ok(Ok(found)) => found,
                    // A `gh` that couldn't answer isn't a pull request that's gone: say what it said.
                    Ok(Err(e)) => {
                        self.put_back(repo, &token, id, &lease, was.as_deref()).await;
                        return Err(format!("Couldn't look for this pull request: {}", gh_reason(&e)));
                    }
                    Err(e) => {
                        self.put_back(repo, &token, id, &lease, was.as_deref()).await;
                        return Err(format!("Couldn't look for this pull request: {e}"));
                    }
                }
            }
        };
        let Some(pr) = found else {
            self.put_back(repo, &token, id, &lease, was.as_deref()).await;
            return Err(
                "Lost track of this pull request: no PR number on record, and none found open for its branch.".into(),
            );
        };
        // Read before the merge, which can't be undone: with no answer the playtest would be lost, so ask twice and
        // then leave the item as it was. The TS agent merges first and records the playtest if it can.
        let mut record = self.client.details_get(repo, Some(&token), id).await;
        if record.is_err() {
            record = self.client.details_get(repo, Some(&token), id).await;
        }
        let playtest = match record {
            Ok(record) => record
                .and_then(|r| r.details.get("implPlaytest").cloned())
                .and_then(|p| serde_json::from_value::<Playtest>(p).ok()),
            Err(e) => {
                self.store_error("details_get", &e);
                self.put_back(repo, &token, id, &lease, was.as_deref()).await;
                return Err(format!(
                    "Couldn't read this item's playtest from the shared backlog, so nothing was merged: {e}"
                ));
            }
        };
        // Merged as the head stands now, so a push after this read is refused instead of landing unreviewed
        // (`mergeForPerson` passes no head in the TS agent, `merge.ts:318`).
        let (merging, delete) = (host.clone(), deletes_head(&branch));
        let merged = {
            let project = project.clone();
            tokio::task::spawn_blocking(move || {
                let head = merging.head(&project, pr)?;
                merging.merge(&project, pr, delete, &head)
            })
            .await
        };
        match merged {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                self.put_back(repo, &token, id, &lease, was.as_deref()).await;
                return Err(format!("Couldn't merge #{pr}: {}", gh_reason(&e)));
            }
            Err(e) => {
                self.put_back(repo, &token, id, &lease, was.as_deref()).await;
                return Err(format!("Couldn't merge #{pr}: {e}"));
            }
        }
        let completion = Completion {
            built: false,
            merge_commit: None,
            on_default_branch: false,
            remaining: Some(item.remaining.clone().unwrap_or_default()),
            playtest,
        };
        // Listed and saved first: the merge can't be undone, so a store that doesn't answer keeps the call, which the loop
        // sends again on later passes, also after a restart (`outbox.rs`).
        if let Sent::Kept(why) | Sent::Dropped(why) = self.complete_item(repo, &token, id, &lease, completion).await {
            return Err(format!(
                "Merged #{pr}, but the shared backlog didn't take it: {why}. It is sent again on the next pass."
            ));
        }
        tracing::info!("{}: merged #{pr} for {id}", self.id);
        Ok(())
    }

    /// Puts an item the claim took back as it was, for a merge that can't be carried out. The store's `release` sends
    /// an item back to `todo`, which would requeue a finished change or lose a help or CI wait, so an item that was
    /// waiting is parked again for what it waited for (the TS agent keeps it held in its wait). Only one that wasn't
    /// waiting, which the TS agent releases too, is released.
    async fn put_back(&mut self, repo: u64, token: &str, id: &str, lease: &str, was_waiting: Option<&str>) {
        let (op, put) = match was_waiting {
            Some(waits_for) => ("park", self.client.park(repo, Some(token), id, lease, waits_for).await),
            None => ("release", self.client.release(repo, Some(token), id, lease).await),
        };
        if let Err(e) = put {
            tracing::warn!("{}: putting {id} back ({op}) failed: {e}", self.id);
            self.store_error(op, &e);
        }
    }
}
