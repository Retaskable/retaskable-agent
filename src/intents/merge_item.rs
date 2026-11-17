//! The `merge_requested` kind for an item (docs/specs/intent-rows.md section 4; `merge_pr` in the TS agent's
//! `commands.ts` and `mergePr` in its `orchestratorActions.ts`): a person's **Merge into main** on an item whose
//! finished pull request waits for them. The handler only checks what the row names and asks the project's loop, which
//! knows the workflow, claims the item by name and runs `gh pr merge` (`driver/merge_item.rs`); the row ends `done`
//! with no result, or `refused` in plain words.
//!
//! The TS agent takes `{ itemId?, draftId?, ship? }` and refuses unless exactly one is named, and so does this. An item
//! or a spec draft (`draftId`, which the loop hands to its draft landing, `driver/land.rs`) ends `done` with no result.
//! `{ ship: true }` is the merge control into main: the loop squashes the dev branch's pull request into main and starts
//! the dev branch again (`driver/merge_item.rs`, `team_branch/ship.rs`), and the row ends `done` with no result, or with
//! `{ says }` when it merged but the dev branch wasn't started again (or the merge was only queued): where the TS
//! agent's HTTP answer carried that warning to the strip, the row's result carries it here.
//!
//! A kind that must never run twice (`NOT_TWICE`): a merge this computer began before a restart is failed, not run
//! again (see [`Handler::not_twice`]).
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use tokio::sync::{mpsc, oneshot};

use super::main_pr::Why;
use super::{Handler, Outcome, dropped};
use crate::backlog::intents::IntentRow;

/// What the loop answers: `Ok` once the pull request merged and the item was completed, or why it wasn't.
pub type Answer = Result<(), String>;

/// What the loop answers a ship: the warning to show (it merged, but the dev branch wasn't started again, or the merge
/// was only queued), or none when all went well; or why not.
pub type ShipAnswer = Result<Option<String>, Why>;

/// A person's merge control into main, with where the loop sends its answer.
pub struct Ship {
    pub reply: oneshot::Sender<ShipAnswer>,
}

/// A person's Merge into main on item `id`, or on spec draft `id` when `draft`, with where the loop sends its answer.
pub struct Merge {
    pub id: String,
    pub draft: bool,
    pub reply: oneshot::Sender<Answer>,
}

/// Asks the loop to merge items. The loop holds the receiving end and answers on its next pass.
pub struct MergeHandler {
    asks: mpsc::UnboundedSender<Merge>,
    ships: mpsc::UnboundedSender<Ship>,
    /// Whether a row naming an item is taken. Off where no item is held (`runs` mode): the claim by name and the
    /// item's lease are a claiming computer's, so that row stays `requested` for one (a TS agent or `claim` mode).
    items: bool,
}

impl MergeHandler {
    pub fn new(asks: mpsc::UnboundedSender<Merge>, ships: mpsc::UnboundedSender<Ship>) -> Self {
        Self {
            asks,
            ships,
            items: true,
        }
    }

    /// Leaves the rows naming an item unclaimed; spec drafts and the merge control into main are still taken.
    pub fn without_items(mut self) -> Self {
        self.items = false;
        self
    }
}

/// Words the TS agent refuses a row naming none or several of the three with (`commands.ts`).
const NAME_ONE: &str = "Name one item, one draft, or the pull request into main to merge.";

/// What a row names: whether an item id and a draft id, and how many of the item id, draft id and `ship: true`.
fn named(row: &IntentRow) -> (bool, bool, usize) {
    let text = |key: &str| row.args[key].as_str().is_some_and(|s| !s.is_empty());
    let (item, draft, ship) = (text("itemId"), text("draftId"), row.args["ship"] == true);
    (item, draft, [item, draft, ship].iter().filter(|n| **n).count())
}

impl<C: Send + Sync> Handler<C> for MergeHandler {
    fn kind(&self) -> &'static str {
        "merge_requested"
    }

    fn takes(&self, row: &IntentRow) -> bool {
        self.items || !named(row).0
    }

    fn not_twice(&self) -> bool {
        true
    }

    fn run(&self, row: IntentRow, _cx: &C) -> BoxFuture<'static, Outcome> {
        let (item, draft, count) = named(&row);
        if count != 1 {
            return async { Outcome::Refused(NAME_ONE.into()) }.boxed();
        }
        if !(item || draft) {
            let ships = self.ships.clone();
            return async move {
                let (reply, answer) = oneshot::channel();
                if ships.send(Ship { reply }).is_err() {
                    return dropped();
                }
                match answer.await {
                    Ok(Ok(None)) => Outcome::Done(serde_json::Value::Null),
                    Ok(Ok(Some(says))) => Outcome::Done(serde_json::json!({ "says": says })),
                    Ok(Err(Why::Refused(why))) => Outcome::Refused(why),
                    Ok(Err(Why::Failed(why))) => Outcome::Failed(why),
                    // The loop dropped the reply (a restart, or a panic in its pass).
                    Err(_) => dropped(),
                }
            }
            .boxed();
        }
        let key = if item { "itemId" } else { "draftId" };
        let id = row.args[key].as_str().unwrap_or_default().to_string();
        let asks = self.asks.clone();
        async move {
            let (reply, answer) = oneshot::channel();
            if asks.send(Merge { id, draft, reply }).is_err() {
                return dropped();
            }
            match answer.await {
                Ok(Ok(())) => Outcome::Done(serde_json::Value::Null),
                Ok(Err(why)) => Outcome::Refused(why),
                // The loop dropped the reply (a restart, or a panic in its pass).
                Err(_) => dropped(),
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests;
