//! The release note's public page on the agent's own retaskable.com (`publish` and `unpublish` in the TS agent's
//! `releaseNotes.ts` and `publish.ts`, release-notes.md section 8.1): **Publish** puts the note's Markdown at a page of
//! `<cloud>/api/agents/releases/<id>`, a new id the first time and the same one after (Update the public page), and answers
//! the page's address; **Unpublish** deletes it, so it answers 404 from then on.
//!
//! The note is the store's row `release_note` (`{ note }`), read when the command comes: this computer keeps no copy. What it
//! writes back is the page's state, `published: { id, url, at, markdown }`, listed with the kept facts
//! ([`KeptFacts::keep_published`]) so a store that can't be reached just then is still told, and only while the note is still
//! the one the page was made for. Refused in TS's words: a note still being built ("Wait for the note to finish."; one
//! building for 30 minutes counts as lost, `withoutStaleBuild`), none ready, one over 200,000 characters, a second
//! publish in flight, an agent not linked to retaskable.com, or one that can't reach it.
use std::collections::BTreeSet;
use std::sync::{LazyLock, Mutex};

use serde_json::{Value, json};

use crate::kept_facts::{FactStore, KeptFacts, RELEASE_NOTE};
use crate::link::TokenAnswer;
use crate::view::CloudToken;

/// `MAX_PUBLISHED_CHARS` (shared/releaseNote.ts).
pub const MAX_PUBLISHED_CHARS: usize = 200_000;
/// A note still gathering or writing this long after it started is taken as lost (`STALE_BUILD_MS`).
const STALE_BUILD_MS: i64 = 30 * 60_000;
/// How long the cloud has to answer a page's request.
const PAGE_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// The projects with a publish in flight, so one is never made twice at once.
static PUBLISHING: LazyLock<Mutex<BTreeSet<String>>> = LazyLock::new(Default::default);

/// Takes the project's turn to publish, or `None` while one is under way. Given back when dropped.
struct Turn(String);

impl Turn {
    fn take(project: &str) -> Option<Self> {
        let mut busy = PUBLISHING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        busy.insert(project.to_string()).then(|| Self(project.to_string()))
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        PUBLISHING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&self.0);
    }
}

/// `publishedId()`: 12 characters of letters and digits.
pub fn published_id() -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    (0..12)
        .map(|_| CHARS[rand::random::<u8>() as usize % CHARS.len()] as char)
        .collect()
}

/// The store's note, or the words it can't be had in: `currentNote`, with a build over 30 minutes old taken as failed.
async fn current_note(store: &dyn FactStore, now_ms: i64) -> Result<Option<Value>, String> {
    let row = store
        .get(RELEASE_NOTE)
        .await
        .map_err(|e| format!("Couldn't read the release note from the shared backlog: {e}"))?;
    let Some(mut note) = row
        .and_then(|(data, _)| data.get("note").cloned())
        .filter(Value::is_object)
    else {
        return Ok(None);
    };
    let building = matches!(note["state"].as_str(), Some("gathering" | "writing"));
    let started = note["generatedAt"]
        .as_str()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok());
    if building && started.is_some_and(|s| now_ms - s.timestamp_millis() > STALE_BUILD_MS) {
        note["state"] = json!("failed");
        note["error"] = json!("The computer building it stopped. Generate it again.");
    }
    Ok(Some(note))
}

/// The cloud's address and token, or why there is none.
async fn linked(link: &dyn CloudToken) -> Result<(String, String), String> {
    match link.answer().await {
        TokenAnswer::Token { url, access_token } => Ok((url, access_token)),
        TokenAnswer::Idle => Err("Link this agent to retaskable.com first, in Profile.".into()),
        TokenAnswer::Unreachable => Err("Couldn't reach retaskable.com: try again in a minute.".into()),
    }
}

/// `releaseUrl`: the page's request address, with the project it is for.
fn page_url(base: &str, project: &str, id: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(&format!("{}/api/agents/releases/{id}", base.trim_end_matches('/')))
        .map_err(|e| e.to_string())?;
    if !project.is_empty() {
        url.query_pairs_mut().append_pair("project", project);
    }
    Ok(url)
}

