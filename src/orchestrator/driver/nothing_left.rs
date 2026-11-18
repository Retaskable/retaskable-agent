//! A claimed item's implementer answers `nothing_left` or `waits_on` (docs/specs/agent/06.2-implementer.md; `nothingLeft`
//! and `waitOn` in the TS agent's `steps.ts`). The agent reports the outcome and the store decides the rest.
//!
//! - **`nothing_left`.** On a shared backlog the item is built at the base branch's head: a change still open for its
//!   branch is closed unmerged with a comment saying why, the lane is freed, and the store gets the one `complete` write
//!   with the base head as the merge commit (what a merged slice sends), which also takes the run, its record and its
//!   lease. A chore stops with the session's words, and an item this computer holds no lease on, or whose base head
//!   can't be read, or whose `complete` the store doesn't take now, stops for a person ("Nothing left to build, so
//!   mark it done: ..."). A `complete` the store didn't take stays listed and is sent again (`outbox.rs`), so the item
//!   is finished when it does.
//! - **`waits_on`.** The store is told the item depends on the items named (`depends`, ids trimmed and deduplicated;
//!   the store refuses an id it doesn't know or a cycle) and the item is given back, unclaimed, noted with the
//!   session's summary. Without a lease to give back, with a change already open, for a chore, with no id named or when
//!   the store refuses, a person decides instead.
//!
//! Both answer `None` when they ended the item here, or `Some(reason)` for the person's stop.
use std::path::PathBuf;

use serde_json::json;

use super::Shared;
use super::project::ProjectLoop;
use crate::backlog::complete::Completion;
use crate::lane_pool::LanePool;
use crate::telemetry::{Event, event};

use super::outbox::Sent;

/// A change's number from its address (`.../pull/739`).
fn change_number(url: &str) -> Option<u64> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

impl ProjectLoop {
    /// What `nothing_left` came to: `None` when the item is built here, else why a person is needed.
    pub(super) async fn nothing_left(
        &mut self,
        pool: &mut LanePool,
        item: &str,
        why: &str,
        shared: &Shared<'_>,
    ) -> Option<String> {
        if self.chores.contains_key(item) {
            return Some(why.to_string());
        }
        let person = |text: String| Some(format!("{text}: {why}"));
        let Some(lease) = self.leases.get(item).cloned() else {
            return person("Nothing left to build, so mark it done".into());
        };
        let base = self.project.base_branch.clone();
        let unread = format!("Nothing left to build, but {base}'s head couldn't be read");
        let (Some(host), Some(repo), Ok(Some(token))) =
            (self.finishing.host.clone(), self.repo, shared.links.store_token().await)
        else {
            return person(unread);
        };
        let repo_path = PathBuf::from(&self.project.repo_path);
        let head = host
            .base_head(repo_path.clone(), self.project.github_repo.clone(), base.clone())
            .await;
        let (sha, on_default) = match head {
            Ok(head) => head,
            Err(e) => {
                tracing::warn!("{}: nothing left on {item}: {e:#}", self.id);
                return person(unread);
            }
        };
        let (branch, pr_url) = {
            let stored = self.items.get(item);
            let branch =
                (self.records.get(item).and_then(|r| r.branch.clone())).or(stored.and_then(|i| i.branch.clone()));
            (branch, stored.and_then(|i| i.pr_url.clone()))
        };
        // Closed unmerged, saying why; a failure is only logged (the item is built whatever the host says).
        if let Some(branch) = branch {
            let comment = crate::orchestrator::nothing_left::closing_comment(why);
            if let Err(e) = host.close_change(repo_path, branch, comment).await {
                tracing::warn!("{}: closing {item}'s change: {e:#}", self.id);
            }
        }
        let closed = pr_url.as_deref().and_then(change_number);
        let note = format!(
            "Nothing left to build{}: {why}",
            closed.map_or(String::new(), |n| format!(", so PR #{n} was closed"))
        );
        self.free_lane(pool, item, "merged", &note);
        self.measure_nothing_left(item);
        let completion = Completion {
            built: true,
            merge_commit: Some(sha),
            on_default_branch: on_default,
            remaining: Some(String::new()),
            playtest: None,
        };
        match self.complete_item(repo, &token, item, &lease, completion).await {
            Sent::Done(_) => {
                self.events.add_in(
                    &self.id,
                    Event::ItemState(event::ItemState {
                        item: Some(item.to_string()),
                        from: Some("implementing".into()),
                        to: Some("merged".into()),
                        note: Some(note),
                        ..Default::default()
                    }),
                );
                tracing::info!(
                    "{}: {item} has nothing left to build; completed at {base}'s head",
                    self.id
                );
                None
            }
            Sent::Kept(why_not) | Sent::Dropped(why_not) => person(format!(
                "Nothing left to build, but the shared backlog didn't take it ({why_not})"
            )),
        }
    }

    /// What `waits_on` came to: `None` when the store took the dependency and the item is given back, else why a person
    /// is needed.
    pub(super) async fn waits_on(
        &mut self,
        pool: &mut LanePool,
        item: &str,
        named: &[String],
        summary: &str,
        shared: &Shared<'_>,
    ) -> Option<String> {
        let mut ids: Vec<String> = Vec::new();
        for id in named.iter().map(|d| d.trim()).filter(|d| !d.is_empty()) {
            if !ids.iter().any(|seen| seen == id) {
                ids.push(id.to_string());
            }
        }
        let needs = format!(
            "The implementer found this item needs {} built first",
            if ids.is_empty() {
                "another item".to_string()
            } else {
                ids.join(", ")
            }
        );
        let open_change = self.items.get(item).is_some_and(|i| i.pr_url.is_some());
        let held = self.leases.contains_key(item) && !self.chores.contains_key(item);
        let (Some(repo), Ok(Some(token)), true) = (self.repo, shared.links.store_token().await, held && !open_change)
        else {
            return Some(format!("{needs}: {summary}"));
        };
        if ids.is_empty() {
            return Some(format!("{needs}: {summary}"));
        }
        if let Err(e) = (self.client)
            .op_done(repo, Some(&token), "depends", json!({ "id": item, "add": ids }))
            .await
        {
            return Some(format!("{needs}, but the backlog refused it ({e}): {summary}"));
        }
        let why = format!("Waits for {} to be built: {summary}", ids.join(", "));
        self.give_back_why(pool, item, &why, shared.links).await;
        None
    }
}
