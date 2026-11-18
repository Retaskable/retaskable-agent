//! Carries out the assistant's checked actions (spec 06.6) through the same code, with the same checks, as the
//! command a person could send: [`crate::commands`]'s `play_lane`, `pause_computer`, `retry_item` and `skip_item`.
//! A `draft_spec` is refused, not carried out ([`NOT_FILED`]). Each action's result goes back with the answer.
use serde_json::{Value, json};

use super::model::Action;
use crate::backlog::{Client, Item};
use crate::commands;
use crate::view::AgentView;

/// What `perform` runs against.
pub struct Perform<'a> {
    pub view: &'a AgentView,
    pub project: &'a str,
    pub client: &'a Client,
    pub repo_id: u64,
    pub token: Option<&'a str>,
    /// The ask the actions came from.
    pub ask_id: &'a str,
    /// The ask's "Attached images…" block, empty when it has no images.
    pub images_block: &'a str,
    /// The backlog list: the items a `revises` may name.
    pub items: &'a [Item],
    /// Where a `draft_queue` the store refuses is kept, for the loop to log.
    pub failures: Option<&'a crate::backlog::failures::Failures>,
}

/// What a `draft_spec` the assistant still asks for comes to: nothing is filed.
pub const NOT_FILED: &str =
    "Not filed: a request is filed from your message as an idea, so send it again as one (\"Add …\").";

/// Runs each action in order; one result per action (`{ type, ok, reason? }`), then one `{ type: "dropped", ok: false, reason }` for each dropped action.
pub async fn perform(actions: &[Action], dropped: &[String], cx: &Perform<'_>) -> Vec<Value> {
    let mut results = Vec::new();
    for action in actions {
        let (kind, done) = run(action, cx).await;
        results.push(match done {
            Ok(Some(draft)) => json!({ "type": kind, "ok": true, "draftId": draft }),
            Ok(None) => json!({ "type": kind, "ok": true }),
            Err(reason) => json!({ "type": kind, "ok": false, "reason": reason }),
        });
    }
    results.extend(
        dropped
            .iter()
            .map(|r| json!({ "type": "dropped", "ok": false, "reason": r })),
    );
    results
}

async fn run(action: &Action, cx: &Perform<'_>) -> (&'static str, Result<Option<String>, String>) {
    let (view, project) = (cx.view, cx.project);
    let plain = |r: Result<(), String>| r.map(|()| None);
    match action {
        Action::Play { lane } => (
            "play",
            plain(commands::play_lane(view, project, true, Some(i64::from(*lane)))),
        ),
        Action::Stop { lane } => (
            "stop",
            plain(commands::play_lane(view, project, false, Some(i64::from(*lane)))),
        ),
        Action::Pause => {
            commands::pause_computer(view, project, "the chat");
            ("pause", Ok(None))
        }
        Action::Resume => {
            commands::resume_computer(view, project);
            ("resume", Ok(None))
        }
        Action::Retry { id } => ("retry", plain(commands::retry_item(view, project, id))),
        Action::Skip { id } => ("skip", plain(commands::skip_item(view, project, id))),
        // The assistant answers and acts; it files nothing. A request is filed on the board by the Chat Agent, never from here.
        Action::DraftSpec { .. } => ("draft_spec", Err(NOT_FILED.to_string())),
    }
}

#[cfg(test)]
mod tests;
