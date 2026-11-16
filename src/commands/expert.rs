//! The expert commands (docs/specs/expert-chat.md section 3, `agent/src/expert/commands.ts`): `expert_start`,
//! `expert_say`, `expert_stop`, `expert_end`, `expert_helper` and `session_history`. Each is queued for the project's loop, which checks that the sender
//! is the person who started the session, carries it out and answers (`orchestrator::driver::experts`), so each
//! answers `pending` here. Refused outside `claim` mode, where nothing runs.
use serde_json::{Value, json};

use super::{Command, PENDING, acts_check, nack};
use crate::backlog::expert::ExpertFrom;
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
            let continue_from = match cmd.args.get("continue") {
                None | Some(Value::Null) => None,
                Some(Value::String(id)) if ExpertFrom::session_id_ok(id) => Some(id.clone()),
                Some(_) => return Err("expert_start's continue must be a Claude Code session id.".into()),
            };
            let request = text_of(cmd, "request").ok_or("expert_start needs a request of 1 to 4000 characters.")?;
            Ok(ExpertAction::Start {
                request: request.to_string(),
                continue_from,
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
        "expert_helper" => {
            let task = text_of(cmd, "task").ok_or("expert_helper needs a task of 1 to 4000 characters.")?;
            let lane = match cmd.args.get("mode").and_then(Value::as_str) {
                Some("read") => false,
                Some("lane") => true,
                _ => return Err("expert_helper needs a mode: read or lane.".into()),
            };
            Ok(ExpertAction::Helper {
                item: item_of(cmd)?,
                task: task.to_string(),
                lane,
            })
        }
        "session_history" => Ok(ExpertAction::History {
            item: item_of(cmd)?,
            before: cmd.args.get("before").and_then(Value::as_i64),
            limit: cmd
                .args
                .get("limit")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok()),
        }),
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

/// `claude_sessions` (expert-chat.md section 3.10): the computer's recent Claude Code sessions, for the Start sheet's
/// "Continue a Claude Code session". Only titles, folders and branches, never a conversation; the sessions in a
/// project's lanes (Retaskable's own) are left out. The hub lets only the computer's owner send it.
pub(super) fn claude_sessions(cmd: &Command, view: &AgentView) -> Value {
    let root = crate::expert::claude_sessions::projects_root(&crate::env::Env::from_process());
    let lanes: Vec<std::path::PathBuf> = view.projects.iter().map(|p| p.worktrees_dir.clone()).collect();
    answer_sessions(&cmd.id, &root, &lanes)
}

pub(super) fn answer_sessions(id: &str, root: &std::path::Path, lanes: &[std::path::PathBuf]) -> Value {
    let sessions: Vec<Value> = (crate::expert::claude_sessions::list(root, lanes).iter())
        .map(crate::expert::claude_sessions::ClaudeSession::to_json)
        .collect();
    json!({ "type": "ack", "id": id, "answer": { "sessions": sessions } })
}
