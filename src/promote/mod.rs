//! Promote to production (`agent/src/promote.ts`, environments.md section 4 and remote-promote.md section 3.2): a
//! fast-forward `git push` of a commit that's on staging onto the production branch, from the project's main checkout,
//! after the checks of section 4.2 in order. Plain code: never a force, never a merge, and the only ref it ever
//! names is `refs/heads/<production>`. The project's CI deploys what's pushed; Retaskable deploys nothing. Each push
//! that gets to GitHub is a row of the store's deploy log ([`log`]), kept on disk until the store has it.
mod git;
pub(crate) mod host;
pub mod log;
mod run;
pub(crate) mod site;
#[cfg(test)]
pub(crate) mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use crate::config::{DeployConfig, ProjectConfig};
pub use log::DeployLog;

/// Why Promote is refused under Production only.
pub const NOTHING_TO_PROMOTE: &str = "This project's merges go straight to production: there's nothing to promote.";
/// How long the push may take before it is cut.
pub const PUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const REFUSAL_MAX: usize = 500;

/// Why a promote didn't happen: the code's own refusal (the TypeScript's 4xx `HttpError`, `refused` for a deploy
/// request) or something that broke (`failed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    Refused(String),
    Failed(String),
}

impl Stop {
    pub(crate) fn refused(why: impl Into<String>) -> Self {
        Self::Refused(why.into())
    }

    /// The words, whichever it is.
    pub fn why(&self) -> &str {
        match self {
            Self::Refused(w) | Self::Failed(w) => w,
        }
    }
}

/// Who pressed it, for the log row (`via` in `promote`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// Staging's head chosen at the machine's own login: no `via` on the row.
    Machine,
    /// The store's rollout (`via: "auto"`, under the machine's login).
    Auto,
    /// The website's Deploy to production: `via: "site"`, and the row names whoever pressed it there.
    Site(String),
}

/// One environment: its branch and, when it has one, its address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    pub branch: String,
    pub url: Option<String>,
}

/// A project's staging and production (`Environments`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environments {
    pub staging: Env,
    pub production: Env,
}

/// `mergesTo(deploy) === "production"` (`shared/deployFlow.ts`): anything but `production` is staging.
pub fn merges_to_production(deploy: &DeployConfig) -> bool {
    deploy.merges_to.as_deref() == Some("production")
}

/// A project's staging and production, or `None` without a `deploy` section naming both branches, or under
/// Production only (`environmentsOf`). Pure.
pub fn environments_of(deploy: &DeployConfig) -> Option<Environments> {
    if merges_to_production(deploy) {
        return None;
    }
    let env = |t: &Option<crate::config::DeployTarget>| {
        let t = t.as_ref()?;
        let branch = t.branch.trim();
        (!branch.is_empty()).then(|| Env {
            branch: branch.to_string(),
            url: t
                .url
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(String::from),
        })
    };
    Some(Environments {
        staging: env(&deploy.staging)?,
        production: env(&deploy.production)?,
    })
}

/// A refused push's own words: the remote's lines and the rejection, without git's hints (`pushRefusal`). At most
/// 500 characters. Pure.
pub fn push_refusal(out: &str) -> String {
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !["hint:", "To ", "Everything up-to-date"]
                    .iter()
                    .any(|p| l.starts_with(p))
        })
        .map(|l| match l.strip_prefix("remote:") {
            Some(rest) => rest.trim_start(),
            None => l,
        })
        .filter(|l| !l.is_empty())
        .collect();
    let joined = lines.join("\n");
    crate::text::truncate(if joined.is_empty() { "the push failed" } else { &joined }, REFUSAL_MAX)
}

/// The first seven characters of a commit, as the words show it.
pub(crate) fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// What a project's promote runs against: its main checkout, its repo's name, its `deploy` section, the `gh` it asks
/// and the deploy log it writes. Cheap to clone; both the deploy requests and the rollout hold one per project.
#[derive(Clone)]
pub struct Promoter {
    pub repo: PathBuf,
    pub github_repo: String,
    pub deploy: DeployConfig,
    /// The `gh` program (a test passes a fake).
    pub gh: PathBuf,
    pub log: Arc<DeployLog>,
    /// The time now, in milliseconds since the epoch (a test passes a fixed one).
    pub now_ms: fn() -> u64,
}

impl Promoter {
    /// For `project`, keeping its deploy log in `project_dir` (`logs.pending.json`).
    pub fn new(project: &ProjectConfig, project_dir: &std::path::Path) -> Self {
        Self {
            repo: PathBuf::from(&project.repo_path),
            github_repo: project.github_repo.clone(),
            deploy: project.deploy.clone(),
            gh: PathBuf::from("gh"),
            log: DeployLog::shared(&project_dir.join(log::FILE)),
            now_ms: real_now,
        }
    }
}

fn real_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
