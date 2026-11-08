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
