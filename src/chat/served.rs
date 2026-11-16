//! The projects the Chat Agent serves, learned from the store (chat-agent-process.md section 5.1) and never from the
//! Coding Agent's config.json. Fail closed: when the store refuses, nothing is served. An unreachable store, or one
//! that is only busy (429, 5xx), leaves the last list in place, held in memory only, because every chat op needs that
//! same store and so can't work through the gap anyway.
//!
//! Projects with no repository (`projectBacklogs`) are ignored: the Chat Agent's board, hello and asks are all keyed by
//! a repository, and it has no way to place a repo-less project yet.
use std::time::Duration;

use super::project::REPO_ID_VAR;
use crate::backlog::served::Served;
use crate::backlog::{BacklogError, Client};
use crate::env::Env;
use crate::link::{DeviceTokens, TokenAnswer};

/// How often the list is asked again, so a project one of the person's agents has just connected to shows up.
pub const REFRESH: Duration = Duration::from_secs(300);

/// One project to serve.
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    pub repo_id: u64,
    pub github_repo: String,
    /// The store's `defaultBranch`. The store doesn't hold a project's own base branch yet, so a project that works
    /// on a dev branch has its code questions answered against the default branch until it does.
    pub base_branch: String,
}

/// What asking the store came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Served(Vec<Wanted>),
    /// The store couldn't answer just now: the last list stays.
    Unavailable(String),
    /// The store said no, or this agent has no link: nothing is served.
    Refused(String),
}