/// What a refused request says (`answer` in publish.ts): the status and the cloud's own `error`.
async fn refused(res: reqwest::Response) -> String {
    let status = res.status().as_u16();
    let error = res.json::<Value>().await.ok().and_then(|j| {
        j.get("error")
            .map(|e| e.as_str().map_or_else(|| e.to_string(), str::to_string))
    });
    format!("{status} {}", error.unwrap_or_default()).trim().to_string()
}

fn iso_now(now_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(now_ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// **Publish**: the page's address, or why not.
pub async fn publish(
    store: &dyn FactStore,
    facts: &KeptFacts,
    project: &str,
    link: &dyn CloudToken,
    http: &reqwest::Client,
    now_ms: i64,
) -> Result<Value, String> {
    let note = current_note(store, now_ms).await?;
    let building = note
        .as_ref()
        .is_some_and(|n| matches!(n["state"].as_str(), Some("gathering" | "writing")));
    if building {
        return Err("Wait for the note to finish.".into());
    }
    let ready = note
        .as_ref()
        .filter(|n| n["state"] == "ready" && !n["markdown"].as_str().unwrap_or("").trim().is_empty());
    let Some(note) = ready else {
        return Err("There's no note ready to publish.".into());
    };
    let markdown = note["markdown"].as_str().unwrap_or_default();
    // JavaScript's `length`: UTF-16 units.
    if markdown.encode_utf16().count() > MAX_PUBLISHED_CHARS {
        return Err("A note can be at most 200,000 characters to publish.".into());
    }
    let Some(_turn) = Turn::take(project) else {
        return Err("It's already being published.".into());
    };
    let (base, token) = linked(link).await?;
    let id = note["published"]["id"]
        .as_str()
        .map_or_else(published_id, str::to_string);
    let put = async {
        let res = http
            .put(page_url(&base, project, &id)?)
            .bearer_auth(&token)
            .timeout(PAGE_LIMIT)
            .json(&json!({ "markdown": markdown, "from": note["from"], "to": note["to"] }))
            .send()
            .await
            .map_err(|e| crate::http_client::chain(&e))?;
        if !res.status().is_success() {
            return Err(refused(res).await);
        }
        let json: Option<Value> = res.json().await.ok();
        json.and_then(|j| j.get("url").and_then(Value::as_str).map(str::to_string))
            .ok_or_else(|| "retaskable.com didn't answer the page's address".to_string())
    };
    let url = put.await.map_err(|e| format!("Couldn't publish it: {e}"))?;
    let published = json!({ "id": id, "url": url, "at": iso_now(now_ms), "markdown": markdown });
    let generated = note["generatedAt"].as_str().unwrap_or_default();
    facts.keep_published(generated, &id, Some(&published));
    facts.flush(store).await;
    Ok(json!({ "url": url }))
}

/// **Unpublish**: the page is deleted and the note says it isn't published.
pub async fn unpublish(
    store: &dyn FactStore,
    facts: &KeptFacts,
    project: &str,
    link: &dyn CloudToken,
    http: &reqwest::Client,
    now_ms: i64,
) -> Result<(), String> {
    let note = current_note(store, now_ms).await?;
    let Some((note, id)) = note.and_then(|n| {
        let id = n["published"]["id"].as_str()?.to_string();
        Some((n, id))
    }) else {
        return Err("The note isn't published.".into());
    };
    let (base, token) = linked(link).await?;
    let delete = async {
        let res = http
            .delete(page_url(&base, project, &id)?)
            .bearer_auth(&token)
            .timeout(PAGE_LIMIT)
            .send()
            .await
            .map_err(|e| crate::http_client::chain(&e))?;
        if res.status().is_success() {
            Ok(())
        } else {
            Err(refused(res).await)
        }
    };
    delete.await.map_err(|e| format!("Couldn't unpublish it: {e}"))?;
    facts.keep_published(note["generatedAt"].as_str().unwrap_or_default(), &id, None);
    facts.flush(store).await;
    Ok(())
}

#[cfg(test)]
mod tests;
