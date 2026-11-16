//! The `draft_wanted` kind (docs/specs/intent-rows.md section 4; `intentCommand` in the TS agent's `intentPuller.ts`,
//! then `commands.ts`): the site's Add spec (`draft_spec { request, audience?, trial?, pictures? }`) and Build this on
//! an idea (`idea_accept { idea }`) are one row kind. A row with an `idea` is the idea's `accept` (`ideas::accept`); the
//! rest is a spec written as `createDraft` does, a `draft_queue` in the store for the first computer with a free drafter
//! to claim. The row's result is the new draft's id. The requester and first assignee are the row's `issuedBy`.
//!
//! A paused project, and a computer signed out of Claude, take none of these (the puller leaves the row open for another
//! computer), as do two such rows already running here.
use std::sync::Arc;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use super::{Handler, Outcome};
use crate::ask::pictures;
use crate::backlog::Client;
use crate::backlog::intents::IntentRow;
use crate::ideas::{self, Ideas};

/// A request is at most this many characters (`DRAFT_REQUEST_MAX`).
const REQUEST_MAX: usize = 20_000;
/// A draft's title is at most this many (`createDraft`).
const TITLE_MAX: usize = 300;
/// An idea's id is at most this many characters (`idea_accept`'s schema).
const IDEA_MAX: usize = 80;
/// The drafts that are open: a trial change is made once while one of these is.
const OPEN: &[&str] = &["queued", "drafting", "needs_person"];
/// The tag a chore's request starts with, and the chore it says started it (`choreDraftKind`).
const CHORE_TAGS: &[(&str, &str)] = &[("[Chore]", "specs"), ("[Lighthouse]", "lighthouse"), ("[Scan]", "scan")];

/// What a handler needs once a row is won (this kind's, and the read kinds' in [`super::reads`]): the store, this
/// computer's token for it, where pictures are kept, and the project the row is for.
#[derive(Clone)]
pub struct RowCx {
    pub client: Arc<Client>,
    pub repo: u64,
    pub token: String,
    pub pictures: std::path::PathBuf,
    pub project: crate::config::ProjectConfig,
}

pub struct WantedHandler;

impl Handler<RowCx> for WantedHandler {
    fn kind(&self) -> &'static str {
        "draft_wanted"
    }

    fn starts_work(&self) -> bool {
        true
    }

    fn runs_session(&self) -> bool {
        true
    }

    fn run(&self, row: IntentRow, cx: &RowCx) -> BoxFuture<'static, Outcome> {
        let cx = cx.clone();
        async move { wanted(&cx, &row).await }.boxed()
    }
}

/// Who the draft is for, as the row names it (`audience`): not named, nobody (`null`) or an audience's id.
enum Audience {
    Unnamed,
    Nobody,
    Is(String),
}

/// What a row's `request`, `audience`, `trial` and `pictureIds` say, checked as the command's schema checks them
/// (`draft_spec` in `shared/cloudCommands.ts`); `Err` is the words the row is refused with.
struct Spec {
    request: String,
    audience: Audience,
    trial: bool,
    pictures: Vec<String>,
}

fn spec_of(args: &Value) -> Result<Spec, String> {
    let request = match args.get("request") {
        Some(Value::String(r)) if r.chars().count() <= REQUEST_MAX => r.clone(),
        Some(Value::String(_)) => {
            return Err(format!(
                "request: String must contain at most {REQUEST_MAX} character(s)"
            ));
        }
        _ => return Err("request: Required".into()),
    };
    let audience = match args.get("audience") {
        None => Audience::Unnamed,
        Some(Value::Null) => Audience::Nobody,
        Some(Value::String(a)) if is_audience_id(a) => Audience::Is(a.clone()),
        Some(_) => return Err("audience: Invalid".into()),
    };
    let trial = match args.get("trial") {
        None => false,
        Some(Value::Bool(true)) => true,
        Some(_) => return Err("trial: Invalid literal value, expected true".into()),
    };
    // The hub writes the ids as `pictures` (`INTENT_COMMANDS`); the command names them `pictureIds`.
    let listed = args.get("pictures").or_else(|| args.get("pictureIds"));
    let pictures = match listed {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(list)) if list.len() <= crate::commands::MAX_PICTURES => list
            .iter()
            .map(|p| match p.as_str() {
                Some(id) if pictures::valid(id) => Ok(id.to_string()),
                _ => Err("pictureIds: Invalid".to_string()),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("pictureIds: Invalid".into()),
    };
    Ok(Spec {
        request,
        audience,
        trial,
        pictures,
    })
}

/// An audience's id: 1 to 40 of lowercase letters, digits and hyphens.
fn is_audience_id(a: &str) -> bool {
    (1..=40).contains(&a.len()) && a.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-'))
}

/// The chore a request says started it, if any.
fn chore_of(request: &str) -> Option<&'static str> {
    CHORE_TAGS
        .iter()
        .find(|(tag, _)| request.starts_with(tag))
        .map(|(_, c)| *c)
}

