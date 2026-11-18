//! A change approved on a card revises that card's item (docs/specs/agent/06.5-spec-drafts.md, "Revisions";
//! Retaskable.com's card-revisions.md section 3.4, `agent/src/revisions.ts`): once the plan merges the item goes back
//! to the queue with the plan's remaining work. `revise` is the store's one write for it, keyed by the plan's id, so a
//! repeat changes nothing; `item` reads the one item a plan names.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{BacklogError, Client};

/// A revision the store hasn't taken yet: what `revise` carries besides the item's id, kept in the state file by
/// plan id (`Landing::revisions`) and sent again until the store answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owed {
    /// The item the plan revises.
    pub item: String,
    /// What is left of it once the change is in.
    pub remaining: String,
    /// The plan's title, for the item's history line.
    pub title: String,
    /// Who asked for the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// When it was first listed (epoch seconds).
    pub since: i64,
}

/// What the store did with a `revise`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Revised {
    /// The item is back in the queue with the change.
    Applied,
    /// A slice is in hand: the change applies once it ends.
    Waiting,
    /// This plan was applied already: nothing changed.
    Repeat,
}

#[derive(Deserialize)]
struct Answer {
    revised: Revised,
}

impl Client {
    /// `revise { id, remaining, draftId, by?, title }`: the plan `draft_id` merged, so item `id` takes `remaining` in
    /// place of what was left (`reviseItem` in the store's `shared/revise.ts`).
    pub async fn revise(
        &self,
        repo_id: u64,
        token: Option<&str>,
        draft_id: &str,
        owed: &Owed,
    ) -> Result<Revised, BacklogError> {
        let mut args = json!({
            "id": owed.item,
            "remaining": owed.remaining,
            "draftId": draft_id,
            "title": crate::text::truncate(owed.title.trim(), 300),
        });
        if let Some(by) = &owed.by {
            args["by"] = json!(by);
        }
        let answer: Answer = self.op(repo_id, token, "revise", args, "revision").await?;
        Ok(answer.revised)
    }

    /// `list { ids: [id] }`: the one item, as the store shows it, or `None` when there is none. An error is "couldn't
    /// ask", never "no such item": a plan never goes ahead blind to its item (backlog.md section 8.1).
    pub async fn item(&self, repo_id: u64, token: Option<&str>, id: &str) -> Result<Option<Value>, BacklogError> {
        let items: Vec<Value> = self.op(repo_id, token, "list", json!({ "ids": [id] }), "item").await?;
        Ok(items.into_iter().next())
    }

    /// `reopen { id, notes, by? }`: a fix asked on a card reopens the card's item, when the store allows it (nobody on
    /// it, built or waiting). `false` when it declined.
    pub async fn reopen_for_fix(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        notes: &str,
        by: Option<&str>,
    ) -> Result<bool, BacklogError> {
        let mut args = json!({ "id": id, "notes": crate::text::truncate(notes.trim(), 4000) });
        if let Some(by) = by {
            args["by"] = json!(by);
        }
        let answer: Value = self.op(repo_id, token, "reopen", args, "reopen answer").await?;
        Ok(answer.get("reopened").and_then(Value::as_bool) == Some(true))
    }
}