/// `RETASKABLE_CHAT_REPO_ID`: when set, only that repository id is served (and nothing if the store doesn't name it).
fn only(env: &Env) -> Option<String> {
    env.get(REPO_ID_VAR)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The answer as the projects to serve: repo-less projects left out, a repo id that isn't a number skipped.
pub fn wanted(served: &Served, env: &Env) -> Vec<Wanted> {
    let only = only(env);
    served
        .projects
        .iter()
        .filter(|p| only.as_ref().is_none_or(|id| *id == p.repo_id))
        .filter_map(|p| {
            Some(Wanted {
                repo_id: p.repo_id.parse().ok()?,
                github_repo: p.github_repo.clone(),
                base_branch: p.default_branch.clone(),
            })
        })
        .collect()
}

/// Asks the store with the link's access token, once more with a fresh token when the first is refused as stale.
pub async fn fetch(client: &Client, tokens: &DeviceTokens, env: &Env) -> Outcome {
    let mut stale: Option<String> = None;
    loop {
        let token = match tokens.access_token(stale.as_deref()).await {
            TokenAnswer::Token { access_token, .. } => access_token,
            TokenAnswer::Idle => {
                return Outcome::Refused(
                    "Chat Agent isn't linked to the shared backlog. Run `retaskable-chat-agent link`.".into(),
                );
            }
            TokenAnswer::Unreachable => {
                return Outcome::Unavailable("Chat Agent couldn't reach the shared backlog to sign in.".into());
            }
        };
        match classify(client.projects_served(&token).await, env) {
            Some(outcome) => return outcome,
            None if stale.is_none() => stale = Some(token),
            None => return Outcome::Refused("The shared backlog says this Chat Agent isn't signed in.".into()),
        }
    }
}

/// How the store's answer reads. `None`: a token it no longer takes (401), worth one fresh token.
pub fn classify(answer: Result<Served, BacklogError>, env: &Env) -> Option<Outcome> {
    Some(match answer {
        Ok(served) => Outcome::Served(wanted(&served, env)),
        Err(BacklogError::BadAnswer { status: 401, .. }) => return None,
        Err(e @ BacklogError::Unreachable(_)) => Outcome::Unavailable(e.to_string()),
        Err(e @ BacklogError::BadAnswer { status, .. }) if status == 429 || status >= 500 => {
            Outcome::Unavailable(e.to_string())
        }
        Err(e) => Outcome::Refused(e.to_string()),
    })
}

/// The list to serve now and the error to show (the `lastError`), given the list before and how the store answered.
pub fn next(before: &[Wanted], outcome: Outcome) -> (Vec<Wanted>, Option<String>) {
    match outcome {
        Outcome::Served(list) => (list, None),
        Outcome::Unavailable(why) => (before.to_vec(), Some(why)),
        Outcome::Refused(why) => (Vec::new(), Some(why)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::served::tests::store;
    use crate::backlog::served::{ServedBacklog, ServedRepo};
    use serde_json::json;

    fn repo(id: &str, name: &str, branch: &str) -> ServedRepo {
        ServedRepo {
            repo_id: id.into(),
            github_repo: name.into(),
            default_branch: branch.into(),
            access: "write".into(),
        }
    }

    fn answer() -> Served {
        Served {
            projects: vec![
                repo("1", "o/a", "main"),
                repo("2", "o/b", "develop"),
                repo("x", "o/c", "main"),
            ],
            project_backlogs: vec![ServedBacklog {
                client: "c".into(),
                project_id: "7".into(),
            }],
        }
    }

    #[test]
    fn the_list_is_the_stores_with_its_default_branch_and_nothing_repo_less() {
        let got = wanted(&answer(), &Env::empty());
        let names: Vec<_> = got
            .iter()
            .map(|w| (w.repo_id, w.github_repo.as_str(), w.base_branch.as_str()))
            .collect();
        assert_eq!(names, [(1, "o/a", "main"), (2, "o/b", "develop")]);
    }

    #[test]
    fn the_override_serves_only_that_repository() {
        let env = Env::from_pairs([(REPO_ID_VAR, " 2 ")]);
        let got = wanted(&answer(), &env);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].github_repo, "o/b");
        let none = Env::from_pairs([(REPO_ID_VAR, "99")]);
        assert!(wanted(&answer(), &none).is_empty());
    }

    #[test]
    fn a_refusal_clears_the_list_and_a_busy_store_keeps_it() {
        let before = wanted(&answer(), &Env::empty());
        let (kept, err) = next(&before, Outcome::Unavailable("down".into()));
        assert_eq!((kept.len(), err.as_deref()), (2, Some("down")));
        let (cleared, err) = next(&before, Outcome::Refused("403".into()));
        assert_eq!((cleared.len(), err.as_deref()), (0, Some("403")));
        let (fresh, err) = next(&[], Outcome::Served(before.clone()));
        assert_eq!((fresh, err), (before, None));
    }

    /// Asks a fake store with token `t`, the way `fetch` does once it has one.
    async fn ask(status: u16, reply: serde_json::Value, env: &Env) -> Outcome {
        let (url, _) = store(status, reply).await;
        classify(Client::new(url).projects_served("t").await, env).expect("not a 401")
    }

    #[tokio::test]
    async fn the_list_comes_from_the_store_and_a_403_serves_nothing_with_the_reason() {
        let ok = json!({ "value": { "projects": [
            { "repoId": "5", "githubRepo": "o/r", "defaultBranch": "main", "access": "read" }
        ], "projectBacklogs": [] }});
        let got = ask(200, ok, &Env::empty()).await;
        assert!(matches!(&got, Outcome::Served(l) if l.len() == 1 && l[0].repo_id == 5));
        let refused = ask(
            403,
            json!({ "error": "forbidden", "message": "Only a Chat Agent" }),
            &Env::empty(),
        )
        .await;
        let (list, err) = next(&[], refused);
        assert!(list.is_empty() && err.unwrap().contains("Only a Chat Agent"));
    }

    #[tokio::test]
    async fn a_store_answering_none_serves_none_whatever_a_config_file_says() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"version":2,"projects":[{"id":"x","repoPath":"x","githubRepo":"o/x","baseBranch":"main","backlogPath":"b"}]}"#,
        )
        .unwrap();
        let env = Env::from_pairs([("RETASKABLE_DATA_DIR", dir.path().to_str().unwrap())]);
        let none = json!({ "value": { "projects": [], "projectBacklogs": [] } });
        assert_eq!(ask(200, none, &env).await, Outcome::Served(vec![]));
    }

    #[tokio::test]
    async fn an_unreachable_or_busy_store_keeps_the_list_and_says_why() {
        let before = wanted(&answer(), &Env::empty());
        let down = classify(
            Client::new("http://127.0.0.1:1").projects_served("t").await,
            &Env::empty(),
        )
        .unwrap();
        let (kept, err) = next(&before, down);
        assert!(kept == before && err.is_some());
        let busy = ask(503, json!({ "error": "unavailable", "message": "busy" }), &Env::empty()).await;
        assert_eq!(next(&before, busy).0, before);
        let stale = classify(
            Err(BacklogError::BadAnswer {
                status: 401,
                code: None,
                message: String::new(),
            }),
            &Env::empty(),
        );
        assert!(stale.is_none());
    }

    #[tokio::test]
    async fn a_refresh_picks_up_a_new_project() {
        let first = json!({ "value": { "projects": [
            { "repoId": "1", "githubRepo": "o/a", "defaultBranch": "main", "access": "write" }
        ], "projectBacklogs": [] }});
        let second = json!({ "value": { "projects": [
            { "repoId": "1", "githubRepo": "o/a", "defaultBranch": "main", "access": "write" },
            { "repoId": "2", "githubRepo": "o/b", "defaultBranch": "main", "access": "write" }
        ], "projectBacklogs": [] }});
        let (a, _) = next(&[], ask(200, first, &Env::empty()).await);
        let (b, _) = next(&a, ask(200, second, &Env::empty()).await);
        assert_eq!((a.len(), b.len()), (1, 2));
    }

    #[test]
    fn the_chat_path_never_reads_the_code_agents_config() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![src.join("bin").join("chat_agent.rs")];
        files.extend(std::fs::read_dir(src.join("chat")).unwrap().map(|e| e.unwrap().path()));
        for f in files.into_iter().filter(|f| f.extension().is_some_and(|e| e == "rs")) {
            // This file's tests name what they forbid.
            if f.file_name().is_some_and(|n| n == "served.rs") {
                continue;
            }
            for line in std::fs::read_to_string(&f).unwrap().lines() {
                let code = !line.trim_start().starts_with("//");
                assert!(
                    !(code && (line.contains("config::load") || line.contains(".projects"))),
                    "{}: {line}",
                    f.display()
                );
            }
        }
    }
}
