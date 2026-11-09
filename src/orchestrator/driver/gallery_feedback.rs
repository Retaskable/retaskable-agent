//! The `gallery_feedback` step of a project's loop (tray/05.3 section 5.3.4, agent/06.8 **Feedback**): behind the
//! `gallery-feedback-command` flag, which this records for the command each pass. A queued feedback is sent to the shared
//! backlog as a spec draft (`draft_queue`: the request alone, so the drafter makes the cut) and the command is answered
//! once it is queued: `ack`, otherwise `nack` with why. A store that can't be reached fails closed: nothing is queued.
use serde_json::json;

use super::Shared;
use super::project::ProjectLoop;
use crate::backlog::Client;
use crate::backlog::asks::QueuedDraft;
use crate::view::GalleryFeedbackCommand;

impl ProjectLoop {
    pub(super) async fn gallery_feedback_pass(&mut self, shared: &Shared<'_>) {
        let queue = &shared.links.item_commands;
        let on = self.flags.on("gallery-feedback-command").await;
        queue.set_flag(&self.id, "gallery-feedback-command", on);
        for cmd in queue.take_feedback(&self.id) {
            let done = queue_feedback(&self.client, self.repo, &cmd, shared).await;
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

async fn queue_feedback(
    client: &Client,
    repo: Option<u64>,
    cmd: &GalleryFeedbackCommand,
    shared: &Shared<'_>,
) -> Result<(), String> {
    let unreachable = |why: String| format!("Couldn't queue the draft in the shared backlog: {why}");
    let repo = repo.ok_or_else(|| unreachable("this project isn't linked to one".into()))?;
    let token = shared
        .links
        .keychain
        .get(&shared.links.backlog_url)
        .map_err(|e| unreachable(format!("reading the backlog link: {e:#}")))?
        .ok_or_else(|| unreachable("this computer isn't linked to it".into()))?;
    let login = Some(cmd.by.clone()).filter(|b| crate::ideas::accept::is_login(b));
    let draft = QueuedDraft {
        request: cmd.request.clone(),
        requested_by: login.clone(),
        started_by: login,
        intake: cmd.do_next.then(|| json!({ "doNext": true })),
        ..QueuedDraft::default()
    };
    client
        .draft_queue(repo, Some(&token), &draft)
        .await
        .map(|_| ())
        .map_err(|e| unreachable(format!("{e}")))
}
