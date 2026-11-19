//! The Environments panel (`environments {}`, `environments` in the TS agent's `promote.ts`, environments.md sections
//! 4.1 and 4.2): after a fetch, each branch's head, what production is behind by, what staging and production serve,
//! the first of Promote's checks that fails, GitHub's rules on production and the last deploys. Under Production only
//! (`deploy.mergesTo`), production's half alone. Read-only: nothing is pushed.
//!
//! Not here yet: `rows`, the items Promote would take (environments.md section 4.3, "Choosing what goes"), which the
//! panel then leaves out and Promote takes staging's head whole.
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::{ReadError, Reading};
use crate::config::{DeployTarget, ProjectConfig};
use crate::promote::{self, NOTHING_TO_PROMOTE, git, merges_to_production, short, site};

/// How many commits the panel lists, and how many deploys it shows.
const MAX_BEHIND: usize = 50;
const SHOWN_DEPLOYS: usize = 10;
const GIT_QUICK: Duration = Duration::from_secs(60);
const GH_TIMEOUT: Duration = Duration::from_secs(30);
const SEP: char = '\x1f';
const NO_ENVIRONMENTS: &str = "This project has no staging and production branches: set them in Settings → Project settings, under Staging and Production.";

fn dir(project: &ProjectConfig) -> &Path {
    Path::new(&project.repo_path)
}

/// `git log` as `{ sha, at, subject }`s, newest first; nothing when git can't say.
async fn commits(repo: &Path, args: &[&str]) -> Vec<Value> {
    let format = format!("--format=%H{SEP}%cI{SEP}%s");
    let mut full = vec!["log", format.as_str()];
    full.extend_from_slice(args);
    let Ok(out) = git::run(repo, &full, GIT_QUICK).await else {
        return Vec::new();
    };
    if !out.ok {
        return Vec::new();
    }
    out.stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut parts = l.splitn(3, SEP);
            let (sha, at, subject) = (parts.next(), parts.next(), parts.next());
            json!({ "sha": sha.unwrap_or(""), "at": at.unwrap_or(""), "subject": subject.unwrap_or("") })
        })
        .collect()
}

async fn head(repo: &Path, branch: &str) -> Value {
    commits(repo, &["-1", &format!("origin/{branch}")])
        .await
        .into_iter()
        .next()
        .unwrap_or(Value::Null)
}

/// What a version path serves, or `None`; also `None` with no address.
async fn serving(target: Option<&DeployTarget>) -> Option<String> {
    let target = target?;
    let url = target.url.as_deref().map(str::trim).filter(|u| !u.is_empty())?;
    site::served_sha(&site::version_url(url, target.version_path.as_deref())).await
}

/// The address a panel shows: trimmed, or none.
fn address(target: Option<&DeployTarget>) -> Option<String> {
    let url = target?.url.as_deref()?.trim();
    (!url.is_empty()).then(|| url.to_string())
}

/// `gh`'s words when it fails, first line.
async fn gh(cx: &Reading, args: &[&str]) -> Result<String, String> {
    crate::promote::host::gh(&cx.gh, dir(&cx.project), args, GH_TIMEOUT).await
}

/// What GitHub enforces on `branch`, in words (`rules` in the TS agent's `gh.ts`): its rulesets and classic protection.
async fn rules(cx: &Reading, branch: &str) -> Result<String, String> {
    let repo = &cx.project.github_repo;
    let found = gh(cx, &["api", &format!("repos/{repo}/rules/branches/{branch}")]).await?;
    let found: Value = serde_json::from_str(&found).unwrap_or(Value::Null);
    let mut ids: Vec<u64> = Vec::new();
    for rule in found.as_array().into_iter().flatten() {
        if let Some(id) = rule.get("ruleset_id").and_then(Value::as_u64)
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    let mut names = Vec::new();
    for id in &ids {
        let named = gh(cx, &["api", &format!("repos/{repo}/rulesets/{id}"), "--jq", "{name}"]).await;
        let name = named
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("name")?.as_str().map(String::from));
        names.push(name.unwrap_or_else(|| format!("#{id}")));
    }
    // 404 is no classic protection; a private repo on the free plan answers 403, which also means none applies.
    let classic = gh(
        cx,
        &["api", &format!("repos/{repo}/branches/{branch}/protection"), "--silent"],
    )
    .await
    .is_ok();
    Ok(rules_words(branch, &names, classic))
}

