//! The `sync_requested` kind (docs/specs/intent-rows.md section 4; `sync_requested` in the TS agent's `intentPuller.ts`,
//! `syncNow` in its `teamBranchActions.ts`): a person's **Sync now**. With `args.dev` true it is the team branch's
//! main sync: the handler asks the project's loop to start one at once (`driver/main_sync.rs`), and the row ends `done`
//! with `{ id }`, the sync chore's item id, or `refused` in plain words, or `failed` when git or the store broke.
//! Without `dev` the TS agent pushes an Apps Script project's code to its target (`sync_push`); this agent has no such
//! storage, so the row is refused and nothing is asked of the loop.
//!
//! A kind that must never run twice (`NOT_TWICE`): a sync this computer began before a restart is failed, not started
//! again (see [`Handler::not_twice`]). It starts no work of a person's own, so a paused project still syncs.
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use super::main_pr::Why;
use super::{Handler, Outcome, dropped};
use crate::backlog::intents::IntentRow;

/// What `sync_push` is refused with here (the TS agent's own words for a project with no code storage).
pub const NOT_SYNCED: &str = "This project's code isn't synced anywhere else.";

/// What the loop answers: the sync chore's item id, or why not.
pub type Answer = Result<String, Why>;

/// A person's Sync now, with where the loop sends its answer.
pub struct SyncNow {
    pub reply: oneshot::Sender<Answer>,
}

/// Asks the loop to start the sync. The loop holds the receiving end and answers on its next pass.
pub struct SyncHandler {
    asks: mpsc::UnboundedSender<SyncNow>,
}

impl SyncHandler {
    pub fn new(asks: mpsc::UnboundedSender<SyncNow>) -> Self {
        Self { asks }
    }
}

impl<C: Send + Sync> Handler<C> for SyncHandler {
    fn kind(&self) -> &'static str {
        "sync_requested"
    }

    fn not_twice(&self) -> bool {
        true
    }

    fn run(&self, row: IntentRow, _cx: &C) -> BoxFuture<'static, Outcome> {
        let asks = self.asks.clone();
        async move {
            if row.args.get("dev").and_then(serde_json::Value::as_bool) != Some(true) {
                return Outcome::Refused(NOT_SYNCED.into());
            }
            let (reply, answer) = oneshot::channel();
            if asks.send(SyncNow { reply }).is_err() {
                return dropped();
            }
            match answer.await {
                Ok(Ok(id)) => Outcome::Done(json!({ "id": id })),
                Ok(Err(Why::Refused(why))) => Outcome::Refused(why),
                Ok(Err(Why::Failed(why))) => Outcome::Failed(why),
                // The loop dropped the reply (a restart, or a panic in its pass).
                Err(_) => dropped(),
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests;
