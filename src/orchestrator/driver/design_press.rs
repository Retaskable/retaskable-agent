//! A person's press on a plan's design review (`draft_skip_design`, `board_design_write`; design-view.md sections 11 and
//! 14). The connector only queues it ([`crate::view::DraftCommand`]); here, paused or not, each runs in a task of its
//! own (a drawing takes a session) and answers its command when it is done: `ack`, or `nack` with the reason. Fails
//! closed: with the store not reachable the press is refused, never done from a guess.
use std::sync::Arc;

use serde_json::json;

use super::Shared;
use super::drafts::{DraftHost, claim_agent};
use super::project::ProjectLoop;
use super::signed_out::signed_out;
use crate::design_board::{Acting, Drawing, draw_design, skip_design};
use crate::view::DraftCommand;

impl ProjectLoop {
    /// Starts each queued design press. `presses` are only [`DraftCommand::SkipDesign`] and
    /// [`DraftCommand::DrawDesign`].
    pub(super) async fn design_presses(
        &mut self,
        presses: Vec<DraftCommand>,
        host: &Arc<dyn DraftHost>,
        shared: &Shared<'_>,
    ) {
        if presses.is_empty() {
            return;
        }
        let items = shared.links.item_commands.clone();
        let refuse = |presses: &[DraftCommand], why: String| {
            for press in presses {
                if let Some(id) = command_id(press) {
                    items.answer(&self.id, json!({ "type": "nack", "id": id, "reason": why }));
                }
            }
        };
        let Some(repo) = self.repo else {
            return refuse(&presses, "This project isn't linked to the shared backlog.".into());
        };
        let token = match shared.links.store_token().await {
            Ok(Some(token)) => token,
            Ok(None) => return refuse(&presses, "This computer isn't linked to the shared backlog.".into()),
            Err(e) => return refuse(&presses, format!("Can't reach the shared backlog: {e:#}")),
        };
        let agent = claim_agent(shared.links, &self.project);
        let signed_out = signed_out(shared);
        for press in presses {
            let (client, host, project) = (self.client.clone(), host.clone(), self.project.clone());
            let (items, id, token, agent) = (items.clone(), self.id.clone(), token.clone(), agent.clone());
            self.landing.tasks.push(tokio::spawn(async move {
                let layouts = |prompt: String| host.design_layout(&project, prompt);
                let transport = host.board_transport();
                let acting = Acting {
                    drawing: Drawing {
                        client: &client,
                        repo,
                        token: &token,
                        transport: transport.as_ref(),
                        layouts: &layouts,
                        timing: Default::default(),
                        claude_signed_out: signed_out,
                    },
                    agent: &agent,
                };
                let (command, done) = match press {
                    DraftCommand::SkipDesign { draft, command_id, .. } => {
                        (command_id, skip_design(&acting, &draft).await)
                    }
                    DraftCommand::DrawDesign {
                        draft,
                        note,
                        command_id,
                        ..
                    } => (command_id, draw_design(&acting, &draft, note.as_deref()).await),
                    _ => return None,
                };
                items.answer(
                    &id,
                    match done {
                        Ok(()) => json!({ "type": "ack", "id": command }),
                        Err(e) => json!({ "type": "nack", "id": command, "reason": e.message }),
                    },
                );
                None
            }));
        }
    }
}

fn command_id(press: &DraftCommand) -> Option<&str> {
    match press {
        DraftCommand::SkipDesign { command_id, .. } | DraftCommand::DrawDesign { command_id, .. } => Some(command_id),
        _ => None,
    }
}
