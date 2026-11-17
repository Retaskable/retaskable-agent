//! A try-out found a problem after the slice's merge (`problem` in the TS agent's `storeTryouts.ts`, `reopen.ts`,
//! docs/specs/agent/06.4-playtests.md "Problem reopens the item itself"). The store forwards it to the computer that sent the
//! try-out (`tryout_problem`, kept by the socket for this loop); that computer takes it (`tryout_taken`, which answers the rest of
//! the row, as only `{ key, sessionId, lane }` of what it built is kept, `tryouts_built`) and asks the store to reopen the item
//! with the person's words (`reopen`, with this computer as the agent: the store hands the item back under a lease, as a claim
//! does). The item then starts like any claimed one: its remaining work is the person's report and the try-out's steps.
//!
//! A problem this computer has no record of is left alone: in 2 minutes the store makes it a fix item any agent can claim. One the
//! item can't be reopened for (a chore's own try-out, an item being built, a store that doesn't know `reopen`) becomes a
//! `playtest_fix` trigger, as in TypeScript, so a person's report is never lost.
use super::{Links, ProjectLoop};
use crate::backlog::pool::Trigger;
use crate::backlog::tryout_taken::Taken;
use crate::backlog::{ReopenAnswer, ReopenRequest};
use crate::item_lifecycle::ItemState;
use crate::telemetry::{Event, event};

impl ProjectLoop {
    /// Takes the problems the store forwarded since the last pass.
    pub(super) async fn hear_tryout_problems(&mut self, links: &Links) {
        for (key, notes) in links.work.take_problems(&self.id) {
            self.tryout_problem(&key, &notes, links).await;
        }
    }

    async fn tryout_problem(&mut self, key: &str, notes: &str, links: &Links) {
        let mut built = self.store.tryouts_built();
        let Some(at) = built.iter().position(|b| b.key == key) else {
            tracing::info!(
                "{}: a problem with {key} came back, but this computer didn't build it: leaving it to the store",
                self.id
            );
            return;
        };
        let (Some(repo), Ok(Some(token))) = (self.repo, links.store_token().await) else {
            tracing::info!(
                "{}: couldn't take the problem with {key}: no store; it becomes a fix item there",
                self.id
            );
            return;
        };
        let taken = match self.client.tryout_taken(repo, Some(&token), key, &links.machine).await {
            Ok(t) => t,
            Err(e) => {
                self.store_error("tryout_taken", &e);
                return;
            }
        };
        built.remove(at);
        self.store.set_tryouts_built(&built);
        if let Err(e) = self.store.save() {
            tracing::warn!("{}: saving the try-outs built failed: {e:#}", self.id);
        }
        let Taken {
            taken: true,
            item_id: Some(item_id),
            ..
        } = &taken
        else {
            tracing::info!(
                "{}: the problem with {key} is already a fix item in the shared backlog",
                self.id
            );
            return;
        };
        let steps = taken.steps.clone();
        let what = taken.what.clone().unwrap_or_default();
        let title = taken.title.clone().unwrap_or_else(|| item_id.clone());
        if crate::reopen::may_reopen(item_id, false) {
            let request = ReopenRequest {
                item_id,
                notes,
                by: taken.by.as_deref(),
                what: Some(&what),
                steps: Some(&steps),
            };
            let agent = self.claim_agent(links);
            match self.client.reopen(repo, Some(&token), &request, &agent).await {
                Ok(ReopenAnswer::Reopened { item, token: lease }) => {
                    tracing::info!("{}: {item_id} reopened for a problem found after it merged", self.id);
                    self.take_lease(&item, lease, ItemState::Queued);
                    self.events.add_in(
                        &self.id,
                        Event::ItemState(event::ItemState {
                            item: Some(item_id.clone()),
                            to: Some("queued".into()),
                            note: Some(format!("Reopened: a try-out after it merged found a problem: {notes}")),
                            ..Default::default()
                        }),
                    );
                    return;
                }
                Ok(ReopenAnswer::Kept) => {}
                Err(e) => tracing::info!(
                    "{}: the backlog couldn't reopen {item_id}, so a playtest fix is asked for as before: {e}",
                    self.id
                ),
            }
        }
        let fix = Trigger::PlaytestFix {
            key: key.to_string(),
            item_id: item_id.clone(),
            title,
            what,
            steps,
            notes: notes.to_string(),
            pr_url: taken.pr_url.clone(),
        };
        if let Err(e) = self.client.chore_trigger(repo, Some(&token), &fix).await {
            self.store_error("chore_trigger", &e);
        }
    }
}
