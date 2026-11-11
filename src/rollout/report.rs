//! The ready report (docs/specs/rollout-on-server.md section 3, step 2): what only git can say about
//! production and the commits staging has that it lacks.
use serde_json::{Value, json};

use super::deps::Deps;
use crate::backlog::ItemStatus;

/// The most commits one report names.
pub const MAX_COMMITS: usize = 200;

/// `rollout_ready`'s arguments, or why there is nothing to send.
pub async fn build<D: Deps>(d: &D, production_branch: &str, staging_branch: &str) -> Result<Value, String> {
    let production = d
        .git(vec!["rev-parse".into(), format!("origin/{production_branch}")])
        .await?
        .trim()
        .to_string();
    let range = format!("{production}..origin/{staging_branch}");
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
    let served = d.staging_served().await?;
    let mut out = Vec::new();
    for (sha, at) in commits {
        let mut items = Vec::new();
        for (id, merge) in &waiting {
            if d.is_ancestor(merge.clone(), sha.clone()).await? {
                items.push(id.clone());
            }
        }
        let served = match &served {
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
