//! **Publish** and **Unpublish** on the release note (`release_publish`, `release_unpublish`; [`crate::release_publish`]): the
//! commands are queued ([`crate::view::ProjectCommand::Release`]) by the connector's thread with the link to the agent's own
//! cloud, and the loop, which has the store link, runs each in a task of its own that answers the command when it is done: a
//! slow cloud holds nothing up. Fails closed: with no link to the store the command is refused, never answered from a guess.
use serde_json::json;

use super::{Links, ProjectLoop};
use crate::kept_facts::{self, StoreFacts};
use crate::view::Link;

impl ProjectLoop {
    pub(super) async fn release_page(&mut self, command_id: String, unpublish: bool, link: Link, links: &Links) {
        let (items, project) = (links.item_commands.clone(), self.id.clone());
        let refuse = |why: &str| items.answer(&project, json!({ "type": "nack", "id": command_id, "reason": why }));
        let Some(repo) = self.repo else {
            refuse("This project isn't linked to the shared backlog.");
            return;
        };
        let token = match links.store_token().await {
            Ok(Some(token)) => token,
            Ok(None) => return refuse("This computer isn't linked to the shared backlog."),
            Err(e) => return refuse(&format!("Can't reach the shared backlog: {e:#}")),
        };
        let store = StoreFacts {
            client: self.client.clone(),
            repo,
            token,
            machine: links.machine.clone(),
        };
        let facts = kept_facts::shared(self.store.dir());
        let (items, project) = (links.item_commands.clone(), self.id.clone());
        tokio::spawn(async move {
            let http = crate::http_client::build(std::time::Duration::from_secs(60));
            let now = crate::telemetry::events::now_ms();
            let done = if unpublish {
                crate::release_publish::unpublish(&store, &facts, &project, link.0.as_ref(), &http, now)
                    .await
                    .map(|()| None)
            } else {
                crate::release_publish::publish(&store, &facts, &project, link.0.as_ref(), &http, now)
                    .await
                    .map(Some)
            };
            let msg = match done {
                Ok(Some(result)) => json!({ "type": "ack", "id": command_id, "result": result }),
                Ok(None) => json!({ "type": "ack", "id": command_id }),
                Err(why) => json!({ "type": "nack", "id": command_id, "reason": why }),
            };
            items.answer(&project, msg);
        });
    }
}
