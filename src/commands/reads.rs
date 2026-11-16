//! `read_file { path }` and `issues_preview { label?, milestone?, anyone? }` (`agent/src/commands.ts`): reads the site
//! asks of this computer's checkout and `gh`. Each is queued for the project's loop, which holds the store link Show
//! matches needs, runs [`crate::read_kinds`] and answers with the answer as the `ack`'s `result`
//! (`orchestrator::driver::reads`), so each answers `pending` here. Taken in `claim` and `runs` mode alike.
use serde_json::{Value, json};

use super::{Command, PENDING, acts_check, nack};
use crate::view::{AgentView, ReadCommand};

pub(super) fn read(cmd: &Command, project: &str, view: &AgentView) -> Value {
    if let Err(reason) = acts_check(view) {
        return nack(&cmd.id, reason);
    }
    view.item_commands.push_read(ReadCommand {
        project: project.to_string(),
        command_id: cmd.id.clone(),
        kind: cmd.name.clone(),
        args: cmd.args.clone(),
    });
    json!({ "type": PENDING, "id": cmd.id })
}
