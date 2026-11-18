//! A person's answer kept on the item (`decision { kind: "answer", answers, pictures }`, the store's `item_answer`; `actOnSite` in
//! the TS agent's `hostedWorkSite.ts`). The site and the MCP `answer` don't send this computer a command for an item parked
//! for its answer: the store keeps the answer on the item for the computer that holds it, and pushes `changed`. Each pass
//! reads the board, so an item this computer waits on for an answer, whose `decision` is there, has the answer queued as
//! an `answer` command's is (`take_answer`), and a restart finds it the same way. The store clears the decision when the
//! item is claimed back, so it is taken once; one queued already is not queued again. A picture that can't be had leaves it
//! for the next pass, as TS's failed `decided` does.
use super::{Links, ProjectLoop};
use crate::ask::pictures;
use crate::backlog::Item;
use crate::work_signal::Wake;

impl ProjectLoop {
    /// On a push that an item changed: reads the items this project waits on for an answer, so an answer given on the site is
    /// carried out now and not at the minute's pass. A read that fails is left to that pass.
    pub(super) async fn hear_site_answers(&mut self, links: &Links) {
        if !links.work.take(&self.id, Wake::Changed) {
            return;
        }
        let waiting: Vec<String> = (self.runs.keys())
            .filter(|id| self.awaits_answer(id))
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

    /// Queues the answer kept on each item that waits here for one.
    pub(super) async fn take_site_answers(&mut self, items: &[Item], links: &Links) {
        for item in items {
            let Some(decision) = item.decision.as_ref().filter(|d| d.kind == "answer") else {
                continue;
            };
            if !self.awaits_answer(&item.id) || self.answering.iter().any(|(id, _)| *id == item.id) {
                continue;
            }
            let mut answers = decision.answers.clone();
            if !decision.pictures.is_empty() {
                let (Some(repo), Ok(Some(token))) = (self.repo, links.store_token().await) else {
                    continue;
                };
                match pictures::fetch(
                    &self.client,
                    repo,
                    Some(&token),
                    &decision.pictures,
                    &pictures::folder(),
                )
                .await
                {
                    Ok(saved) => {
                        let last = answers.pop().unwrap_or_default();
                        answers.push(pictures::with_images(&last, &saved.keep()));
                    }
                    Err(e) => {
                        tracing::warn!(
                            "{}: the pictures with {}'s answer couldn't be had: {e}",
                            self.id,
                            item.id
                        );
                        continue;
                    }
                }
            }
            tracing::info!(
                "{}: {} was answered by {} on the site",
                self.id,
                item.id,
                decision.by.as_deref().unwrap_or("a person")
            );
            self.take_answer(item.id.clone(), answers);
        }
    }
}
