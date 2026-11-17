//! Importing issues as plan requests (issue-import.md sections 3.2 and 3.5; `agent/src/issues/import.ts`): no LLM call,
//! only `gh`, Linear's or Jira's API. At start, every 10 minutes and when the project's `issues` setting changes, each new
//! issue becomes an imported row in the store (`draft_queue`) that waits for a person's Plan this. Show matches
//! (`read_kinds/issues.rs`, section 3.6) reads the same list with the same skip rules, and imports nothing.
use std::collections::HashSet;

use serde_json::{Value, json};

use super::config::{IssuesConfig, source};
use super::{Issue, IssueRef, Job};
use crate::backlog::access;
use crate::backlog::{Draft, LockAnswer};
use crate::text::truncate;

pub const IMPORT_EVERY_MS: u64 = 10 * 60_000;
/// At most this many plan requests wait at once per project; past that, new issues wait for the next check
/// (`MAX_WAITING`).
pub const MAX_WAITING: usize = 20;
/// The store's lock that keeps two computers from importing the same issues at once. It is kept after a check, as the
/// team's cooldown: another computer takes it over only once [`IMPORT_EVERY_MS`] has passed since the last take.
pub const IMPORT_LOCK: &str = "issue-import";
/// How much of an issue's text a plan request keeps.
const ISSUE_BODY_MAX: usize = 8000;
/// A plan request's text (the store's `DRAFT_REQUEST_MAX`) and a title (`importDraft`).
const REQUEST_MAX: usize = 20_000;
const TITLE_MAX: usize = 300;

/// What a check does with one issue: brings it in, or skips it as one a draft already names or one whose author isn't
/// trusted while Anyone's issues is off (`IssueComes`). Importing and Show matches both ask this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comes {
    Yes,
    Seen,
    Untrusted,
}

impl Comes {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::Seen => "seen",
            Self::Untrusted => "untrusted",
        }
    }
}

/// `importable`: whether an issue by this `ref_key`, trusted or not, comes in.
pub fn comes(ref_key: &str, trusted: bool, named: &HashSet<String>, anyone: bool) -> Comes {
    if named.contains(ref_key) {
        Comes::Seen
    } else if trusted || anyone {
        Comes::Yes
    } else {
        Comes::Untrusted
    }
}

/// The issues any draft names (discarded ones too), and the plan requests waiting (`known`).
pub fn known(drafts: &[Draft]) -> (HashSet<String>, usize) {
    let named = drafts
        .iter()
        .filter_map(|d| IssueRef::of(d.issue.as_ref()?))
        .map(|r| r.ref_key())
        .collect();
    let waiting = drafts
        .iter()
        .filter(|d| d.detail.as_deref() == Some("imported") && d.status.as_deref() == Some("needs_person"))
        .count();
    (named, waiting)
}

/// The `draft_queue` arguments of a plan request from an issue (`importDraft`): a row no computer holds, which waits for a
/// person's Plan this before any drafter starts.
pub fn draft_args(i: &Issue) -> Value {
    let request = format!("{}\n\n{}", i.title.trim(), truncate(i.body.trim(), ISSUE_BODY_MAX));
    let title = truncate(i.title.trim(), TITLE_MAX);
    let mut issue = json!({ "provider": i.issue.provider.as_str(), "key": i.issue.key, "url": i.issue.url });
    if !i.author.is_empty() {
        issue["author"] = json!(i.author);
    }
    let by = if i.author.is_empty() {
        String::new()
    } else {
        format!(" by @{}", i.author)
    };
    json!({
        "imported": true,
        "request": truncate(&request, REQUEST_MAX),
        "titles": [if title.is_empty() { i.issue.key.clone() } else { title }],
        "issue": issue,
        "intake": { "note": format!("Imported from issue {}{by}", i.issue.key) },
    })
}

