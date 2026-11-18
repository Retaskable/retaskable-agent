//! The store's merge turn for a ship (`merge_requested { ship: true }`, `merge_item.rs`): so no piece from any
//! computer lands on the dev branch while the pull request into main is squashed in and the dev branch restarted
//! from main (Retaskable/Retaskable.com#3852; docs/specs/agent/06.6-ask-claude.md).
//!
//! No store change was needed: a chore entry is a turn. The ship queues `ship-<base>` as a chore that holds its
//! branch (so only this computer is ever handed it), asks `merge_next` without `piece: true` (with it the store
//! answers `waiting`) and, with the turn, beats it before and after the re-read and before `gh pr merge`. It never
//! waits in line: a line-holder with no piece at the front would hold every piece's merge up, so a turn that is
//! not free at once is withdrawn and refused in words. The turn is always given back as `closed` (never `released`,
//! which would requeue the chore) and the entry withdrawn.
//!
//! The store's rollout repair freeze (`mayMerge`) lets a chore through, so a ship is not held up by one. Two projects
//! on one repo and base would share an agent id (one per computer), but the loops run one after the other in one task
//! and a ship holds its turn only inside its own pass, so they never interleave.
use std::sync::Arc;

use super::Shared;
use super::merge_item::MergeHost;
use super::project::ProjectLoop;
use crate::backlog::merge::{MergeEntry, MergeNext, MergeOutcome, MergePr};
use crate::backlog::{BacklogError, Item};
use crate::config::ProjectConfig;
use crate::team_branch::ship::{Shipped, Shipping, Stop};

/// The id a ship's turn is queued under: a chore's own id, never an item of the store.
pub(super) fn ship_item(base: &str) -> String {
    format!("ship-{base}")
}

/// Whether a turn's item is a ship's, which no piece run can take.
pub(super) fn is_ship(item: &str) -> bool {
    item.starts_with("ship-")
}

fn busy(base: &str, ahead: Option<&str>) -> String {
    let ahead = ahead.map(|a| format!(" (waiting on {a})")).unwrap_or_default();
    format!("A piece is being merged into {base}{ahead}. Try again when it has landed.")
}

const PAUSED: &str = "The project is paused, so the merge turn can't be taken.";
const LOST: &str = "The merge turn was lost, so nothing was merged.";

impl ProjectLoop {
    /// Withdraws this base's ship entry, which also takes a turn it holds back at once. Errors are the caller's to read.
    async fn withdraw_ship(&mut self, repo: u64, token: &str) -> Result<(), BacklogError> {
        let item = ship_item(&self.project.base_branch);
        self.client.merge_withdraw(repo, Some(token), &item).await
    }

    /// After a restart: a ship's entry or turn left by a stop is withdrawn, once, before any ship or piece merge asks.
    pub(super) async fn sweep_ship_turn(&mut self, shared: &Shared<'_>) {
        if self.merging.swept {
            return;
        }
        let (Some(repo), Ok(Some(token))) = (self.repo, shared.links.store_token().await) else {
            return;
        };
        match self.withdraw_ship(repo, &token).await {
            Ok(()) => self.merging.swept = true,
            Err(e) => self.store_error("merge_withdraw", &e),
        }
    }

