//! `link_project { repo, ... }` (`agent/src/commands.ts`, `linkProject.ts`): Link project… on retaskable.com. Only a GitHub
//! repo (`owner/name`) is taken here: a project kept only on this computer, a plain git remote, a plain folder, an Apps
//! Script project, joining a project on others and a new GitHub repo are the TS agent's local project kinds, and this agent
//! has none of them. The repo must be one this computer's `gh` lists for its owner (the same check the picker makes) and not
//! linked already; then it is added as **Add project** on the settings page adds it ([`crate::add_project`]): the checkout
//! on this computer is used or the repo is cloned beside the others, Setup's scan fills in the rest, and the project
//! starts paused. It is taken before Setup too, as the first project of a new computer.
//!
//! The add takes minutes (a clone), so this answers `pending` and a task sends the `ack` with `{ id }`, or a `nack` with the
//! reason, and **then** restarts the agent to take the project (every part of it is built from the project list once, at
//! start): the answer goes out first. The TS agent takes the project live; this one restarts.
use std::time::Duration;

use serde_json::{Value, json};

use super::{Command, PENDING, nack};
use crate::add_project::{AddProject, Refusal, Status};
use crate::view::AgentView;

/// What the add is waited for: a big clone is slow, but not an hour.
const LIMIT: Duration = Duration::from_secs(30 * 60);
const EVERY: Duration = Duration::from_millis(250);

/// The one field besides `repo` the site may send that this agent can take: which organisation a project goes in is the
/// hub's (it fills in what the project is on retaskable.com), not something saved here.
const IGNORED: &[&str] = &["organisationId"];

pub(super) fn link_project(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let Some(adder) = view.add_project.get() else {
        return nack(&cmd.id, "This agent can't link a project from retaskable.com.".into());
    };
    let args = cmd.args.as_object().cloned().unwrap_or_default();
    if let Some(other) = args.keys().find(|k| *k != "repo" && !IGNORED.contains(&k.as_str())) {
        return nack(
            &cmd.id,
            format!(
                "This agent links only a GitHub repo from retaskable.com, not a project with {other}: \
                 it keeps no project only on this computer, on a plain remote or in a folder."
            ),
        );
    }
    let Some(repo) = args.get("repo").and_then(Value::as_str).filter(|r| !r.is_empty()) else {
        return nack(&cmd.id, "Name a repo to link.".into());
    };
    let Some(name) = crate::add_project::parse_repo(repo) else {
        return nack(&cmd.id, "repo must be owner/name.".into());
    };
    let (picker, commands) = (view.github.clone(), view.item_commands.clone());
    let linked: Vec<String> = view.projects.iter().map(|p| p.github_repo.clone()).collect();
    let (id, project) = (cmd.id.clone(), project.to_string());
    tokio::spawn(async move {
        let done = add_repo(&adder, &picker, &linked, &name).await;
        let answer = match done {
            Ok(added) => json!({ "type": "ack", "id": id, "result": { "id": added } }),
            Err(why) => nack(&id, why),
        };
        let ok = answer["type"] == "ack";
        // The answer goes up the connection before the agent restarts to take the project.
        commands.answer(&project, answer);
        if ok {
            adder.restart_now();
        }
    });
    json!({ "type": PENDING, "id": cmd.id })
}

/// The checks the picker makes (an owner `gh` lists, a repo in its list), then the add; the new project's id.
async fn add_repo(
    adder: &std::sync::Arc<AddProject>,
    picker: &std::sync::Arc<crate::github_pick::GithubPicker>,
    linked: &[String],
    name: &str,
) -> Result<String, String> {
    let owner = name.split('/').next().unwrap_or_default().to_string();
    let (p, o, l) = (picker.clone(), owner.clone(), linked.to_vec());
    let rows = tokio::task::spawn_blocking(move || p.repos(&o, false, &l))
        .await
        .map_err(|_| "Couldn't ask GitHub just now.".to_string())??;
    let found = rows
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| {
            r["nameWithOwner"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
        .ok_or_else(|| format!("{name} isn't one of {owner}'s repos."))?;
    // GitHub's own spelling, as the list gives it.
    let name = found["nameWithOwner"].as_str().unwrap_or(name).to_string();
    if found["linked"] == true || linked.iter().any(|l| l.eq_ignore_ascii_case(&name)) {
        return Err(format!("{name} is already linked on this machine."));
    }
    adder.start_holding_restart(&name).map_err(|r| match r {
        Refusal::Bad(m) | Refusal::Conflict(m) => m,
    })?;
    let began = std::time::Instant::now();
    loop {
        match adder.status() {
            Status::Done { repo, id, .. } if repo.eq_ignore_ascii_case(&name) => return Ok(id),
            Status::Failed { repo, message } if repo.eq_ignore_ascii_case(&name) => return Err(message),
            _ if began.elapsed() > LIMIT => return Err("Linking took too long: check the settings page.".to_string()),
            _ => tokio::time::sleep(EVERY).await,
        }
    }
}
