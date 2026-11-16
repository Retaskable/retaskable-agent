//! The expert commands (docs/specs/expert-chat.md section 3, `agent/src/expert/commands.ts`): `expert_start`,
//! `expert_say`, `expert_stop` and `expert_end`. Each is queued for the project's loop, which checks that the sender
//! is the person who started the session, carries it out and answers (`orchestrator::driver::experts`), so each
//! answers `pending` here. Refused outside `claim` mode, where nothing runs.
use serde_json::{Value, json};

use super::{Command, PENDING, acts_check, nack};
use crate::view::{AgentView, ExpertAction, ExpertCommand};

/// The longest request or message (`shared/cloudCommands.ts`).
const TEXT_MAX: usize = 4000;

fn text_of<'a>(cmd: &'a Command, key: &str) -> Option<&'a str> {
    cmd.args
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.chars().count() <= TEXT_MAX)
}

fn item_of(cmd: &Command) -> Result<String, String> {
    match cmd.args.get("itemId").and_then(Value::as_str).map(str::trim) {
        Some(id) if !id.is_empty() => Ok(id.to_string()),
        _ => Err(format!("{} needs the session's itemId.", cmd.name)),
    }
}

/// `pictureIds` (`shared/cloudCommands.ts`): up to four ids the store gave. One that isn't an id of that shape is
/// refused, so it is never made into a file name.
fn picture_ids(cmd: &Command) -> Result<Vec<String>, String> {
    let list = match cmd.args.get("pictureIds") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err("pictureIds must be a list.".into()),
    };
    if list.len() > super::MAX_PICTURES {
        return Err(format!("At most {} pictures to a message.", super::MAX_PICTURES));
    }
    list.iter()
        .map(|p| match p.as_str() {
            Some(id) if crate::ask::pictures::valid(id) => Ok(id.to_string()),
            other => Err(format!("No attached image {}.", other.unwrap_or("?"))),
        })
        .collect()
}

fn action(cmd: &Command) -> Result<ExpertAction, String> {
    match cmd.name.as_str() {
        "expert_start" => {
            if cmd.args.get("continue").is_some_and(|c| !c.is_null()) {
                return Err("Carrying on a Claude Code session isn't built in this agent yet.".into());
            }
            let request = text_of(cmd, "request").ok_or("expert_start needs a request of 1 to 4000 characters.")?;
            Ok(ExpertAction::Start {
                request: request.to_string(),
            })
        }
        "expert_say" => {
            let text = text_of(cmd, "text").ok_or("expert_say needs text of 1 to 4000 characters.")?;
            let pictures = picture_ids(cmd)?;
            Ok(ExpertAction::Say {
                item: item_of(cmd)?,
                text: text.to_string(),
                pictures,
                now: cmd.args.get("now").and_then(Value::as_bool) == Some(true),
            })
        }
        "expert_stop" => Ok(ExpertAction::Stop { item: item_of(cmd)? }),
        _ => {
            let outcome = cmd.args.get("outcome").and_then(Value::as_str).unwrap_or("");
            if !matches!(outcome, "ship" | "backlog" | "park" | "discard") {
                return Err("expert_end needs an outcome: ship, backlog, park or discard.".into());
            }
            Ok(ExpertAction::End {
                item: item_of(cmd)?,
                outcome: outcome.to_string(),
            })
        }
    }
}

/// Queues the command for the loop, which answers it once it has been carried out.
pub(super) fn expert(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let action = match action(cmd) {
        Ok(a) => a,
        Err(reason) => return nack(&cmd.id, reason),
    };
    if let Err(reason) = acts_check(view) {
        return nack(&cmd.id, reason);
    }
    view.item_commands.push_expert(ExpertCommand {
        project: project.to_string(),
        command_id: cmd.id.clone(),
        by: cmd.issued_by.clone(),
        action,
    });
    json!({ "type": PENDING, "id": cmd.id })
}
