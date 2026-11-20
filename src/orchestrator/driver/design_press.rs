//! A person's press on a plan's design review (a `design_skip_requested` or `design_draw_requested` row; design-view.md
//! sections 11 and 14). The row's handler queues it ([`crate::view::DraftCommand`]); here each runs in a task of its own (a
//! drawing takes a session) and answers the row's reply when it is done: `Ok`, or the reason. Fails closed: with the
//! store not reachable the press is refused, never done from a guess.
use std::sync::Arc;

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
        let refuse = |presses: &[DraftCommand], why: String| {
            for press in presses {
                if let Some(reply) = reply_of(press) {
                    reply.send(Err(why.clone()));
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
        let machine = shared.links.machine.clone();
        let signed_out = signed_out(shared);
        for press in presses {
            let (client, host, project) = (self.client.clone(), host.clone(), self.project.clone());
            let (token, agent, machine) = (token.clone(), agent.clone(), machine.clone());
            self.landing.tasks.push(tokio::spawn(async move {
                let kit = host
                    .project_kit(&project, client.clone(), repo, token.clone(), machine)
                    .await;
                let layouts = |prompt: String| host.design_layout(&project, prompt);
                let sentences = |prompt: String| host.design_sentence(&project, prompt);
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
                        kit: &kit,
                    },
                    sentences: &sentences,
                    agent: &agent,
                };
                let (reply, done) = match press {
                    DraftCommand::SkipDesign { draft, reply, .. } => (reply, skip_design(&acting, &draft).await),
                    DraftCommand::DrawDesign { draft, note, reply, .. } => {
                        (reply, draw_design(&acting, &draft, note.as_deref()).await)
                    }
                    _ => return None,
                };
                reply.send(done.map_err(|e| e.message));
                None
            }));
        }
    }
}

fn reply_of(press: &DraftCommand) -> Option<&crate::view::Reply> {
    match press {
        DraftCommand::SkipDesign { reply, .. } | DraftCommand::DrawDesign { reply, .. } => Some(reply),
        _ => None,
    }
}
