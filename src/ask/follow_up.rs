//! Retaskable's own messages in the chat (docs/specs/agent/06.6-ask-claude.md, "Retaskable's own messages"). They are
//! written here, in code, never by a model.

/// A draft a person asked for is written and waits for their yes.
pub const WRITTEN: &str = "The spec is written. Do you want to approve it?";

/// The drafter needs answers before it can write the spec.
pub const HAS_QUESTIONS: &str = "The drafter has questions before it can write this up. Can you answer them?";

/// The draft is approved and merged: `titles` are the chat's parsed titles when there is one per item.
pub fn merged(item_count: usize, titles: &[String]) -> String {
    match (item_count, titles.len() == item_count) {
        (0, _) => "Approved and merged.".into(),
        (1, true) => format!("Approved and merged. 1 item is queued: {}.", titles[0]),
        (n, true) => format!("Approved and merged. {n} items are queued: {}.", titles.join(", ")),
        (1, false) => "Approved and merged. 1 item is queued.".into(),
        (n, false) => format!("Approved and merged. {n} items are queued."),
    }
}

/// Approved, with the pattern that waits for a person to merge on the code host.
pub const WAITING_FOR_MERGE: &str = "Approved. It's waiting for someone to merge it on GitHub.";

/// An item the draft added is first worked on.
pub fn started(title: &str) -> String {
    format!("Started building {title}.")
}

/// An item the draft added stopped for a person.
pub fn needs_you(title: &str) -> String {
    format!("{title} needs you. See its card.")
}

/// An item the draft added had a slice merged and has more to do.
pub fn merged_in_part(title: &str) -> String {
    format!("{title}: a part of it was merged. More to come.")
}

/// An item the draft added is done.
pub fn built(title: &str) -> String {
    format!("{title} is built and merged.")
}

/// An item the draft added was skipped.
pub fn skipped(title: &str) -> String {
    format!("{title} was skipped.")
}

/// Every item is in and the project has no staging that serves the base branch.
pub const NO_STAGING: &str = "There's no staging link set up for this project.";

/// Staging hasn't served the merge after 20 minutes.
pub const STAGING_LATE: &str = "It's merged, but staging hasn't picked it up yet.";

/// Staging serves the merge.
pub fn on_staging(url: &str) -> String {
    format!("It's on staging: {url}")
}

/// Posts `text` about `draft_id` unless the conversation already has it (so a restart, or a second pass over the same
/// state, says nothing twice), into the conversation of the draft's latest message, else of `ask_id`, else a new one.
/// Answers whether it posted.
pub async fn post_once(
    client: &crate::backlog::Client,
    repo: u64,
    token: &str,
    draft_id: &str,
    ask_id: Option<&str>,
    text: &str,
) -> Result<bool, crate::backlog::BacklogError> {
    let asks = client.asks_list(repo, Some(token)).await?;
    let mine = || {
        asks.iter()
            .filter(|a| a.from.as_deref() == Some("retaskable") && a.draft_id.as_deref() == Some(draft_id))
    };
    if mine().any(|a| a.text == text) {
        return Ok(false);
    }
    let reply_to = mine().next_back().map(|a| a.id.as_str()).or(ask_id);
    client.ask_post(repo, Some(token), text, draft_id, reply_to).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::Client;
    use crate::backlog::tests::store;
    use serde_json::json;

    fn titles(t: &[&str]) -> Vec<String> {
        t.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn merged_names_the_items_only_when_there_is_one_title_each() {
        assert_eq!(merged(0, &[]), "Approved and merged.");
        assert_eq!(merged(0, &titles(&["stray"])), "Approved and merged.");
        assert_eq!(merged(1, &titles(&["A"])), "Approved and merged. 1 item is queued: A.");
        assert_eq!(
            merged(2, &titles(&["A", "B"])),
            "Approved and merged. 2 items are queued: A, B."
        );
        assert_eq!(merged(1, &[]), "Approved and merged. 1 item is queued.");
        assert_eq!(
            merged(3, &titles(&["A", "B"])),
            "Approved and merged. 3 items are queued."
        );
    }

    #[test]
    fn item_messages_carry_the_title() {
        assert_eq!(started("X"), "Started building X.");
        assert_eq!(needs_you("X"), "X needs you. See its card.");
        assert_eq!(merged_in_part("X"), "X: a part of it was merged. More to come.");
        assert_eq!(built("X"), "X is built and merged.");
        assert_eq!(skipped("X"), "X was skipped.");
        assert_eq!(on_staging("https://s.test"), "It's on staging: https://s.test");
    }

    #[tokio::test]
    async fn post_once_says_nothing_twice() {
        let (base, seen) = store(
            200,
            json!({ "value": { "id": "new", "asks": [
                { "id": "a1", "text": "Done.", "from": "retaskable", "draftId": "d1" },
                { "id": "a2", "text": "Done.", "from": "person", "draftId": "d2" } ] } }),
        )
        .await;
        let client = Client::new(base);
        assert!(!post_once(&client, 1, "t", "d1", None, "Done.").await.unwrap());
        assert_eq!(seen.lock().unwrap().len(), 1, "only the list was read");
        // A person's message, or another draft's, doesn't count as already said.
        assert!(post_once(&client, 1, "t", "d2", None, "Done.").await.unwrap());
    }

    #[tokio::test]
    async fn post_once_replies_into_the_drafts_latest_message_else_the_ask() {
        let (base, seen) = store(
            200,
            json!({ "value": { "id": "new", "asks": [
                { "id": "a1", "text": "one", "from": "retaskable", "draftId": "d1" },
                { "id": "a2", "text": "two", "from": "retaskable", "draftId": "d1" } ] } }),
        )
        .await;
        let client = Client::new(base);
        assert!(post_once(&client, 1, "t", "d1", Some("ask"), "three").await.unwrap());
        assert!(post_once(&client, 1, "t", "d9", Some("ask"), "x").await.unwrap());
        assert!(post_once(&client, 1, "t", "d9", None, "y").await.unwrap());
        let seen = seen.lock().unwrap();
        let reply = |i: usize| seen[i]["args"].get("replyTo").cloned();
        assert_eq!(reply(1), Some(json!("a2")));
        assert_eq!(reply(3), Some(json!("ask")));
        assert_eq!(reply(5), None);
    }
}
