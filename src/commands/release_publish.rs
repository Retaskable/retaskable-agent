//! `release_publish {}` and `release_unpublish {}` (`commands.ts`, release-notes.md section 8.1): the release note's public page
//! on the agent's own retaskable.com. Queued for the project's loop (`orchestrator::driver::release_page`), which reads the
//! note from the store, asks the cloud and answers; **Publish** answers `{ url }`. Only the agent's own cloud (the first this
//! agent connects to) may ask: one sent over another cloud's connection is refused in TS's words, and so is one when this agent
//! has no cloud link yet. Taken where the orchestrator acts (`claim` or `runs` mode), as the loop is what has the store.
use serde_json::{Value, json};

use super::{Command, PENDING, acts_check, nack};
use crate::view::{AgentView, Link, ProjectCommand};

/// What a command from another cloud is told (`NOT_OWN_CLOUD`).
pub(crate) const NOT_OWN_CLOUD: &str =
    "Release notes are published on this agent's own retaskable.com, not this one: ask from there.";

pub(super) fn release_page(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let Some(link) = view.item_commands.cloud.get() else {
        return nack(&cmd.id, NOT_OWN_CLOUD.into());
    };
    if let Err(reason) = acts_check(view) {
        return nack(&cmd.id, reason);
    }
    view.item_commands.projects.push(ProjectCommand::Release {
        project: project.to_string(),
        command_id: cmd.id.clone(),
        unpublish: cmd.name == "release_unpublish",
        link: Link(link),
    });
    view.work.wake(project);
    json!({ "type": PENDING, "id": cmd.id })
}
