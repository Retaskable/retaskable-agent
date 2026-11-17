//! The tracker an issue job talks to (`issueSource` in `agent/src/issues/sources.ts`): which source and which filter, from
//! a job's own arguments. The store sends the filter (`issuesArgs` in `shared/issueImport.ts`); this reads it as it comes and
//! treats an argument it can't make sense of as missing, never as a guess.
use serde_json::{Map, Value};

use super::keys::Keys;
use super::{Filter, GithubIssues, JiraIssues, LinearIssues, Provider, Source};
use crate::config::ProjectConfig;

/// A text argument: present, a string, and not blank.
fn text(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// What a list asks the tracker for (`filterOf`), from an `issues` job's `args`.
pub fn filter(provider: Provider, args: &Map<String, Value>) -> Filter {
    match provider {
        Provider::Linear => Filter {
            team: text(args, "team"),
            label: text(args, "label"),
            ..Default::default()
        },
        Provider::Jira => Filter {
            project: text(args, "project"),
            label: text(args, "label"),
            ..Default::default()
        },
        Provider::Github => Filter {
            label: text(args, "label"),
            milestone: text(args, "milestone"),
            ..Default::default()
        },
    }
}

/// An `issues` job's Jira site (`acme.atlassian.net`), the one thing besides the filter its listing needs.
pub fn site(args: &Map<String, Value>) -> Option<String> {
    text(args, "site")
}

/// The tracker an issue of `provider` is in (`issueSource`). Listing needs the job's Jira site; an issue's comment and
/// close find theirs in its url.
pub fn source(
    project: &ProjectConfig,
    gh: &std::path::Path,
    keys: &std::sync::Arc<dyn Keys>,
    site: Option<String>,
    provider: Provider,
) -> Source {
    match provider {
        Provider::Linear => Source::Linear(LinearIssues::new(keys.clone())),
        Provider::Jira => Source::Jira(JiraIssues::new(site, keys.clone())),
        Provider::Github => Source::Github(GithubIssues::new(gh, &project.repo_path, &project.github_repo)),
    }
}
