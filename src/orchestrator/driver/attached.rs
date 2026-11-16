//! The pictures a `continue`, `retry` or `playtest` carries (`pictureIds`, last-local-state.md sections 3.3 and 3.14;
//! `withPictures` in the TS agent's `commands.ts`): fetched from the store, kept in the pictures folder, and their
//! paths added after the notes in the "Attached images" block a session reads. The command went up as `pending`;
//! the answer goes up here, `ack` once the pictures are in, `nack` when the store won't give one.
use serde_json::json;

use super::Links;
use crate::ask::pictures;
use crate::backlog::Client;
use crate::view::Attached;

/// What a refused fetch says, as the TS agent does.
const FETCH_FAILED: &str = "Couldn't fetch a picture from the shared backlog";

/// The command's notes with its pictures' paths after them, and the command answered. With none attached, the notes
/// as they are and nothing sent. `None` when the pictures couldn't be had: the command is refused and is not carried
/// out. (A free function, so the loop isn't held across the fetch.)
pub(super) async fn notes_with_pictures(
    (client, repo): (&Client, Option<u64>),
    project: &str,
    links: &Links,
    notes: Option<String>,
    attached: Option<Attached>,
) -> Option<Option<String>> {
    let Some(Attached { command_id, ids }) = attached else {
        return Some(notes);
    };
    let nack = |why: String| {
        links
            .item_commands
            .answer(project, json!({ "type": "nack", "id": command_id, "reason": why }));
        None
    };
    let Some(repo) = repo else {
        return nack(format!("{FETCH_FAILED}: this project isn't linked to one."));
    };
    let token = match links.store_token().await {
        Ok(Some(token)) => token,
        Ok(None) => return nack(format!("{FETCH_FAILED}: this computer isn't linked to it.")),
        Err(e) => return nack(format!("{FETCH_FAILED}: {e:#}")),
    };
    match pictures::fetch(client, repo, Some(&token), &ids, &pictures::folder()).await {
        Ok(saved) => {
            links
                .item_commands
                .answer(project, json!({ "type": "ack", "id": command_id }));
            let text = pictures::with_images(notes.as_deref().unwrap_or(""), &saved.keep());
            Some((!text.trim().is_empty()).then_some(text))
        }
        Err(e) => nack(format!("{FETCH_FAILED}: {e}")),
    }
}
