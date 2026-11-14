//! The `take_over_requested` kind (docs/specs/intent-rows.md section 4; `takeOverItem` in the TS agent's
//! `orchestratorActions.ts`): a person ends the wait of an item whose owner's computer has been offline over 2 hours,
//! so it becomes this computer's and Continue, Start over and Skip work here. The store claims it for the person with
//! its own claim and refuses when the item moved or its owner was online lately; the handler turns a refusal into
//! words. The claimed item is handed to the project's loop, which makes it a run of its own (`take_overs.rs`).
//!
//! Only an item that waits for help is taken: a claim of one parked for CI, an answer, a try-out or a merge is given
//! back and refused, since this computer has nothing to carry those on from yet.
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::json;

use super::{Handler, Outcome};
use crate::backlog::Item;
use crate::backlog::intents::IntentRow;
use crate::backlog::take_over::PersonClaim;

/// What the store's claim by name is for this computer. Faked in tests.
pub trait Taker: Send + Sync {
    /// Claims item `id` as a person's take-over; a refusal is its words.
    fn take(&self, id: &str) -> BoxFuture<'static, Result<PersonClaim, String>>;
    /// Gives the lease `token` of item `id` back, for an item that turned out not to be waiting for help.
    fn give_back(&self, id: &str, token: &str) -> BoxFuture<'static, ()>;
}

/// An item this computer now holds, waiting for the loop to make it a run.
#[derive(Debug, Clone, PartialEq)]
pub struct Taken {
    pub item: Item,
    pub token: String,
}

/// What the handler and the loop share: the items this computer holds, and those just taken.
#[derive(Default)]
pub struct TakeOvers {
    held: Mutex<BTreeSet<String>>,
    taken: Mutex<Vec<Taken>>,
}

impl TakeOvers {
    fn held(&self) -> MutexGuard<'_, BTreeSet<String>> {
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn taken(&self) -> MutexGuard<'_, Vec<Taken>> {
        self.taken.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The loop says which items it holds now, so a take-over of one of them is refused without a claim.
    pub fn set_held(&self, ids: impl IntoIterator<Item = String>) {
        let mut held = self.held();
        *held = ids.into_iter().collect();
        held.extend(self.taken().iter().map(|t| t.item.id.clone()));
    }

    /// The items taken since the last call, each once.
    pub fn drain(&self) -> Vec<Taken> {
        std::mem::take(&mut *self.taken())
    }
}

pub struct TakeOverHandler {
    shared: Arc<TakeOvers>,
}

impl TakeOverHandler {
    pub fn new(shared: Arc<TakeOvers>) -> Self {
        Self { shared }
    }
}

fn item_id(row: &IntentRow) -> Option<&str> {
    row.args
        .get("itemId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// The puller's context is the store link of the moment, built when a row is won.
impl Handler<Arc<dyn Taker>> for TakeOverHandler {
    fn kind(&self) -> &'static str {
        "take_over_requested"
    }

    fn takes(&self, row: &IntentRow) -> bool {
        item_id(row).is_some()
    }

    fn run(&self, row: IntentRow, taker: &Arc<dyn Taker>) -> BoxFuture<'static, Outcome> {
        let (taker, shared) = (taker.clone(), self.shared.clone());
        async move {
            let Some(id) = item_id(&row) else {
                return Outcome::Refused("There's no item to take over.".into());
            };
            if shared.held().contains(id) {
                return Outcome::Refused("It's on this computer already.".into());
            }
            let claim = match taker.take(id).await {
                Ok(claim) => claim,
                Err(why) => return Outcome::Refused(why),
            };
            if claim.was_waiting.as_deref() != Some("help") {
                taker.give_back(id, &claim.token).await;
                return Outcome::Refused(
                    "It isn't waiting for help on another computer, so there's nothing to take over.".into(),
                );
            }
            shared.taken().push(Taken {
                item: claim.item,
                token: claim.token,
            });
            shared.held().insert(id.to_string());
            Outcome::Done(json!({ "itemId": id }))
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests;
