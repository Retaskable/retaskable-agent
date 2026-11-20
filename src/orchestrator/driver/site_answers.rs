//! A person's word kept on the item from the site (`decision { kind: "answer" | "tryout" }` and `sendBack`, the store's
//! `item_answer`, `item_tryout` and `item_send_back`; `actOnSite` and `applyKeptAnswer` in the TS agent's
//! `hostedWorkSite.ts` and `storeLink.ts`; docs/specs/agent/09.3, "A decision kept on the item from the site"). The site and the
//! MCP don't send this computer a command for an item parked for a person: the store keeps the word on the item for the
//! computer that holds it, and pushes `changed`. Each pass reads the board, so a word kept for an item run here is acted on
//! as the command's is, and a restart finds it the same way.
//!
//! The store clears the word only when the item is claimed back by name, so a word that is read and not claimed is read on
//! every pass for ever and the board keeps asking (the loop "Go with the recommended" came back in). So an answer or a try-out
//! always ends in the claim, applied or not: an answer for an item that moved on is still applied (it resumes with the answers
//! as a note), and a try-out for an item that doesn't await one is only claimed back. Each word is acted on once (`site_taken`,
//! by its `at`). A picture that can't be had leaves the word for the next pass, as TS's failed `decided` does.
use super::parking::Claimed;
use super::{Links, ProjectLoop};
use crate::ask::pictures;
use crate::backlog::{Item, ItemStatus};
use crate::item_lifecycle::ItemState;
use crate::work_signal::Wake;

impl ProjectLoop {
    /// On a push that an item changed: reads the items this project waits on for an answer, so an answer given on the site is
    /// carried out now and not at the minute's pass. A read that fails is left to that pass.
    pub(super) async fn hear_site_answers(&mut self, links: &Links) {
        if !links.work.take(&self.id, Wake::Changed) {
            return;
        }
        let waiting: Vec<String> = (self.runs.iter())
            .filter(|(_, r)| waits_for_a_person(r.state))
            .map(|(id, _)| id)
            .cloned()
            .collect();
        let (Some(repo), Ok(Some(token))) = (self.repo, links.store_token().await) else {
            return;
        };
        if waiting.is_empty() {
            return;
        }
        match self.client.list_ids(repo, Some(&token), &waiting).await {
            Ok(items) => self.take_site_answers(&items, links).await,
            Err(e) => self.store_error("list", &e),
        }
    }

    /// Acts on the word kept on each item run here, once (`site_taken`).
    pub(super) async fn take_site_answers(&mut self, items: &[Item], links: &Links) {
        for item in items {
            let at = (item.decision.as_ref().map(|d| d.at)).or(item.send_back.as_ref().map(|s| s.at));
            let Some(at) = at else {
                self.site_taken.remove(&item.id);
                continue;
            };
            if item.status != ItemStatus::NeedsPerson
                || !self.runs.contains_key(&item.id)
                || self.site_taken.get(&item.id) == Some(&at)
            {
                continue;
            }
            let taken = match (&item.decision, &item.send_back) {
                (Some(d), _) if d.kind == "answer" => self.site_answer(item, links).await,
                (Some(d), _) if d.kind == "tryout" => self.site_tryout(item, links).await,
                (_, Some(_)) => self.site_send_back(item, links).await,
                _ => true,
            };
            if taken {
                self.site_taken.insert(item.id.clone(), at);
            }
        }
    }

    /// The text with the pictures sent with it saved and their paths added. `None` when they can't be had (the word waits).
    async fn with_site_pictures(&mut self, links: &Links, item: &str, ids: &[String], text: String) -> Option<String> {
        if ids.is_empty() {
            return Some(text);
        }
        let (Some(repo), Ok(Some(token))) = (self.repo, links.store_token().await) else {
            return None;
        };
        match pictures::fetch(&self.client, repo, Some(&token), ids, &pictures::folder()).await {
            Ok(saved) => Some(pictures::with_images(&text, &saved.keep())),
            Err(e) => {
                tracing::warn!("{}: the pictures kept with {item}'s word couldn't be had: {e}", self.id);
                None
            }
        }
    }

