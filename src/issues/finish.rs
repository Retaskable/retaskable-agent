//! Finishing an issue (issue-import.md section 3.4; `agent/src/issues/finish.ts`): the computer that holds a merged plan
//! from an issue checks its items at start and every 10 minutes. Once every one is built, it comments "Built by
//! Retaskable: <PR links>." on the issue and, with Close issues when done, closes it. Only that computer does, so it is
//! done once. No LLM call, only `gh` (or Linear's or Jira's API, for an issue from there) and the store.
//!
//! The TypeScript agent keeps the plan's timeline and marks each step on it ("Commented on issue #4"). This agent keeps no
//! drafts of its own, so the plan is the store's (`status: merged`, `heldBy` this computer, its `issue` and `itemIds`) and
//! what was done is a small file of the project's own, `issue_finish.json`, which survives a restart. A step that
//! failed is tried again at the next check, and its words are logged once, not every 10 minutes.
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::config::{IssuesConfig, source};
use super::{IssueRef, Job};
use crate::backlog::ItemStatus;
use crate::backlog::access;
use crate::owed_file::OwedFile;

pub const FINISH_EVERY_MS: u64 = 10 * 60_000;
/// The file in the project's folder.
pub const FILE: &str = "issue_finish.json";
/// The store's statuses of a built item: merged with its last slice, on any branch.
const BUILT: [ItemStatus; 3] = [ItemStatus::OnBranch, ItemStatus::OnMain, ItemStatus::InProduction];

/// What was done on a plan's issue, and why the last try failed (`commentedNote`, `closedNote`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    #[serde(default)]
    pub commented: bool,
    #[serde(default)]
    pub closed: bool,
    /// The last failure's words, so the same one is logged once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The implementer's line for an item from an issue, so each slice's pull request links it on the code host. `pr` is
/// the host's word for one ("PR", "MR").
pub fn part_of_line(r: &IssueRef, pr: &str) -> String {
    format!(
        "This item comes from issue {} ({}). Put the line \"Part of {}\" in the {pr} body, so the issue links to it.",
        r.key, r.url, r.key
    )
}

/// The comment on a finished issue.
pub fn built_comment(links: &[String]) -> String {
    if links.is_empty() {
        "Built by Retaskable.".to_string()
    } else {
        format!("Built by Retaskable: {}.", links.join(", "))
    }
}

/// A merged plan from an issue with something left to do on it: the comment, or the close with `close` on. Pure.
pub fn unfinished(mark: Option<&Mark>, close: bool) -> bool {
    let done = |f: fn(&Mark) -> bool| mark.is_some_and(f);
    !done(|m| m.commented) || (close && !done(|m| m.closed))
}

/// A store draft, as finishing reads it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Plan {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    issue: Option<Value>,
    #[serde(default)]
    item_ids: Option<Vec<String>>,
    #[serde(default)]
    held_by: Option<Value>,
}

/// One check: each unfinished plan of this computer whose items are all built gets its comment, and its close. Answers the
/// issues it finished. A failure is logged once, and the next check tries again. The store being down finishes nothing:
/// what is built is the store's to say.
pub async fn finish_issues(job: &Job, close: bool, setting: Option<&IssuesConfig>) -> Vec<String> {
    let Some(repo) = job.repo else {
        return Vec::new();
    };
    let token = match access::current(&*job.access).await {
        Ok(Some(token)) => token,
        Ok(None) => return Vec::new(),
        Err(e) => {
            tracing::warn!("{}: issues: reading the backlog link: {e:#}", job.id);
            return Vec::new();
        }
    };
    let marks = marks(&job.dir);
    let plans = match job
        .client
        .op::<Vec<Plan>>(repo, Some(&token), "drafts", json!({}), "drafts")
        .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "{}: issues: reading the store's drafts failed, so no issue is finished: {e}",
                job.id
            );
            return Vec::new();
        }
    };
    let mine = |p: &Plan| p.held_by.as_ref().and_then(|h| h["machine"].as_str()) == Some(job.machine.as_str());
    let todo: Vec<(&Plan, IssueRef, Mark)> = plans
        .iter()
        .filter(|p| p.status.as_deref() == Some("merged") && mine(p))
        .filter(|p| p.item_ids.as_ref().is_some_and(|i| !i.is_empty()))
        .filter_map(|p| Some((p, IssueRef::of(p.issue.as_ref()?)?)))
        .map(|(p, issue)| (p, issue, marks.get(&p.id).unwrap_or_default()))
        .filter(|(_, _, mark)| unfinished(Some(mark), close))
        .collect();
    if todo.is_empty() {
        return Vec::new();
    }
    let items = match job.client.list(repo, Some(&token)).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(
                "{}: issues: reading the backlog failed, so no issue is finished: {e}",
                job.id
            );
            return Vec::new();
        }
    };
    let mut finished = Vec::new();
    for (plan, issue, mut mark) in todo {
        let ids = plan.item_ids.as_deref().unwrap_or_default();
        let step = async {
            let Some(links) = built_links(job, repo, &token, &items, ids).await? else {
                return Ok(false);
            };
            // The issue's own tracker, whatever the setting is now.
            let src = source(&job.project, &job.gh, &job.keys, setting, issue.provider);
            if !mark.commented {
                src.comment(&issue, &built_comment(&links)).await?;
                mark.commented = true;
                marks.put(&plan.id, mark.clone());
            }
            if close && !mark.closed {
                src.close(&issue).await?;
                mark.closed = true;
                marks.put(&plan.id, mark.clone());
            }
            Ok::<_, String>(true)
        };
        match step.await {
            Ok(true) => {
                if mark.error.take().is_some() {
                    marks.put(&plan.id, mark.clone());
                }
                tracing::info!("{}: issues: finished issue {}", job.id, issue.key);
                finished.push(issue.key.clone());
            }
            // Not every item is built yet.
            Ok(false) => {}
            Err(why) => {
                let line = format!("Couldn't finish issue {}: {}", issue.key, super::first_line(&why));
                // Once per failure, not every 10 minutes.
                if mark.error.as_deref() != Some(line.as_str()) {
                    tracing::warn!("{}: issues: {line}", job.id);
                    mark.error = Some(line);
                    marks.put(&plan.id, mark);
                }
            }
        }
    }
    finished
}

/// What finishing has done, by plan id.
fn marks(dir: &Path) -> OwedFile<Mark> {
    OwedFile::open(dir.join(FILE))
}

/// Each item's merged pull requests, when every one is built; else `None`. A store that can't say is an error: nothing is
/// commented on a guess.
async fn built_links(
    job: &Job,
    repo: u64,
    token: &str,
    items: &[crate::backlog::Item],
    ids: &[String],
) -> Result<Option<Vec<String>>, String> {
    let status = |id: &String| items.iter().find(|i| &i.id == id);
    if ids
        .iter()
        .any(|id| !status(id).is_some_and(|i| BUILT.contains(&i.status)))
    {
        return Ok(None);
    }
    let mut links: Vec<String> = Vec::new();
    for id in ids {
        let details = job
            .client
            .details_get(repo, Some(token), id)
            .await
            .map_err(|e| e.to_string())?;
        let mut prs: Vec<String> = details
            .and_then(|d| d.details.get("mergedPrs").and_then(Value::as_array).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        if prs.is_empty() {
            prs.extend(status(id).and_then(|i| i.pr_url.clone()).filter(|u| !u.is_empty()));
        }
        for url in prs {
            if !links.contains(&url) {
                links.push(url);
            }
        }
    }
    Ok(Some(links))
}
