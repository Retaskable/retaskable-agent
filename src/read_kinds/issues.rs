//! Show matches (`issues_preview { label?, milestone?, anyone? }`, `previewIssues` in the TS agent's `issues/import.ts`,
//! issue-import.md section 3.6): GitHub's open issues for the filter, each tagged as an import would treat it, and the
//! plan requests already waiting (for the cap). It reads the store's drafts but takes no lock, makes no draft and
//! records nothing: a failed `gh` is the command's refusal. The list is the REST API's (`gh api`), which carries the
//! author's association; the TS agent's `GithubIssues.list`.
use std::collections::HashSet;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ReadError, Reading};

/// A GitHub issue list is at most this many issues (`ISSUE_LIST_MAX`).
const LIST_MAX: usize = 50;
/// A filter is one line of at most this many characters (`issueFilter`).
const FILTER_MAX: usize = 50;
/// The authors whose issues come in without Anyone's issues: the repo's owner, members and collaborators.
const TRUSTED: &[&str] = &["OWNER", "MEMBER", "COLLABORATOR"];
const GH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct RestIssue {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    user: Option<RestUser>,
    #[serde(default)]
    author_association: String,
    /// The REST list has pull requests in it too.
    #[serde(default)]
    pull_request: Option<Value>,
}

#[derive(Deserialize)]
struct RestUser {
    #[serde(default)]
    login: String,
}

#[derive(Deserialize)]
struct Milestone {
    number: u64,
    #[serde(default)]
    title: String,
}

/// A filter as typed: trimmed, one line, at most 50 characters; `None` when blank or absent. `Err` is the schema's words.
fn filter_of(args: &Value, key: &str) -> Result<Option<String>, ReadError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.chars().count() <= FILTER_MAX && !s.contains(['\r', '\n']) => {
            let s = s.trim();
            Ok((!s.is_empty()).then(|| s.to_string()))
        }
        Some(_) => Err(ReadError::Refused(format!("{key}: Invalid"))),
    }
}

/// `gh`'s first line of words, as `GithubIssues.gh` reports a failure.
fn first_line(words: &str) -> String {
    words.trim().lines().next().unwrap_or("").to_string()
}

async fn gh(cx: &Reading, args: &[&str]) -> Result<String, String> {
    let dir = std::path::Path::new(&cx.project.repo_path);
    crate::promote::host::gh(&cx.gh, dir, args, GH_TIMEOUT).await
}

/// The open issues for the filter, newest first as GitHub lists them, without pull requests; `None` when a milestone was
/// named and no open milestone has its title.
async fn list(cx: &Reading, label: Option<&str>, milestone: Option<&str>) -> Result<Option<Vec<RestIssue>>, String> {
    let repo = &cx.project.github_repo;
    let per_page = format!("per_page={LIST_MAX}");
    let mut query: Vec<String> = ["-f", "state=open", "-f", &per_page].map(String::from).to_vec();
    if let Some(label) = label {
        query.extend(["-f".into(), format!("labels={label}")]);
    }
    if let Some(title) = milestone {
        let endpoint = format!("repos/{repo}/milestones");
        let all = gh(
            cx,
            &["api", "-X", "GET", &endpoint, "-f", "state=open", "-f", "per_page=100"],
        )
        .await?;
        let all: Vec<Milestone> = serde_json::from_str(&all).map_err(|e| e.to_string())?;
        let Some(found) = all.iter().find(|m| m.title == title) else {
            return Ok(None);
        };
        query.extend(["-f".into(), format!("milestone={}", found.number)]);
    }
    let endpoint = format!("repos/{repo}/issues");
    let mut args = vec!["api", "-X", "GET", endpoint.as_str()];
    args.extend(query.iter().map(String::as_str));
    let issues = gh(cx, &args).await?;
    let issues: Vec<RestIssue> = serde_json::from_str(&issues).map_err(|e| e.to_string())?;
    Ok(Some(issues.into_iter().filter(|i| i.pull_request.is_none()).collect()))
}

/// `provider:key` of an issue a draft names, like `refKey`.
fn ref_key(provider: &str, key: &str) -> String {
    format!("{provider}:{key}")
}

pub async fn issues_preview(cx: &Reading, args: &Value) -> Result<Value, ReadError> {
    if cx.project.repo_path.is_empty() {
        return Err(ReadError::Refused("Finish setup first.".into()));
    }
    let label = filter_of(args, "label")?;
    let milestone = filter_of(args, "milestone")?;
    let anyone = match args.get("anyone") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(ReadError::Refused("anyone: Invalid".into())),
    };
    if label.is_none() && milestone.is_none() {
        return Err(ReadError::Refused("issues needs a label or a milestone".into()));
    }
    let drafts = (cx.client.drafts(cx.repo, Some(&cx.token)).await).map_err(|e| ReadError::Failed(e.to_string()))?;
    let found = list(cx, label.as_deref(), milestone.as_deref())
        .await
        .map_err(|e| ReadError::Failed(format!("Couldn't read GitHub issues: {}", first_line(&e))))?;
    // The issues any draft names (discarded ones too), and the plan requests waiting (`known`).
    let named: HashSet<String> = drafts
        .iter()
        .filter_map(|d| {
            let issue = d.issue.as_ref()?;
            Some(ref_key(issue.get("provider")?.as_str()?, issue.get("key")?.as_str()?))
        })
        .collect();
    let waiting = drafts
        .iter()
        .filter(|d| d.detail.as_deref() == Some("imported") && d.status.as_deref() == Some("needs_person"))
        .count();
    let Some(found) = found else {
        return Ok(json!({ "issues": [], "waiting": waiting, "milestoneMissing": true }));
    };
    let issues: Vec<Value> = found
        .iter()
        .map(|i| {
            let key = format!("#{}", i.number);
            let comes = if named.contains(&ref_key("github", &key)) {
                "seen"
            } else if anyone || TRUSTED.contains(&i.author_association.as_str()) {
                "yes"
            } else {
                "untrusted"
            };
            json!({ "key": key, "title": i.title, "url": i.html_url,
                "author": i.user.as_ref().map_or("", |u| u.login.as_str()), "comes": comes })
        })
        .collect();
    Ok(json!({ "issues": issues, "waiting": waiting }))
}
