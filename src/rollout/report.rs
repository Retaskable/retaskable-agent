//! The ready report (docs/specs/rollout-on-server.md section 3, step 2): what only git can say about
//! production and the commits staging has that it lacks.
use serde_json::{Value, json};

use super::deps::Deps;
use crate::backlog::ItemStatus;

/// The most commits one report names.
pub const MAX_COMMITS: usize = 200;

/// How many of the newest commits Production only looks at for a passing build.
const CI_WINDOW: usize = 10;

/// Whether a commit's checks all passed, from its check-runs; only when those can't be read, from its combined
/// statuses. A pending commit, a failed one and one with no checks are not.
async fn checks_passed<D: Deps>(d: &D, sha: &str) -> bool {
    match d.gh_api(format!("commits/{sha}/check-runs")).await {
        Ok(v) => {
            let runs = v["check_runs"].as_array().map(Vec::as_slice).unwrap_or_default();
            !runs.is_empty()
                && runs.iter().all(|r| {
                    r["status"] == "completed"
                        && matches!(r["conclusion"].as_str(), Some("success" | "neutral" | "skipped"))
                })
        }
        Err(_) => match d.gh_api(format!("commits/{sha}/status")).await {
            Ok(v) => v["state"] == "success" && v["statuses"].as_array().is_some_and(|s| !s.is_empty()),
            Err(_) => false,
        },
    }
}

/// `rollout_ready`'s arguments, or why there is nothing to send. With no staging branch (Production only)
/// production is what its version path serves and the commits are the production branch's since it.
pub async fn build<D: Deps>(d: &D, production_branch: &str, staging_branch: Option<&str>) -> Result<Value, String> {
    let production = match staging_branch {
        Some(_) => d
            .git(vec!["rev-parse".into(), format!("origin/{production_branch}")])
            .await?
            .trim()
            .to_string(),
        None => d.production_served().await?,
    };
    let range = format!("{production}..origin/{}", staging_branch.unwrap_or(production_branch));
    let log = d
        .git(
            ["log", "--first-parent", "--format=%H %ct", range.as_str()]
                .map(String::from)
                .to_vec(),
        )
        .await?;
    let commits: Vec<(String, i64)> = log
        .lines()
        .filter_map(|l| {
            let (sha, at) = l.split_once(' ')?;
            Some((sha.to_string(), at.trim().parse::<i64>().ok()? * 1000))
        })
        .take(MAX_COMMITS)
        .collect();
    // Items whose merge commit production lacks.
    let mut waiting = Vec::new();
    for item in d.items().await? {
        let counts = matches!(
            item.status,
            ItemStatus::OnBranch | ItemStatus::OnMain | ItemStatus::InProduction
        ) || item.reopened.is_some();
        let Some(merge) = item.merge_commit.filter(|_| counts) else {
            continue;
        };
        if !d.is_ancestor(merge.clone(), production.clone()).await? {
            waiting.push((item.id, merge));
        }
    }
    // Production only: the newest of the first commits whose checks passed, and everything older, is served.
    let mut passing = None;
    if staging_branch.is_none() {
        for (i, (sha, _)) in commits.iter().take(CI_WINDOW).enumerate() {
            if checks_passed(d, sha).await {
                passing = Some(i);
                break;
            }
        }
    }
    let served = match staging_branch {
        Some(_) => d.staging_served().await?,
        None => None,
    };
    let mut out = Vec::new();
    for (n, (sha, at)) in commits.into_iter().enumerate() {
        let mut items = Vec::new();
        for (id, merge) in &waiting {
            if d.is_ancestor(merge.clone(), sha.clone()).await? {
                items.push(id.clone());
            }
        }
        let served = match &served {
            None if staging_branch.is_none() => passing.is_some_and(|p| n >= p),
            Some(s) => *s == sha || d.is_ancestor(sha.clone(), s.clone()).await?,
            None => false,
        };
        out.push(json!({ "sha": sha, "at": at, "items": items, "served": served }));
    }
    Ok(json!({ "production": production, "commits": out }))
}

/// What the log says about a report: production's first 9 characters, commits waiting, how many served, how many items.
pub fn summary(report: &Value) -> String {
    let commits = report["commits"].as_array().map(Vec::as_slice).unwrap_or_default();
    let served = commits.iter().filter(|c| c["served"] == true).count();
    let items = commits
        .iter()
        .filter_map(|c| c["items"].as_array())
        .flatten()
        .collect::<std::collections::HashSet<_>>()
        .len();
    let production = report["production"].as_str().unwrap_or_default();
    format!(
        "reported production {}, {} waiting, {served} served, {items} items",
        &production[..production.len().min(9)],
        commits.len()
    )
}