/// How a check ended, for the page and the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    /// Not this computer's turn, or the store wasn't reachable: nothing was checked.
    Skipped,
    /// Checked; the issues imported.
    Imported(Vec<String>),
    /// The tracker couldn't be read: the words the page shows.
    Failed(String),
}

/// One check: each new issue from a trusted author (anyone's with `anyone`) becomes a plan request, until
/// [`MAX_WAITING`] wait. An issue any draft names, discarded ones included, is skipped. `fresh` takes the lock at once,
/// whoever checked last (a changed setting); otherwise a computer waits for the team's cooldown.
///
/// The store being down checks nothing and starts nothing (docs/specs/agent/07.5): a lock that can't be taken is the
/// store's own word that it is not this computer's turn. A store that fails mid-way ends the check there; only a
/// tracker that can't be read is the page's error (the TS agent also shows a store failure as "Couldn't read GitHub
/// issues", which blames the wrong side).
pub async fn import_issues(job: &Job, issues: &IssuesConfig, fresh: bool) -> Checked {
    let Some(repo) = job.repo else {
        return Checked::Skipped;
    };
    let token = match access::current(&*job.access).await {
        Ok(Some(token)) => token,
        Ok(None) => return Checked::Skipped,
        Err(e) => {
            tracing::warn!("{}: issues: reading the backlog link: {e:#}", job.id);
            return Checked::Skipped;
        }
    };
    let stale_ms = if fresh { 0 } else { IMPORT_EVERY_MS };
    let lock = job
        .client
        .op::<LockAnswer>(
            repo,
            Some(&token),
            "chore_lock",
            json!({ "name": IMPORT_LOCK, "take": true, "staleMs": stale_ms, "fresh": true }),
            "lock answer",
        )
        .await;
    match lock {
        Ok(l) if l.held => {}
        Ok(l) => {
            tracing::info!(
                "{}: issues: {}'s agent holds the {IMPORT_LOCK} lock, so this one leaves it to them",
                job.id,
                l.by.as_deref().unwrap_or("another agent")
            );
            return Checked::Skipped;
        }
        Err(e) => {
            tracing::warn!(
                "{}: issues: taking the {IMPORT_LOCK} lock failed, so nothing is imported: {e}",
                job.id
            );
            return Checked::Skipped;
        }
    }
    let drafts = match job.client.drafts(repo, Some(&token)).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                "{}: issues: reading the store's drafts failed, so nothing is imported: {e}",
                job.id
            );
            return Checked::Skipped;
        }
    };
    let name = issues.source.name();
    let src = source(&job.project, &job.gh, &job.keys, Some(issues), issues.source);
    let found = match src.list(&issues.filter()).await {
        Ok(f) => f,
        Err(why) => {
            let why = super::first_line(&why);
            tracing::warn!("{}: issues: couldn't read {name} issues: {why}", job.id);
            return Checked::Failed(format!("Couldn't read {name} issues: {why}"));
        }
    };
    let (mut named, mut waiting) = known(&drafts);
    let mut added = Vec::new();
    for issue in &found.issues {
        if waiting >= MAX_WAITING {
            break;
        }
        if comes(&issue.issue.ref_key(), issue.trusted, &named, issues.anyone) != Comes::Yes {
            continue;
        }
        // Each is in the store as soon as it is made, so the next computer to take the lock sees it.
        match job.client.draft_queue_args(repo, Some(&token), draft_args(issue)).await {
            Ok(_) => {
                named.insert(issue.issue.ref_key());
                waiting += 1;
                added.push(issue.issue.key.clone());
            }
            Err(e) => {
                tracing::warn!(
                    "{}: issues: the store didn't take {} as a plan request: {e}",
                    job.id,
                    issue.issue.key
                );
                break;
            }
        }
    }
    if !added.is_empty() {
        tracing::info!(
            "{}: issues: imported {} as plan requests",
            job.id,
            plural(added.len(), "issue")
        );
    }
    Checked::Imported(added)
}

pub(crate) fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}