    /// The whole ship with the store's turn held across it, under the sync lock the caller holds. Fails closed: with
    /// the store out of reach or the turn not free, nothing is merged.
    pub(super) async fn ship_under_turn(
        &mut self,
        host: &Arc<dyn MergeHost>,
        shared: &Shared<'_>,
        main: &str,
        items: Vec<Item>,
    ) -> Result<Shipped, Stop> {
        let Some(repo) = self.repo else {
            return Err(Stop::Refused(
                "The shared backlog isn't reachable from here yet. Try again in a minute.".into(),
            ));
        };
        let token = match shared.links.store_token().await {
            Ok(Some(token)) => token,
            _ => {
                return Err(Stop::Refused(
                    "This computer isn't signed in to the shared backlog.".into(),
                ));
            }
        };
        let (base, item) = (self.project.base_branch.clone(), ship_item(&self.project.base_branch));
        let store_down =
            |e: &BacklogError| Stop::Failed(anyhow::anyhow!("Couldn't ask the store for the merge turn: {e}"));
        // A leftover of an earlier ship goes first, so it can't be the front of the line.
        if let Err(e) = self.withdraw_ship(repo, &token).await {
            self.store_error("merge_withdraw", &e);
            return Err(store_down(&e));
        }
        let shipping = Self::read_ship(self.project.clone(), host, main, &items).await?;
        let entry = MergeEntry {
            item: &item,
            piece: None,
            base: &base,
            head: &shipping.head,
            approved_sha: None,
            pr: MergePr {
                number: shipping.number,
                url: &shipping.url,
            },
            land: "retaskable",
            chore: true,
            holds_branch: true,
        };
        if let Err(e) = self.client.merge_enqueue(repo, Some(&token), &entry).await {
            self.store_error("merge_enqueue", &e);
            self.give_up_ship(repo, &token, None).await;
            return Err(store_down(&e));
        }
        let turn = match self.client.merge_next(repo, Some(&token), &base, false).await {
            Ok(MergeNext::Turn(turn)) => turn,
            Ok(MergeNext::Waiting { ahead, .. }) => {
                self.give_up_ship(repo, &token, None).await;
                return Err(Stop::Refused(busy(&base, ahead.as_deref())));
            }
            Ok(MergeNext::Nothing) => {
                self.give_up_ship(repo, &token, None).await;
                return Err(Stop::Refused(PAUSED.into()));
            }
            Err(e) => {
                self.store_error("merge_next", &e);
                self.give_up_ship(repo, &token, None).await;
                return Err(store_down(&e));
            }
        };
        let turn = turn.token;
        self.merging.ship_turn = Some(turn.clone());
        let shipped = self.ship_with_turn(host, repo, &token, &turn, main, &items).await;
        let outcome = match &shipped {
            Ok(Shipped {
                merged: true,
                merge_commit: Some(commit),
                ..
            }) => MergeOutcome::Merged {
                merge_commit: Some(commit.clone()),
            },
            _ => MergeOutcome::Closed,
        };
        self.give_up_ship(repo, &token, Some((&turn, outcome))).await;
        self.merging.ship_turn = None;
        shipped
    }

    /// The beats and the git and `gh` work, with the turn held. Whatever comes back, the caller gives the turn back.
    async fn ship_with_turn(
        &mut self,
        host: &Arc<dyn MergeHost>,
        repo: u64,
        token: &str,
        turn: &str,
        main: &str,
        items: &[Item],
    ) -> Result<Shipped, Stop> {
        self.beat_ship(repo, token, turn, "updating").await?;
        // The branch as it stands with the turn held: nothing lands on it from here to the merge.
        let shipping = Self::read_ship(self.project.clone(), host, main, items).await?;
        self.beat_ship(repo, token, turn, "merging").await?;
        let (project, main, host) = (self.project.clone(), main.to_string(), host.clone());
        match tokio::task::spawn_blocking(move || host.ship_merge(&project, &main, &shipping)).await {
            Ok(done) => done,
            Err(e) => Err(Stop::Failed(anyhow::anyhow!("{e}"))),
        }
    }

    /// `merge_beat`: the turn must still be ours before the next step, which can't be undone once it is a merge.
    async fn beat_ship(&mut self, repo: u64, token: &str, turn: &str, phase: &str) -> Result<(), Stop> {
        match self.client.merge_beat(repo, Some(token), turn, phase).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(Stop::Refused(LOST.into())),
            Err(e) => {
                self.store_error("merge_beat", &e);
                Err(Stop::Failed(anyhow::anyhow!("Couldn't keep the merge turn: {e}")))
            }
        }
    }

    /// Reads the pull request on a blocking thread (git and `gh` wait).
    async fn read_ship(
        project: ProjectConfig,
        host: &Arc<dyn MergeHost>,
        main: &str,
        items: &[Item],
    ) -> Result<Shipping, Stop> {
        let (host, main, items) = (host.clone(), main.to_string(), items.to_vec());
        match tokio::task::spawn_blocking(move || host.ship_read(&project, &main, &items)).await {
            Ok(read) => read,
            Err(e) => Err(Stop::Failed(anyhow::anyhow!("{e}"))),
        }
    }

    /// Gives the turn back, if one was taken (`closed` or `merged`, never `released`), and withdraws the entry as a
    /// backstop, on every path.
    async fn give_up_ship(&mut self, repo: u64, token: &str, turn: Option<(&str, MergeOutcome)>) {
        if let Some((turn, outcome)) = turn
            && let Err(e) = self.client.merge_result(repo, Some(token), turn, &outcome).await
        {
            self.store_error("merge_result", &e);
        }
        if let Err(e) = self.withdraw_ship(repo, token).await {
            self.store_error("merge_withdraw", &e);
        }
    }
}
