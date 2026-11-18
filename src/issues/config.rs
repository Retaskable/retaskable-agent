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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn args(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn each_provider_reads_only_its_own_filter_arguments() {
        let a = args(json!({"team": "ENG", "project": "PROJ", "label": "bug", "milestone": "v1"}));
        assert_eq!(
            filter(Provider::Linear, &a),
            Filter {
                team: Some("ENG".into()),
                label: Some("bug".into()),
                ..Default::default()
            }
        );
        assert_eq!(
            filter(Provider::Jira, &a),
            Filter {
                project: Some("PROJ".into()),
                label: Some("bug".into()),
                ..Default::default()
            }
        );
        assert_eq!(
            filter(Provider::Github, &a),
            Filter {
                label: Some("bug".into()),
                milestone: Some("v1".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn blank_or_non_text_arguments_are_missing_and_text_is_trimmed() {
        let a = args(json!({"label": "  ", "milestone": 7, "team": null, "site": " acme.atlassian.net "}));
        assert_eq!(filter(Provider::Github, &a), Filter::default());
        assert_eq!(filter(Provider::Linear, &a), Filter::default());
        assert_eq!(site(&a).as_deref(), Some("acme.atlassian.net"));
        assert_eq!(site(&args(json!({"site": ""}))), None);
        assert_eq!(site(&args(json!({}))), None);
    }
}
