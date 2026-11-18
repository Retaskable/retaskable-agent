//! Issues as the store's jobs (issue-import.md section 3.7; `agent/src/issues/` in Retaskable.com): GitHub Issues through the
//! agent's own `gh`, Linear or Jira with this computer's key. No LLM call, and no decision: [`import`] is the `issues` job,
//! which reads the tracker and answers the raw list, and [`finish`] is the `issues_finish` job, which comments on (and closes)
//! the issues the store names. The store decides when to read, what comes in and what is finished; this computer keeps no
//! timer, lock, list or cap. [`github`], [`linear`] and [`jira`] are the trackers behind [`Source`]. Show matches is
//! `read_kinds/issues.rs`, which reads the same list with the same skip rule ([`import::comes`]).
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod config;
pub mod finish;
pub mod github;
pub mod import;
pub mod jira;
pub mod keys;
pub mod linear;

pub use github::GithubIssues;
pub use jira::JiraIssues;
pub use linear::LinearIssues;

/// Which tracker an issue is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Github,
    Linear,
    Jira,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Linear => "linear",
            Self::Jira => "jira",
        }
    }

    /// The tracker's name in words (`trackerName`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Linear => "Linear",
            Self::Jira => "Jira",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "github" => Some(Self::Github),
            "linear" => Some(Self::Linear),
            "jira" => Some(Self::Jira),
            _ => None,
        }
    }
}

/// An issue as a draft names it (`IssueRef`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueRef {
    pub provider: Provider,
    pub key: String,
    pub url: String,
}

impl IssueRef {
    /// `provider:key`, how a check tells whether any draft already names an issue (`refKey`).
    pub fn ref_key(&self) -> String {
        ref_key(self.provider.as_str(), &self.key)
    }

    /// The issue a store draft names (`{ provider, key, url, author? }`), if it does and this agent knows the tracker.
    pub fn of(v: &Value) -> Option<Self> {
        Some(Self {
            provider: Provider::parse(v.get("provider")?.as_str()?)?,
            key: v.get("key")?.as_str()?.to_string(),
            url: v.get("url").and_then(Value::as_str).unwrap_or_default().to_string(),
        })
    }
}

/// `provider:key`.
pub fn ref_key(provider: &str, key: &str) -> String {
    format!("{provider}:{key}")
}

/// An open issue of a tracker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub issue: IssueRef,
    pub title: String,
    pub body: String,
    /// Who opened it: a login.
    pub author: String,
    /// Whether its author is the repo's owner, a member or a collaborator (GitHub's `authorAssociation`); always on
    /// Linear and Jira, where the label is the filter.
    pub trusted: bool,
}

/// One open issue filter: a label or a milestone on GitHub, a team and a label on Linear, a project and a label on Jira
/// (whose site is the source's).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub label: Option<String>,
    pub milestone: Option<String>,
    pub team: Option<String>,
    pub project: Option<String>,
}

/// A list's issues; on GitHub, `milestone_missing` when no open milestone has the filter's title (section 3.6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueList {
    pub issues: Vec<Issue>,
    pub milestone_missing: bool,
}

/// A tracker the agent imports from and finishes issues in.
pub enum Source {
    Github(GithubIssues),
    Linear(LinearIssues),
    Jira(JiraIssues),
}

impl Source {
    pub async fn list(&self, filter: &Filter) -> Result<IssueList, String> {
        match self {
            Self::Github(s) => s.list(filter).await,
            Self::Linear(s) => s.list(filter).await,
            Self::Jira(s) => s.list(filter).await,
        }
    }

    pub async fn comment(&self, issue: &IssueRef, text: &str) -> Result<(), String> {
        match self {
            Self::Github(s) => s.comment(issue, text).await,
            Self::Linear(s) => s.comment(issue, text).await,
            Self::Jira(s) => s.comment(issue, text).await,
        }
    }

    pub async fn close(&self, issue: &IssueRef) -> Result<(), String> {
        match self {
            Self::Github(s) => s.close(issue).await,
            Self::Linear(s) => s.close(issue).await,
            Self::Jira(s) => s.close(issue).await,
        }
    }
}

/// The first line of an error's words.
pub(crate) fn first_line(words: &str) -> String {
    words.trim().lines().next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod job_tests;
