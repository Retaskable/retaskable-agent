//! The commands on one item the hub still pushes (`answer`; `exec` in the TS agent's `commands.ts`): checked here, queued for
//! the project's loop, which owns the item's lane and lease and carries them out.
use serde_json::{Value, json};

use super::{Command, PENDING, ack, acts_check, attach, claim_check, nack};
use crate::view::{AgentView, ItemCommand, ProjectCommand};

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

/// Words `merge_pr` is refused with unless it names exactly one of the three (`commands.ts`).
const NAME_ONE: &str = "Name one item, one draft, or the pull request into main to merge.";

/// `merge_pr { itemId | draftId | ship: true }`: exactly one is named, as the TS agent checks first. A draft's is **Merge into
/// main** on a spec draft ([`super::drafts::press`]). An item's, or the merge control into main (`ship`), is queued for the
/// project's loop (`orchestrator::driver::pushed`), which asks for the same merge the `merge_requested` row does and answers
/// when it is done: `ack` (with `result: { warning }` when the merge control merged but the dev branch wasn't started again, or
/// the merge was only queued: the TS agent's HTTP route carried that, its command dropped it), or `nack` with why.
pub(super) fn merge_pr(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let named = |k: &str| match cmd.args.get(k) {
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Bool(b)) => *b,
        Some(Value::Null) | None => false,
        Some(_) => true,
    };
    let (item, draft, ship) = (named("itemId"), named("draftId"), named("ship"));
    if [item, draft, ship].iter().filter(|n| **n).count() != 1 {
        return nack(&cmd.id, NAME_ONE.into());
    }
    if draft {
        return super::drafts::press(cmd, project, view);
    }
    if let Err(reason) = acts_check(view) {
        return nack(&cmd.id, reason);
    }
    let project_for_wake = project.to_string();
    let (project, command_id) = (project.to_string(), cmd.id.clone());
    view.item_commands.projects.push(if ship {
        ProjectCommand::Ship { project, command_id }
    } else {
        let item = cmd
            .args
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if item.is_empty() {
            return nack(&cmd.id, NAME_ONE.into());
        }
        ProjectCommand::Merge {
            project,
            command_id,
            item: item.to_string(),
        }
    });
    view.work.wake(&project_for_wake);
    json!({ "type": PENDING, "id": cmd.id })
}

/// `main_pr_draft {}`: what Open PR to main would send, for its confirmation (team-branch.md section 5). The hub answers from
/// the store's `main_pr_draft` fact when it holds one with something ahead; this is for when it holds none, or an empty one. Queued
/// for the project's loop, which knows the dev branch and answers `{ title, body }` (`orchestrator::driver::main_pr`).
pub(super) fn main_pr_draft(cmd: &Command, project: &str, view: &AgentView) -> Value {
    if let Err(reason) = acts_check(view) {
        return nack(&cmd.id, reason);
    }
    view.item_commands.projects.push(ProjectCommand::MainPrDraft {
        project: project.to_string(),
        command_id: cmd.id.clone(),
    });
    view.work.wake(project);
    json!({ "type": PENDING, "id": cmd.id })
}

/// `play { itemId }` (**Play this build**) and `stop_play { itemId }`: queued for the project's loop, which holds the item's lane and
/// branch, plays its build in the lane or in the project's play checkout and answers when the game has started or why not
/// (`orchestrator::driver::build_play`). A Play needs the project's play command.
pub(super) fn play_item(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let item = cmd
        .args
        .get("itemId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if let Err(reason) = acts_check(view) {
        return nack(&cmd.id, reason);
    }
    let play = cmd.name == "play";
    if play {
        let Some(p) = super::allowed_project(view, project) else {
            return nack(&cmd.id, "Finish setup first.".into());
        };
        if crate::text::is_blank(&p.play_command) {
            return nack(
                &cmd.id,
                "No play command set. Add one in Settings → Project settings, under How to try it.".into(),
            );
        }
    }
    let (project, item, command_id) = (project.to_string(), item.to_string(), cmd.id.clone());
    view.item_commands.push(if play {
        ItemCommand::PlayItem {
            project,
            item,
            command_id,
        }
    } else {
        ItemCommand::StopItem {
            project,
            item,
            command_id,
        }
    });
    json!({ "type": PENDING, "id": cmd.id })
}
