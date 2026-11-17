//! The commands on one item the hub still pushes (`answer`; `exec` in the TS agent's `commands.ts`): checked here, queued for
//! the project's loop, which owns the item's lane and lease and carries them out.
use serde_json::{Value, json};

use super::{Command, PENDING, ack, attach, claim_check, nack};
use crate::view::{AgentView, ItemCommand};

/// `answer { itemId, answers: string[], pictureIds? }` (a person's answers to what the session asked): the item goes back
/// to `implementing` with the questions and answers as the implementer's message (`orchestrator::driver::answers`). Refused
/// outside `claim` mode, and for an item the loop holds in a state other than the one that waits for answers. The hub answers
/// it itself when a piece of the item waits for the answer, so this only ever sees an item the session asked about.
pub(super) fn answer(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let id = cmd
        .args
        .get("itemId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if id.is_empty() {
        return nack(&cmd.id, "An answer needs the item's id.".into());
    }
    let answers: Option<Vec<String>> = cmd
        .args
        .get("answers")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|x| x.as_str().map(str::to_string)).collect::<Option<_>>())
        .unwrap_or(None);
    let Some(answers) = answers else {
        return nack(&cmd.id, "An answer needs its answers, each one a string.".into());
    };
    if let Err(reason) = claim_check(view) {
        return nack(&cmd.id, reason);
    }
    let held = view
        .live
        .snapshot()
        .projects
        .get(project)
        .and_then(|p| p.items.iter().find(|i| i.id == id).map(|i| i.state.clone()));
    if let Some(state) = held.filter(|s| s != "needs_clarification" && s != "needs_human") {
        return nack(&cmd.id, format!("Item {id} is {state}, not needs_clarification"));
    }
    let attached = match attach::attached(cmd) {
        Ok(a) => a,
        Err(reason) => return nack(&cmd.id, reason),
    };
    let pending = attached.is_some();
    view.item_commands.push(ItemCommand::Answer {
        project: project.to_string(),
        item: id.to_string(),
        answers,
        attached,
    });
    if pending {
        json!({ "type": PENDING, "id": cmd.id })
    } else {
        ack(&cmd.id)
    }
}
