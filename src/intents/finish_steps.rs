//! The `finish_requested` kind (docs/specs/intent-rows.md section 5; `finishSteps` in the TS agent's
//! `orchestratorActions.ts`): a person presses Done on an item that needs help with nothing on a branch to lose. The
//! item is built at the base branch's head, as "nothing left to build" makes it. The handler only asks the project's
//! loop, which owns the item (`driver/finish_steps.rs`), and ends the row with the answer: `done` with what the
//! store built, or `refused` in plain words.
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use super::{Handler, Outcome};
use crate::backlog::intents::IntentRow;

/// What the loop answers: the row's result, or why it isn't done.
pub type Answer = Result<Value, String>;

/// A person's Done on item `id`, with where the loop sends its answer.
pub struct Finish {
    pub id: String,
    pub reply: oneshot::Sender<Answer>,
}

/// Asks the loop to finish items. The loop holds the receiving end and answers on its next pass.
pub struct FinishHandler {
    asks: mpsc::UnboundedSender<Finish>,
}

impl FinishHandler {
    pub fn new(asks: mpsc::UnboundedSender<Finish>) -> Self {
        Self { asks }
    }
}

impl<C: Send + Sync> Handler<C> for FinishHandler {
    fn kind(&self) -> &'static str {
        "finish_requested"
    }

    fn takes(&self, row: &IntentRow) -> bool {
        row.args["itemId"].as_str().is_some_and(|id| !id.is_empty())
    }

    fn run(&self, row: IntentRow, _cx: &C) -> BoxFuture<'static, Outcome> {
        let id = row.args["itemId"].as_str().unwrap_or_default().to_string();
        let asks = self.asks.clone();
        async move {
            let (reply, answer) = oneshot::channel();
            if asks.send(Finish { id, reply }).is_err() {
                return Outcome::Abandoned;
            }
            match answer.await {
                Ok(Ok(result)) => Outcome::Done(result),
                Ok(Err(why)) => Outcome::Refused(why),
                // The loop went away before it answered (a restart): the store opens the row again.
                Err(_) => Outcome::Abandoned,
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests;
