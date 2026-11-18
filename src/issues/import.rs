//! Reading issues for the store (issue-import.md sections 3.2, 3.6 and 3.7; `agent/src/issues/import.ts`): no LLM call, only
//! `gh`, Linear's or Jira's API. The store decides when to read, sends the `issues` job to a computer that has the tracker's
//! access (`can`) and makes the plan requests from what this answers: this computer holds no timer, no lock, no list of what
//! was imported and no cap. Show matches (`read_kinds/issues.rs`, section 3.6) reads the same list with the same skip rule
//! ([`comes`], the store's `issueComes`), and imports nothing.
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};

use super::config::{filter, site, source};
use super::keys::Keys;
use super::{Issue, IssueList, Provider, Source, first_line};
use crate::backlog::Draft;
use crate::jobs::{JobOutcome, JobSpec, Outcome};
use crate::text::truncate;

/// How much of an issue's text an answer keeps (`ISSUE_BODY_MAX`), and how many issues (`ISSUES_RESULT_MAX`).
const ISSUE_BODY_MAX: usize = 8000;
const ISSUES_RESULT_MAX: usize = 100;
const TITLE_MAX: usize = 300;
const AUTHOR_MAX: usize = 100;

/// What a check does with one issue: brings it in, or skips it as one a draft already names or one whose author isn't
/// trusted while Anyone's issues is off (`IssueComes`). The store and Show matches both ask this.
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

/// `issueComes`: whether an issue by this `ref_key`, trusted or not, comes in.
pub fn comes(ref_key: &str, trusted: bool, named: &HashSet<String>, anyone: bool) -> Comes {
    if named.contains(ref_key) {
        Comes::Seen
    } else if trusted || anyone {
        Comes::Yes
    } else {
        Comes::Untrusted
    }
}

/// The issues any draft names (discarded ones too), and the plan requests waiting, for Show matches.
pub fn known(drafts: &[Draft]) -> (HashSet<String>, usize) {
    let named = drafts
        .iter()
        .filter_map(|d| super::IssueRef::of(d.issue.as_ref()?))
        .map(|r| r.ref_key())
        .collect();
    let waiting = drafts
        .iter()
        .filter(|d| d.detail.as_deref() == Some("imported") && d.status.as_deref() == Some("needs_person"))
        .count();
    (named, waiting)
}

/// One issue as the `issues` job answers it: facts only (what it says and whether its author is trusted).
fn raw(i: &Issue) -> Value {
    json!({
        "ref": { "provider": i.issue.provider.as_str(), "key": i.issue.key, "url": i.issue.url },
        "title": truncate(&i.title, TITLE_MAX),
        "body": truncate(&i.body, ISSUE_BODY_MAX),
        "author": truncate(&i.author, AUTHOR_MAX),
        "trusted": i.trusted,
    })
}

/// The `issues` job: lists the tracker's open issues for the filter the store sent and answers them raw (what each says and
/// whether its author is trusted). It makes no draft and decides nothing; a failure is the first line of the tracker's own
/// words, which the store shows.
/// The job's tracker, or the refusal in words.
fn provider(spec: &JobSpec) -> Result<Provider, JobOutcome> {
    let said = spec.arg_str("source").unwrap_or_default();
    Provider::parse(said).ok_or_else(|| Outcome::refused(format!("This computer doesn't read {said} issues.")))
}

pub async fn issues_job(
    project: &crate::config::ProjectConfig,
    gh: &Path,
    keys: &Arc<dyn Keys>,
    spec: &JobSpec,
) -> JobOutcome {
    let provider = match provider(spec) {
        Ok(p) => p,
        Err(refused) => return refused,
    };
    if project.repo_path.trim().is_empty() {
        return Outcome::refused("This computer has no checkout of the project to read issues for.");
    }
    let src = source(project, gh, keys, site(&spec.args), provider);
    run_list(provider, spec, &src).await
}

/// [`issues_job`] against a given tracker (a test hands a fake one).
pub async fn run_list(provider: Provider, spec: &JobSpec, src: &Source) -> JobOutcome {
    list_answer(src.list(&filter(provider, &spec.args)).await)
}

/// What a list becomes as the job's answer: at most 100 raw issues (`milestoneMissing` when the tracker has no such milestone), or
/// the first line of why it could not be read.
pub fn list_answer(found: Result<IssueList, String>) -> JobOutcome {
    match found {
        Err(why) => Outcome::failed(first_line(&why)),
        Ok(list) => {
            let issues: Vec<Value> = list.issues.iter().take(ISSUES_RESULT_MAX).map(raw).collect();
            let mut result = json!({ "issues": issues });
            if list.milestone_missing {
                result["milestoneMissing"] = json!(true);
            }
            Outcome::done(result)
        }
    }
}
