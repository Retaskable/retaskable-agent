//! The `merge_requested` kind for an item (docs/specs/intent-rows.md section 4; `merge_pr` in the TS agent's
//! `commands.ts` and `mergePr` in its `orchestratorActions.ts`): a person's **Merge into main** on an item whose
//! finished pull request waits for them. The handler only checks what the row names and asks the project's loop, which
//! knows the workflow, claims the item by name and runs `gh pr merge` (`driver/merge_item.rs`); the row ends `done`
//! with no result, or `refused` in plain words.
//!
//! The TS agent takes `{ itemId?, draftId?, ship? }` and refuses unless exactly one is named. This slice runs only an
//! item: a row naming only a draft or only the pull request into main is left for the kinds that will run them, and one
//! naming more than one is refused here with the TS words.
//!
//! A kind that must never run twice (`NOT_TWICE`): a merge this computer began before a restart is failed, not run
//! again (see [`Handler::not_twice`]).
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use tokio::sync::{mpsc, oneshot};

use super::{Handler, Outcome};
use crate::backlog::intents::IntentRow;

/// What the loop answers: `Ok` once the pull request merged and the item was completed, or why it wasn't.
pub type Answer = Result<(), String>;

/// A person's Merge into main on item `id`, with where the loop sends its answer.
pub struct Merge {
    pub id: String,
    pub reply: oneshot::Sender<Answer>,
}

/// Asks the loop to merge items. The loop holds the receiving end and answers on its next pass.
pub struct MergeHandler {
    asks: mpsc::UnboundedSender<Merge>,
}

impl MergeHandler {
    pub fn new(asks: mpsc::UnboundedSender<Merge>) -> Self {
        Self { asks }
    }
}

/// Words the TS agent refuses a row naming none or several of the three with (`commands.ts`).
const NAME_ONE: &str = "Name one item, one draft, or the pull request into main to merge.";

/// What a row names: an item id, a draft id, `ship: true`.
fn named(row: &IntentRow) -> (bool, usize) {
    let text = |key: &str| row.args[key].as_str().is_some_and(|s| !s.is_empty());
    let (item, draft, ship) = (text("itemId"), text("draftId"), row.args["ship"] == true);
    (item, [item, draft, ship].iter().filter(|n| **n).count())
}

impl<C: Send + Sync> Handler<C> for MergeHandler {
    fn kind(&self) -> &'static str {
        "merge_requested"
    }

    /// Everything but a row naming only a draft or only the pull request into main, which later slices run.
    fn takes(&self, row: &IntentRow) -> bool {
        let (item, count) = named(row);
        item || count != 1
    }

    fn not_twice(&self) -> bool {
        true
    }

    fn run(&self, row: IntentRow, _cx: &C) -> BoxFuture<'static, Outcome> {
        let (item, count) = named(&row);
        if count != 1 || !item {
            return async { Outcome::Refused(NAME_ONE.into()) }.boxed();
        }
        let id = row.args["itemId"].as_str().unwrap_or_default().to_string();
        let asks = self.asks.clone();
        async move {
            let (reply, answer) = oneshot::channel();
            if asks.send(Merge { id, reply }).is_err() {
                return Outcome::Abandoned;
            }
            match answer.await {
                Ok(Ok(())) => Outcome::Done(serde_json::Value::Null),
                Ok(Err(why)) => Outcome::Refused(why),
                // The loop went away before it answered (a restart): the row, held and not twice, is failed then.
                Err(_) => Outcome::Abandoned,
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests;
