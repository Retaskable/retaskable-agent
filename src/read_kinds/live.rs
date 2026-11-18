//! `go_live { url, branch? }` (`verifyLive` in the TS agent's `goLive.ts`, go-live.md section 3): the walkthrough's last step.
//! The live address, checked to answer, as the address to save: a plain GET that follows redirects and gets a 2xx within 15
//! seconds. Nothing deploys and nothing is saved here (a project's `deploy` is the team's, in the store: the page saves the
//! address there after this answers). Refused in words: not a web address, a branch with spaces, an address that doesn't answer.
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde_json::{Value, json};

use super::{ReadError, Reading};

/// How long the live address has to answer.
const CHECK: Duration = Duration::from_secs(15);

static SCHEME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[a-z][a-z0-9+.-]*:").unwrap());

/// `liveAddress` (shared/hosting.ts): the address as typed, as a URL (`https://` added when it has no scheme); `None` when it
/// isn't an http or https address on a named host (`my.github.io`, not `localhost`). A bare site is its origin, else its address.
pub fn live_address(raw: &str) -> Option<String> {
    let typed = raw.trim();
    if typed.is_empty() || typed.contains(char::is_whitespace) {
        return None;
    }
    let candidate = if SCHEME.is_match(typed) {
        typed.to_string()
    } else {
        format!("https://{typed}")
    };
    let url = reqwest::Url::parse(&candidate).ok()?;
    if !matches!(url.scheme(), "http" | "https") || !url.host_str()?.contains('.') {
        return None;
    }
    Some(if url.path() == "/" && url.query().is_none_or(str::is_empty) {
        url.origin().ascii_serialization()
    } else {
        url.to_string()
    })
}

const BUILDING: &str = "If the provider is still building it, wait a minute and check again.";

/// `checkLive`: `None` when `url` answers a plain GET (after redirects) with a 2xx, else why not, in words.
pub async fn check_live(url: &str) -> Option<String> {
    let http = crate::http_client::build(CHECK);
    match http.get(url).send().await {
        Ok(res) if res.status().is_success() => None,
        Ok(res) => Some(format!("{url} answered {}. {BUILDING}", res.status().as_u16())),
        Err(e) => Some(format!(
            "Couldn't reach {url}: {}. {BUILDING}",
            crate::http_client::chain(&e)
        )),
    }
}

/// `go_live`: the address checked, answered as `{ url }`.
pub async fn go_live(cx: &Reading, args: &Value) -> Result<Value, ReadError> {
    let text = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let Some(url) = live_address(&text("url")) else {
        return Err(ReadError::Refused(
            "That isn't a web address. Copy the one the provider shows, like https://you.github.io/your-site.".into(),
        ));
    };
    let configured = (cx.project.deploy.production.as_ref())
        .map(|p| p.branch.trim().to_string())
        .unwrap_or_default();
    let branch = [text("branch"), configured, cx.project.base_branch.clone()]
        .into_iter()
        .find(|b| !b.is_empty())
        .unwrap_or_default();
    if branch.contains(char::is_whitespace) {
        return Err(ReadError::Refused("A branch name has no spaces.".into()));
    }
    if let Some(why) = check_live(&url).await {
        return Err(ReadError::Refused(why));
    }
    Ok(json!({ "url": url }))
}

/// `confirm { itemId }`: Process, on an item the hub didn't find held in the store (it answers one that is with the store's own
/// `process`): the same op, here, so one the board shows another way still goes in. The store checks it is waiting for its yes
/// and refuses in its own words.
pub async fn confirm(cx: &Reading, args: &Value) -> Result<Value, ReadError> {
    let id = args
        .get("itemId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if id.is_empty() {
        return Err(ReadError::Refused("Process needs the item's id.".into()));
    }
    match cx
        .client
        .op_done(cx.repo, Some(&cx.token), "process", json!({ "id": id }))
        .await
    {
        Ok(()) => Ok(Value::Null),
        Err(crate::backlog::BacklogError::BadAnswer {
            message, code: Some(_), ..
        }) => Err(ReadError::Refused(message)),
        Err(e) => Err(ReadError::Failed(format!("Couldn't ask the shared backlog: {e}"))),
    }
}

#[cfg(test)]
mod tests;
