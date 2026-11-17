//! The project's `issues` setting (`IssuesConfig`, checked by `shared/projectSettings.ts`): which tracker and which
//! filter, as the store keeps it in the project's settings values. The site checks the fields when they are saved; this
//! reads them as they come and treats a setting it can't make sense of as none, never as a guess.
use serde_json::Value;

use super::keys::Keys;
use super::{Filter, GithubIssues, JiraIssues, LinearIssues, Provider, Source};
use crate::config::ProjectConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuesConfig {
    pub source: Provider,
    pub label: Option<String>,
    pub milestone: Option<String>,
    /// Linear's team key.
    pub team: Option<String>,
    /// Jira's site (`acme.atlassian.net`).
    pub site: Option<String>,
    /// Jira's project.
    pub project: Option<String>,
    /// Anyone's issues: import an issue whatever its author is (GitHub's untrusted authors).
    pub anyone: bool,
    /// Close issues when done: after the comment, close the issue.
    pub close_when_done: bool,
}

impl IssuesConfig {
    /// The `issues` of a project's settings values; `None` when there is none, or it names no tracker this agent knows.
    pub fn of(values: &Value) -> Option<Self> {
        let o = values.get("issues")?.as_object()?;
        let text = |k: &str| {
            o.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        };
        let flag = |k: &str| o.get(k).and_then(Value::as_bool).unwrap_or(false);
        Some(Self {
            source: Provider::parse(o.get("source")?.as_str()?)?,
            label: text("label"),
            milestone: text("milestone"),
            team: text("team"),
            site: text("site"),
            project: text("project"),
            anyone: flag("anyone"),
            close_when_done: flag("closeWhenDone"),
        })
    }

    /// What a list asks the tracker for (`filterOf`).
    pub fn filter(&self) -> Filter {
        match self.source {
            Provider::Linear => Filter {
                team: self.team.clone(),
                label: self.label.clone(),
                ..Default::default()
            },
            Provider::Jira => Filter {
                project: self.project.clone(),
                label: self.label.clone(),
                ..Default::default()
            },
            Provider::Github => Filter {
                label: self.label.clone(),
                milestone: self.milestone.clone(),
                ..Default::default()
            },
        }
    }
}

/// The tracker an issue of `provider` is in (`issueSource`). Listing needs the setting's site; an issue's comment and
/// close find theirs in its url.
pub fn source(
    project: &ProjectConfig,
    gh: &std::path::Path,
    keys: &std::sync::Arc<dyn Keys>,
    setting: Option<&IssuesConfig>,
    provider: Provider,
) -> Source {
    match provider {
        Provider::Linear => Source::Linear(LinearIssues::new(keys.clone())),
        Provider::Jira => Source::Jira(JiraIssues::new(
            setting
                .filter(|s| s.source == Provider::Jira)
                .and_then(|s| s.site.clone()),
            keys.clone(),
        )),
        Provider::Github => Source::Github(GithubIssues::new(gh, &project.repo_path, &project.github_repo)),
    }
}
