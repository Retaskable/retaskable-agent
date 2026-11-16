//! The `draft_wanted` kind (docs/specs/intent-rows.md section 4; `intentCommand` in the TS agent's `intentPuller.ts`):
//! what the website writes for a person's **Build this** on an idea (`{ idea }`, the old `idea_accept` command) and for a
//! drafted request (`{ request, audience?, trial?, pictures }`, the old `draft_spec`).
//!
//! A row with `idea` is what `idea_accept` ran: the handler asks the project's loop, which queues the idea as a draft in
//! the shared backlog and answers the idea (`ideas::accept`); the row ends `done`, or `refused` with the reason in plain
//! words (the idea gone or already answered, the backlog out of reach). A row without `idea` is a `draft_spec`, which this
//! agent can't start yet: it is `refused`, never `failed`. The kind starts work and runs a session, so a paused project or
//! a computer signed out of Claude leaves the row open for another computer.
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use super::{Handler, Outcome, dropped};
use crate::backlog::intents::IntentRow;

/// Why a row without an `idea` is refused.
pub const NO_DRAFT_SPEC: &str = "This computer can't start a draft from a request yet. Ask again from a computer that can.";

/// A person's Build this, with where the loop sends its answer: `Err` is why the idea wasn't queued.
pub struct AcceptIdea {
    pub idea: String,
    pub by: String,
    pub reply: oneshot::Sender<Result<(), String>>,
}

/// Asks the loop to queue the idea's draft. The loop holds the receiving end and answers on its next pass.
pub struct DraftWantedHandler {
    asks: mpsc::UnboundedSender<AcceptIdea>,
}

impl DraftWantedHandler {
    pub fn new(asks: mpsc::UnboundedSender<AcceptIdea>) -> Self {
        Self { asks }
    }
}

impl<C: Send + Sync> Handler<C> for DraftWantedHandler {
    fn kind(&self) -> &'static str {
        "draft_wanted"
    }

    fn starts_work(&self) -> bool {
        true
    }

    fn runs_session(&self) -> bool {
        true
    }

    fn run(&self, row: IntentRow, _cx: &C) -> BoxFuture<'static, Outcome> {
        let asks = self.asks.clone();
        async move {
            let idea = row.args.get("idea").and_then(|v| v.as_str()).map(str::trim);
            let Some(idea) = idea.filter(|i| !i.is_empty()) else {
                return Outcome::Refused(NO_DRAFT_SPEC.into());
            };
            let (reply, answer) = oneshot::channel();
            let ask = AcceptIdea {
                idea: idea.to_string(),
                by: row.issued_by.clone().unwrap_or_default(),
                reply,
            };
            if asks.send(ask).is_err() {
                return dropped();
            }
            match answer.await {
                Ok(Ok(())) => Outcome::Done(json!({})),
                Ok(Err(why)) => Outcome::Refused(why),
                Err(_) => dropped(),
            }
        }
        .boxed()
    }
}
