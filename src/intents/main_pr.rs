//! The `main_pr_requested` kind (docs/specs/intent-rows.md section 4; `main_pr` in the TS agent's `commands.ts` and
//! `openMainPr` in its `teamBranchActions.ts`): a person's **Open PR to main** on a project working on a team branch.
//! The handler only asks the project's loop, which reads the branch and opens the pull request from the base branch
//! into the default branch (`driver/main_pr.rs`); the row ends `done` with `{ url }`, `refused` in plain words or
//! `failed` when `gh` or git broke. It takes no arguments.
//!
//! A kind that must never run twice (`NOT_TWICE`): a pull request this computer began before a restart is failed, not
//! opened again (see [`Handler::not_twice`]). It starts no work, so a paused project still opens one.
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use super::{Handler, Outcome, dropped};
use crate::backlog::intents::IntentRow;

/// Why the loop didn't open it: it said no (`refused`), or something broke (`failed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    Refused(String),
    Failed(String),
}

/// What the loop answers: the new pull request's URL, or why not.
pub type Answer = Result<String, Why>;

/// A person's Open PR to main, with where the loop sends its answer.
pub struct OpenMainPr {
    pub reply: oneshot::Sender<Answer>,
}

/// Asks the loop to open the pull request. The loop holds the receiving end and answers on its next pass.
pub struct MainPrHandler {
    asks: mpsc::UnboundedSender<OpenMainPr>,
}

impl MainPrHandler {
    pub fn new(asks: mpsc::UnboundedSender<OpenMainPr>) -> Self {
        Self { asks }
    }
}

impl<C: Send + Sync> Handler<C> for MainPrHandler {
    fn kind(&self) -> &'static str {
        "main_pr_requested"
    }

    fn not_twice(&self) -> bool {
        true
    }

    fn run(&self, _row: IntentRow, _cx: &C) -> BoxFuture<'static, Outcome> {
        let asks = self.asks.clone();
        async move {
            let (reply, answer) = oneshot::channel();
            if asks.send(OpenMainPr { reply }).is_err() {
                return dropped();
            }
            match answer.await {
                Ok(Ok(url)) => Outcome::Done(json!({ "url": url })),
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