/// What the store's refusal or silence says, as `storeOp` words it: nothing is queued while it can't be reached
/// (fail closed).
fn store_words(e: &crate::backlog::BacklogError) -> Outcome {
    if e.code() == Some("conflict") {
        return Outcome::Refused(e.to_string());
    }
    Outcome::Failed(format!(
        "Can't reach the shared backlog, so no new spec starts until it can be: {e}"
    ))
}

/// Carries out one `draft_wanted` row.
pub async fn wanted(cx: &RowCx, row: &IntentRow) -> Outcome {
    let by = row.issued_by.as_deref().unwrap_or("");
    if let Some(idea) = row.args.get("idea").and_then(Value::as_str) {
        if idea.chars().count() > IDEA_MAX {
            return Outcome::Refused(format!("idea: String must contain at most {IDEA_MAX} character(s)"));
        }
        let ideas = Ideas {
            client: &cx.client,
            repo_id: cx.repo,
            token: Some(&cx.token),
        };
        return match ideas::accept(&ideas, idea, by).await {
            Ok(()) => Outcome::Done(Value::Null),
            Err(reason) => Outcome::Refused(reason),
        };
    }
    let spec = match spec_of(&row.args) {
        Ok(s) => s,
        Err(why) => return Outcome::Refused(why),
    };
    match create(cx, spec, by).await {
        Ok(id) => Outcome::Done(json!({ "id": id })),
        Err(done) => done,
    }
}

/// `createDraft` for a person's request: the pictures fetched and listed after it, the audience checked against the
/// company's list, a trial change made once while one is open, then the draft queued in the store. Answers the draft's id.
async fn create(cx: &RowCx, spec: Spec, by: &str) -> Result<String, Outcome> {
    let mut request = spec.request;
    if !spec.pictures.is_empty() {
        let saved = pictures::fetch(&cx.client, cx.repo, Some(&cx.token), &spec.pictures, &cx.pictures)
            .await
            .map_err(|e| Outcome::Failed(format!("Couldn't fetch a picture from the shared backlog: {e}")))?;
        request = pictures::with_images(&request, &saved.keep());
    }
    if request.trim().is_empty() {
        return Err(Outcome::Refused("Describe what the spec should cover.".into()));
    }
    if let Audience::Is(named) = &spec.audience {
        let listed = (cx.client.audiences(cx.repo, Some(&cx.token)).await).map_err(|e| store_words(&e))?;
        if !listed.iter().any(|a| &a.id == named) {
            return Err(Outcome::Refused("That audience is not on this company's list.".into()));
        }
    }
    if spec.trial {
        let drafts = (cx.client.drafts(cx.repo, Some(&cx.token)).await).map_err(|e| store_words(&e))?;
        let open = drafts.iter().find(|d| {
            d.intake
                .as_ref()
                .and_then(|i| i.get("trial"))
                .is_some_and(|t| t.as_bool() == Some(true))
                && d.status.as_deref().is_some_and(|s| OPEN.contains(&s))
        });
        if let Some(open) = open {
            return Ok(open.id.clone());
        }
    }
    let queued = queued(&request, &spec.audience, spec.trial, by);
    (cx.client.draft_queue_args(cx.repo, Some(&cx.token), queued).await).map_err(|e| store_words(&e))
}

/// The `draft_queue` arguments `createDraft` sends: the request cut to its limit, its first line as the title until the
/// drafter gives it one, who asked (a GitHub login, else nobody), the audience only when one was named, the chore a
/// tag names, and an intake that says the chat follows it (and that it is the trial change).
fn queued(request: &str, audience: &Audience, trial: bool, by: &str) -> Value {
    let first = request.trim().lines().next().unwrap_or("").trim();
    let title = crate::text::truncate(first, TITLE_MAX);
    let mut args = json!({
        "request": crate::text::truncate(request.trim(), REQUEST_MAX),
        "titles": [if title.trim().is_empty() { "Spec draft".to_string() } else { title }],
        "intake": if trial { json!({ "trial": true, "chat": true }) } else { json!({ "chat": true }) },
    });
    if ideas::accept::is_login(by) {
        args["requestedBy"] = json!(by);
        args["assignee"] = json!(by);
    }
    match audience {
        Audience::Unnamed => {}
        Audience::Nobody => args["audience"] = Value::Null,
        Audience::Is(a) => args["audience"] = json!(a),
    }
    if let Some(chore) = chore_of(request) {
        args["chore"] = json!(chore);
    }
    args
}

#[cfg(test)]
mod tests;
