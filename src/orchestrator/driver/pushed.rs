//! The commands the hub still pushes that the project's loop settles with what the passes already do (command sweep 2): **Merge
//! into main** on an item and the merge control into main (`merge_pr { itemId }`, `merge_pr { ship: true }`) go to the same
//! asks the `merge_requested` row makes (`merge_item.rs`), and `main_pr_draft` to the draft Open PR to main makes
//! (`main_pr.rs`). Each is queued by the connector's thread ([`crate::view::ProjectCommand`]); here its ask is made and the
//! command is answered from a task when the pass that owns it has answered, `ack`, or `nack` with the reason in the TS agent's
//! words. Nothing starts other work: a command does the thing asked, or asks the store.
use serde_json::{Value, json};
use tokio::sync::oneshot;

use super::{Links, ProjectLoop};
use crate::intents::main_pr::Why;
use crate::view::ProjectCommand;

fn why(w: Why) -> String {
    match w {
        Why::Refused(m) | Why::Failed(m) => m,
    }
}

impl ProjectLoop {
    /// Asks for what `command` names, if it is one of these. `true` when it was.
    pub(super) fn push_command(&mut self, command: &ProjectCommand, links: &Links) -> bool {
        let (items, project) = (links.item_commands.clone(), self.id.clone());
        let answer = move |msg: Value| items.answer(&project, msg);
        match command {
            ProjectCommand::Merge { command_id, item, .. } => {
                let (reply, done) = oneshot::channel();
                self.queue_merge(item.clone(), reply);
                let id = command_id.clone();
                tokio::spawn(async move {
                    let msg = match done.await {
                        Ok(Ok(())) => json!({ "type": "ack", "id": id }),
                        Ok(Err(m)) => json!({ "type": "nack", "id": id, "reason": m }),
                        Err(_) => {
                            json!({ "type": "nack", "id": id, "reason": "This computer stopped before it could merge it." })
                        }
                    };
                    answer(msg);
                });
            }
            ProjectCommand::Ship { command_id, .. } => {
                let (reply, done) = oneshot::channel();
                self.queue_ship(reply);
                let id = command_id.clone();
                tokio::spawn(async move {
                    let msg = match done.await {
                        // It merged, but the dev branch wasn't started again (or the merge was only queued): the strip shows it.
                        Ok(Ok(Some(warning))) => json!({ "type": "ack", "id": id, "result": { "warning": warning } }),
                        Ok(Ok(None)) => json!({ "type": "ack", "id": id }),
                        Ok(Err(w)) => json!({ "type": "nack", "id": id, "reason": why(w) }),
                        Err(_) => {
                            json!({ "type": "nack", "id": id, "reason": "This computer stopped before it could merge it." })
                        }
                    };
                    answer(msg);
                });
            }
            ProjectCommand::MainPrDraft { command_id, .. } => {
                let (reply, done) = oneshot::channel();
                self.queue_main_pr_draft(reply);
                let id = command_id.clone();
                tokio::spawn(async move {
                    let msg = match done.await {
                        Ok(Ok(draft)) => {
                            json!({ "type": "ack", "id": id, "result": { "title": draft.title, "body": draft.body } })
                        }
                        Ok(Err(w)) => json!({ "type": "nack", "id": id, "reason": why(w) }),
                        Err(_) => {
                            json!({ "type": "nack", "id": id, "reason": "This computer stopped before it could read it." })
                        }
                    };
                    answer(msg);
                });
            }
            _ => return false,
        }
        true
    }
}