/// Turns the rulesets' names and whether the classic protection exists into words (`rulesWords`). Pure.
pub(super) fn rules_words(branch: &str, rulesets: &[String], classic: bool) -> String {
    let mut parts = Vec::new();
    if !rulesets.is_empty() {
        let quoted: Vec<String> = rulesets.iter().map(|n| format!("\u{201c}{n}\u{201d}")).collect();
        let noun = if rulesets.len() == 1 { "ruleset" } else { "rulesets" };
        parts.push(format!("{noun} {}", quoted.join(", ")));
    }
    if classic {
        parts.push("branch protection".to_string());
    }
    if parts.is_empty() {
        return format!("No GitHub rules on {branch}: only Retaskable's check applies");
    }
    format!(
        "Protected on GitHub by {}: GitHub checks them on the push",
        parts.join(" and ")
    )
}

async fn rules_or_why(cx: &Reading, branch: &str) -> String {
    match rules(cx, branch).await {
        Ok(words) => words,
        Err(why) => format!("Couldn't read GitHub's rules on {branch}: {why}"),
    }
}

/// Section 4.2's first check, on git alone: `sha` is on staging and ahead of production. Its reason, if any.
async fn git_check(repo: &Path, staging: &str, production: &str, sha: &str) -> Option<String> {
    let (stage, prod) = (format!("origin/{staging}"), format!("origin/{production}"));
    if !git::is_ancestor(repo, sha, &stage).await.unwrap_or(false) {
        return Some(format!("{} isn't on {staging}", short(sha)));
    }
    if git::is_ancestor(repo, sha, &prod).await.unwrap_or(false) {
        return Some("Up to date".into());
    }
    if !git::is_ancestor(repo, &prod, sha).await.unwrap_or(false) {
        return Some("Production has commits staging doesn't: fix that by hand".into());
    }
    None
}

/// Section 4.2's second check: only the repo's admins promote. Its reason, if any.
async fn admin_check(cx: &Reading) -> Option<String> {
    let repo = &cx.project.github_repo;
    let endpoint = format!("repos/{repo}");
    let said = gh(cx, &["api", &endpoint, "--jq", ".permissions"]).await;
    let permissions: Value = match said.as_ref().map(|t| serde_json::from_str(t)) {
        Ok(Ok(p)) => p,
        Ok(Err(_)) => Value::Null,
        Err(_) => Value::Null,
    };
    if !permissions.is_object() {
        let why = said.err().unwrap_or_else(|| "gh didn't say".into());
        return Some(format!("Couldn't ask GitHub whether you're an admin of {repo}: {why}"));
    }
    if permissions["admin"] == true {
        return None;
    }
    Some(if permissions["push"] == true {
        "Only the repo's admins can promote (you have write access)".into()
    } else {
        "Only the repo's admins can promote".into()
    })
}

async fn deploys(cx: &Reading) -> Vec<Value> {
    match &cx.deploy_log {
        Some(log) => log.recent(&cx.client, cx.repo, &cx.token, SHOWN_DEPLOYS).await,
        None => Vec::new(),
    }
}

/// One environment as the view shows it: its branch, and its address when it has one.
fn target(t: &Option<DeployTarget>, branch: &str) -> Value {
    let mut v = json!({ "branch": branch });
    if let Some(url) = address(t.as_ref()) {
        v["url"] = json!(url);
    }
    v
}

