//! The `design_skip_requested` and `design_draw_requested` kinds (docs/specs/agent/06.6-ask-claude.md; `draft_skip_design`
//! and `board_design_write` in the TS agent's `commands.ts`): a person's **Skip the design** and **Try again** /
//! **Change it...** on a plan waiting for its design review. The handler only checks what the row names and hands the press
//! to the project's loop, which runs it (`driver/design_press.rs`: the store's `draft_skip_design`, or a drawing in a
//! read-only session) and answers on its next pass; the row ends `done` with no result, or `refused` in the press's
//! words. A skip starts no work. A draw does and runs a session, so a paused project, a computer signed out of Claude
//! and a computer already running two such rows leave it open for another.
//!
//! The TS agent claimed these only on the computer that held the plan, because its design flag and board lived in that
//! computer's memory. Here the plan is the store's draft (`needsDesign`, `designNote`, and `draft_take` for the lease),
//! so any computer takes either row.
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use tokio::sync::{mpsc, oneshot};

use super::{Handler, Outcome, dropped};
use crate::backlog::intents::IntentRow;

/// The most a row's `why` holds.
const WHY_MAX: usize = 280;
/// The most of a person's change a drawing is told.
const NOTE_MAX: usize = 2000;

/// A person's press on a plan's design review, with where the loop sends its answer: `Ok`, or why not.
pub struct Press {
    pub draft: String,
    /// A draw's change (**Change it...**), cut to [`NOTE_MAX`]; none for a plain **Try again** and for a skip.
    pub note: Option<String>,
    /// A draw, else a skip.
    pub draw: bool,
    pub reply: oneshot::Sender<Result<(), String>>,
}

/// Asks the loop to run a press. One handler per kind: the skip and the draw differ in whether they start work.
pub struct DesignHandler {
    draw: bool,
    presses: mpsc::UnboundedSender<Press>,
}

impl DesignHandler {
    /// The handler of `design_skip_requested`.
    pub fn skip(presses: mpsc::UnboundedSender<Press>) -> Self {
        Self { draw: false, presses }
    }

    /// The handler of `design_draw_requested`.
    pub fn draw(presses: mpsc::UnboundedSender<Press>) -> Self {
        Self { draw: true, presses }
    }
}

impl<C: Send + Sync> Handler<C> for DesignHandler {
    fn kind(&self) -> &'static str {
        if self.draw {
            "design_draw_requested"
        } else {
            "design_skip_requested"
        }
    }

    fn starts_work(&self) -> bool {
        self.draw
    }

    fn runs_session(&self) -> bool {
        self.draw
    }

    fn run(&self, row: IntentRow, _cx: &C) -> BoxFuture<'static, Outcome> {
        let draft = row.args.get("draftId").and_then(|v| v.as_str()).unwrap_or("").trim();
        if draft.is_empty() || draft.contains(['/', '\\']) || draft.contains("..") {
            let what = <Self as Handler<C>>::kind(self);
            return async move { Outcome::Refused(format!("{what} needs the draft's id.")) }.boxed();
        }
        let note = row
            .args
            .get("note")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|n| self.draw && !n.is_empty())
            .map(|n| crate::text::truncate(n, NOTE_MAX));
        let (draft, draw, presses) = (draft.to_string(), self.draw, self.presses.clone());
        async move {
            let (reply, answer) = oneshot::channel();
            if presses
                .send(Press {
                    draft,
                    note,
                    draw,
                    reply,
                })
                .is_err()
            {
                return dropped();
            }
            match answer.await {
                Ok(Ok(())) => Outcome::Done(serde_json::Value::Null),
                Ok(Err(why)) => Outcome::Refused(crate::text::truncate(&why, WHY_MAX)),
                // The loop dropped the reply (a restart, or a panic in its pass).
                Err(_) => dropped(),
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests;
