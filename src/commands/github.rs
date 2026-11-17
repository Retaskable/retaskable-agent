//! `github_owners { fresh? }` and `github_repos { owner, fresh? }` (`agent/src/commands.ts`, `github.ts`): the owners and
//! repos this computer's own `gh` lists for **Link project…**. Reads, so a computer with no project yet takes them too; the
//! answer is the ack's `result` (the audit log keeps its size only). `gh` can take a while, so each answers `pending` and a
//! task sends the `ack`, or a `nack` with the reason.
use std::time::Duration;

use serde_json::{Value, json};

use super::{Command, PENDING, nack};
use crate::view::AgentView;

/// `gh` is given this long, the TS agent's longest step (`gh repo list`).
const LIMIT: Duration = Duration::from_secs(120);

pub(super) fn run(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let fresh = cmd.args.get("fresh").and_then(Value::as_bool) == Some(true);
    let owner = match cmd.name.as_str() {
        "github_repos" => match cmd.args.get("owner").and_then(Value::as_str) {
            Some(o) if !o.is_empty() && o.len() <= 39 && o.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => {
                Some(o.to_string())
            }
            _ => {
                return nack(
                    &cmd.id,
                    "Not a command this agent takes: github_repos with a GitHub login as owner".into(),
                );
            }
        },
        _ => None,
    };
    let linked: Vec<String> = view.projects.iter().map(|p| p.github_repo.clone()).collect();
    let (picker, commands) = (view.github.clone(), view.item_commands.clone());
    let (id, project) = (cmd.id.clone(), project.to_string());
    tokio::spawn(async move {
        let asked = tokio::task::spawn_blocking(move || match owner {
            Some(owner) => picker.repos(&owner, fresh, &linked),
            None => Ok(picker.owners(fresh)),
        });
        let answer = match tokio::time::timeout(LIMIT, asked).await {
            Ok(Ok(Ok(result))) => json!({ "type": "ack", "id": id, "result": result }),
            Ok(Ok(Err(why))) => nack(&id, why),
            Ok(Err(_)) => nack(&id, "Couldn't ask GitHub just now.".into()),
            Err(_) => nack(&id, "GitHub didn't answer in time.".into()),
        };
        commands.answer(&project, answer);
    });
    json!({ "type": PENDING, "id": cmd.id })
}
