//! The `idea_accept` step of a project's loop (docs/specs/agent/09-web-ui.md section 9.3, "Ideas"): behind the
//! command asks for it (the pass also records the `restart-play-command` flag for `restart_play` / `stop_play`). A queued Build this is written up as a draft in
//! the shared backlog (`ideas::accept`) and the command is answered once it has been: `ack` after the draft is queued,
//! otherwise `nack` with why. A store that can't be reached fails closed: nothing is queued.
use serde_json::json;

use super::Shared;
use super::project::ProjectLoop;
use crate::backlog::Client;
use crate::ideas::{self, Ideas};

impl ProjectLoop {
    pub(super) async fn idea_pass(&mut self, shared: &Shared<'_>) {
        let queue = &shared.links.item_commands;
        for cmd in queue.take_ideas(&self.id) {
            let done = accept_idea(&self.client, self.repo, &cmd.idea, &cmd.by, shared).await;
            queue.answer(
                &self.id,
                match done {
                    Ok(()) => json!({ "type": "ack", "id": cmd.command_id }),
                    Err(reason) => json!({ "type": "nack", "id": cmd.command_id, "reason": reason }),
                },
            );
        }
    }
}

async fn accept_idea(
    client: &Client,
    repo: Option<u64>,
    idea: &str,
    by: &str,
    shared: &Shared<'_>,
) -> Result<(), String> {
    let unreachable = |why: String| format!("Couldn't answer the idea in the shared backlog: {why}");
    let repo = repo.ok_or_else(|| unreachable("this project isn't linked to one".into()))?;
    let token = shared
        .links
        .keychain
        .get(&shared.links.backlog_url)
        .map_err(|e| unreachable(format!("reading the backlog link: {e:#}")))?
        .ok_or_else(|| unreachable("this computer isn't linked to it".into()))?;
    let ideas = Ideas {
        client,
        repo_id: repo,
        token: Some(&token),
    };
    ideas::accept(&ideas, idea, by).await
}