    /// An answer kept for the item. True once it is queued (or can't be acted on at all).
    async fn site_answer(&mut self, item: &Item, links: &Links) -> bool {
        let Some(decision) = item.decision.as_ref() else {
            return true;
        };
        let id = &item.id;
        let state = self.runs.get(id).map(|r| r.state);
        let asks = self.awaits_answer(id);
        // It moved on (the local copy waits for something else and holds no lease): the store's word still wins.
        let moved_on = !asks && !self.leases.contains_key(id) && state.is_some_and(waits_for_a_person);
        if (!asks && !moved_on) || self.answering.iter().any(|(i, _)| i == id) {
            return true;
        }
        let mut answers = decision.answers.clone();
        if !decision.pictures.is_empty() {
            let last = answers.pop().unwrap_or_default();
            let Some(last) = self.with_site_pictures(links, id, &decision.pictures, last).await else {
                return false;
            };
            answers.push(last);
        }
        let by = decision.by.as_deref().unwrap_or("a person");
        tracing::info!("{}: {id} was answered by {by} on the site", self.id);
        if moved_on {
            tracing::info!(
                "{}: {id} no longer asks (it is {state:?}); the answer is applied anyway",
                self.id
            );
            if let Some(run) = self.runs.get_mut(id) {
                run.state = ItemState::NeedsClarification;
            }
            if let Some(record) = self.records.get_mut(id) {
                record.flow.questions.clear();
            }
        }
        self.take_answer(id.clone(), answers);
        true
    }

    /// A try-out kept for the item: applied when it awaits one here, else only claimed back so the store forgets it.
    async fn site_tryout(&mut self, item: &Item, links: &Links) -> bool {
        let Some(decision) = item.decision.as_ref() else {
            return true;
        };
        let id = &item.id;
        let state = self.runs.get(id).map(|r| r.state);
        let by = decision.by.as_deref().unwrap_or("a person");
        if state == Some(ItemState::AwaitingPlaytest) {
            let notes = decision.notes.clone().unwrap_or_default();
            let Some(notes) = self.with_site_pictures(links, id, &decision.pictures, notes).await else {
                return false;
            };
            tracing::info!("{}: {id} was tried out by {by} on the site", self.id);
            self.playtest_answer(id, decision.result.as_deref() == Some("pass"), Some(notes));
            return true;
        }
        if self.leases.contains_key(id)
            || !state.is_some_and(|s| waits_for_a_person(s) || s == ItemState::NeedsClarification)
        {
            return true;
        }
        tracing::info!(
            "{}: {id} was tried out by {by} on the site but isn't awaiting one here ({state:?}); claimed back to clear it",
            self.id
        );
        // Only the claim matters: it makes the store forget the word. Another computer having it, or a refusal for good, ends it.
        !matches!(self.claim_one(id, links).await, Claimed::Later)
    }

    /// A Start again or Carry on kept for an item stopped here with its lease held: queued as the command is.
    async fn site_send_back(&mut self, item: &Item, links: &Links) -> bool {
        let Some(send) = item.send_back.as_ref() else {
            return true;
        };
        let id = &item.id;
        let stopped = self.runs.get(id).is_some_and(|r| r.state == ItemState::NeedsHuman);
        if !stopped || !self.leases.contains_key(id) {
            return true;
        }
        let Some(notes) = self
            .with_site_pictures(links, id, &send.pictures, send.notes.clone().unwrap_or_default())
            .await
        else {
            return false;
        };
        let retry = send.how == "retry";
        tracing::info!(
            "{}: {id} was {} by {} on the site",
            self.id,
            if retry { "started again" } else { "carried on" },
            send.by.as_deref().unwrap_or("a person")
        );
        self.drop_claudes(id);
        self.queue_send_back(
            id.clone(),
            Some(notes),
            retry,
            if retry { "start over" } else { "continue" },
        );
        true
    }
}

/// The states in which an item run here waits for something and a word kept on it from the site can be for it.
fn waits_for_a_person(state: ItemState) -> bool {
    matches!(
        state,
        ItemState::NeedsClarification
            | ItemState::NeedsHuman
            | ItemState::AwaitingCi
            | ItemState::AwaitingPlaytest
            | ItemState::AwaitingMerge
    )
}