pub async fn environments(cx: &Reading) -> Result<Value, ReadError> {
    let project = &cx.project;
    if project.repo_path.is_empty() {
        return Err(ReadError::Refused("Finish setup first.".into()));
    }
    let repo = dir(project);
    if merges_to_production(&project.deploy) {
        return production_only(cx, repo).await;
    }
    let env = promote::environments_of(&project.deploy).ok_or_else(|| ReadError::Refused(NO_ENVIRONMENTS.into()))?;
    git::fetch(repo).await.map_err(ReadError::Failed)?;
    let (staging, production) = (env.staging.branch.as_str(), env.production.branch.as_str());
    let (staging_head, production_head, production_serves, staging_serves) = tokio::join!(
        head(repo, staging),
        head(repo, production),
        serving(project.deploy.production.as_ref()),
        serving(project.deploy.staging.as_ref()),
    );
    let both = !staging_head.is_null() && !production_head.is_null();
    let range = format!("origin/{production}..origin/{staging}");
    let behind = if both {
        commits(repo, &[&format!("-{MAX_BEHIND}"), "--first-parent", &range]).await
    } else {
        Vec::new()
    };
    let count = if both {
        git::run(repo, &["rev-list", "--count", &range], GIT_QUICK).await.ok()
    } else {
        None
    };
    let behind_count = count
        .filter(|c| c.ok)
        .and_then(|c| c.stdout.trim().parse::<usize>().ok())
        .unwrap_or(behind.len());
    let missing = if staging_head.is_null() {
        Some(staging)
    } else if production_head.is_null() {
        Some(production)
    } else {
        None
    };
    let blocked = match missing {
        Some(branch) => Some(format!("There's no {branch} branch on GitHub")),
        None => {
            let sha = staging_head["sha"].as_str().unwrap_or("");
            match git_check(repo, staging, production, sha).await {
                Some(why) => Some(why),
                None => admin_check(cx).await,
            }
        }
    };
    let rules = rules_or_why(cx, production).await;
    let mut view = json!({
        "staging": target(&project.deploy.staging, staging),
        "production": target(&project.deploy.production, production),
        "stagingHead": staging_head,
        "productionHead": production_head,
        "behind": behind,
        "behindCount": behind_count,
        "rules": rules,
        "deploys": deploys(cx).await,
    });
    for (key, served) in [
        ("productionServes", production_serves),
        ("stagingServes", staging_serves),
    ] {
        if let Some(sha) = served {
            view[key] = json!(sha);
        }
    }
    if let Some(why) = blocked {
        view["blocked"] = json!(why);
    }
    Ok(view)
}

/// Under Production only: production is the default branch at the production address; no staging, so nothing is behind
/// and nothing promotes (`productionOnlyEnvironments`). The saved `deploy.staging` is never read.
async fn production_only(cx: &Reading, repo: &Path) -> Result<Value, ReadError> {
    let branch = cx.project.base_branch.as_str();
    git::fetch(repo).await.map_err(ReadError::Failed)?;
    let production = cx.project.deploy.production.as_ref();
    let (production_head, production_serves) = tokio::join!(head(repo, branch), serving(production));
    let mut view = json!({
        "staging": { "branch": branch },
        "production": target(&cx.project.deploy.production, branch),
        "stagingHead": Value::Null,
        "productionHead": production_head,
        "behind": [],
        "behindCount": 0,
        "blocked": NOTHING_TO_PROMOTE,
        "rules": rules_or_why(cx, branch).await,
        "deploys": deploys(cx).await,
    });
    if let Some(sha) = production_serves {
        view["productionServes"] = json!(sha);
    }
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(url: Option<&str>) -> Option<DeployTarget> {
        Some(DeployTarget {
            branch: "main".into(),
            url: url.map(String::from),
            smoke: None,
            version_path: None,
            rollout: None,
        })
    }

    #[test]
    fn address_is_trimmed_and_blank_or_missing_is_none() {
        assert_eq!(
            address(at(Some("  https://x.test ")).as_ref()).as_deref(),
            Some("https://x.test")
        );
        assert_eq!(address(at(Some("   ")).as_ref()), None);
        assert_eq!(address(at(None).as_ref()), None);
        assert_eq!(address(None), None);
    }

    #[test]
    fn target_shows_the_url_only_when_there_is_an_address() {
        assert_eq!(
            target(&at(Some(" https://x.test ")), "stage"),
            json!({"branch": "stage", "url": "https://x.test"})
        );
        assert_eq!(target(&at(Some("")), "stage"), json!({"branch": "stage"}));
        assert_eq!(target(&None, "stage"), json!({"branch": "stage"}));
    }
}
