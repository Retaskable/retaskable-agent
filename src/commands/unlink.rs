//! `unlink_project { removeLanes? }` (`unlinkProject.ts` in the TS agent, projects.md section 6.6): Unlink this project on
//! retaskable.com. Queued for the project's loop (`orchestrator::driver::unlink`), which refuses while anything runs in the
//! project, answers the command (`ack`, first, as TS does), gives back what this computer holds in the shared backlog and takes the
//! project out of the config. When that is over this thread removes the project's state folder and restarts the agent, which
//! builds every part from the project list once at start (the TS agent closes the project live). Taken where the orchestrator
//! acts (`claim` or `runs` mode): it is the loop that knows what runs.
use serde_json::{Value, json};

use super::{Command, PENDING, acts_check, nack};
use crate::view::{AgentView, ProjectCommand, Reply};

pub(super) fn unlink_project(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let Some(adder) = view.add_project.get() else {
        return nack(&cmd.id, "This agent can't unlink a project from retaskable.com.".into());
    };
    let Some(dir) = view.settings.dir().map(std::path::Path::to_path_buf) else {
        return nack(&cmd.id, "This computer's settings can't be found.".into());
    };
    if let Err(reason) = acts_check(view) {
        return nack(&cmd.id, reason);
    }
    let remove_lanes = cmd.args.get("removeLanes").and_then(Value::as_bool) != Some(false);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    view.item_commands.projects.push(ProjectCommand::Unlink {
        project: project.to_string(),
        command_id: cmd.id.clone(),
        remove_lanes,
        done: Reply::new(sender),
    });
    view.work.wake(project);
    let id = project.to_string();
    tokio::spawn(async move {
        // Refused or failed: the loop answered the command and logged why, and nothing here changes.
        if let Ok(Ok(())) = receiver.await {
            if let Err(e) = crate::unlink::remove_state(&dir, &id) {
                tracing::error!("Couldn't remove {id}'s state folder: {e}");
            }
            adder.restart_now();
        }
    });
    json!({ "type": PENDING, "id": cmd.id })
}
