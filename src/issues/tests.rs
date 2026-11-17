//! The trackers, against a fake HTTP server (Linear and Jira) and a fake `gh` (GitHub): no real network.
use std::sync::{Arc, Mutex};

use axum::Router;
use serde_json::{Value, json};

use super::keys::FakeKeys;
use super::*;

/// One request the fake server saw.
#[derive(Debug, Clone)]
pub(crate) struct Seen {
    pub method: String,
    pub target: String,
    pub auth: String,
    pub body: String,
}

/// A server that answers each request with `answer(&seen)` (status, text) and keeps what it saw. Answers its base URL.
pub(crate) async fn serve(
    answer: impl Fn(&Seen) -> (u16, String) + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (kept, answer) = (log.clone(), Arc::new(answer));
    let app = Router::new().fallback(move |req: axum::extract::Request| {
        let (kept, answer) = (kept.clone(), answer.clone());
        async move {
            let (parts, body) = req.into_parts();
            let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
            let seen = Seen {
                method: parts.method.to_string(),
                target: parts.uri.path_and_query().map(|p| p.to_string()).unwrap_or_default(),
                auth: parts
                    .headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string(),
                body: String::from_utf8_lossy(&body).into_owned(),
            };
            let (status, text) = answer(&seen);
            kept.lock().unwrap().push(seen);
            (axum::http::StatusCode::from_u16(status).unwrap(), text)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("127.0.0.1:{}", addr.port()), log)
}

fn issue_ref(provider: Provider, key: &str, url: &str) -> IssueRef {
    IssueRef {
        provider,
        key: key.into(),
        url: url.into(),
    }
}

mod linear {
    use super::*;

    fn linear(base: &str, key: Option<&str>) -> LinearIssues {
        LinearIssues::at(
            Arc::new(FakeKeys(key.map(String::from), None)),
            format!("http://{base}/graphql"),
        )
    }

    fn filter() -> Filter {
        Filter {
            team: Some("ENG".into()),
            label: Some("retaskable".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn lists_a_teams_labelled_issues_all_trusted() {
        let (base, seen) = serve(|_| {
            (
                200,
                json!({ "data": { "issues": { "nodes": [
                    { "identifier": "ENG-1", "title": "Add it", "description": "Because.", "url": "https://linear.app/x/ENG-1", "creator": { "displayName": "Ana" } },
                    { "identifier": "ENG-2", "title": "Bare", "description": null, "url": "https://linear.app/x/ENG-2", "creator": null }
                ] } } })
                .to_string(),
            )
        })
        .await;
        let got = linear(&base, Some("lin_key")).list(&filter()).await.unwrap();
        assert_eq!(got.issues.len(), 2);
        assert_eq!(
            got.issues[0].issue,
            issue_ref(Provider::Linear, "ENG-1", "https://linear.app/x/ENG-1")
        );
        assert_eq!(
            (got.issues[0].body.as_str(), got.issues[0].author.as_str()),
            ("Because.", "Ana")
        );
        assert!(got.issues.iter().all(|i| i.trusted));
        assert_eq!((got.issues[1].body.as_str(), got.issues[1].author.as_str()), ("", ""));
        let seen = seen.lock().unwrap();
        assert_eq!((seen[0].method.as_str(), seen[0].auth.as_str()), ("POST", "lin_key"));
        let sent: Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(
            sent["variables"],
            json!({ "team": "ENG", "label": "retaskable", "first": 50 })
        );
    }

    #[tokio::test]
    async fn a_filter_without_a_team_or_label_lists_nothing_and_asks_nothing() {
        let (base, seen) = serve(|_| (200, "{}".into())).await;
        let l = linear(&base, Some("k"));
        for f in [
            Filter::default(),
            Filter {
                team: Some("ENG".into()),
                ..Default::default()
            },
        ] {
            assert_eq!(l.list(&f).await.unwrap(), IssueList::default());
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn no_key_asks_nothing_and_says_so() {
        let (base, seen) = serve(|_| (200, "{}".into())).await;
        let err = linear(&base, None).list(&filter()).await.unwrap_err();
        assert_eq!(err, crate::issues::linear::NO_KEY);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failure_is_linears_first_error_line_or_its_status() {
        let (base, _) = serve(|_| (200, json!({ "errors": [{ "message": "Bad key\nmore" }] }).to_string())).await;
        assert_eq!(
            linear(&base, Some("k")).list(&filter()).await.unwrap_err(),
            "Linear: Bad key"
        );
        let (base, _) = serve(|_| (502, "<html>".into())).await;
        assert_eq!(
            linear(&base, Some("k")).list(&filter()).await.unwrap_err(),
            "Linear: HTTP 502"
        );
        let (base, _) = serve(|_| (200, json!({}).to_string())).await;
        assert_eq!(
            linear(&base, Some("k")).list(&filter()).await.unwrap_err(),
            "Linear: HTTP 200"
        );
    }

    #[tokio::test]
    async fn done_is_a_comment_and_the_first_completed_state_by_position() {
        let (base, seen) = serve(|s| {
            let sent: Value = serde_json::from_str(&s.body).unwrap();
            let q = sent["query"].as_str().unwrap();
            let data = if q.starts_with("query Issue(") {
                json!({ "issue": { "id": "uuid-1", "team": { "states": { "nodes": [
                    { "id": "late", "position": 5 }, { "id": "first", "position": 2 }] } } } })
            } else {
                json!({ "ok": { "success": true } })
            };
            (200, json!({ "data": data }).to_string())
        })
        .await;
        let l = linear(&base, Some("k"));
        let r = issue_ref(Provider::Linear, "ENG-1", "u");
        l.comment(&r, "Built by Retaskable.").await.unwrap();
        l.close(&r).await.unwrap();
        let bodies: Vec<Value> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| serde_json::from_str(&s.body).unwrap())
            .collect();
        assert_eq!(
            bodies[1]["variables"],
            json!({ "issueId": "uuid-1", "body": "Built by Retaskable." })
        );
        assert_eq!(bodies[3]["variables"], json!({ "id": "uuid-1", "stateId": "first" }));
    }

    #[tokio::test]
    async fn a_team_with_no_completed_state_and_a_missing_issue_are_refused_in_words() {
        let (base, _) = serve(|_| {
            (
                200,
                json!({ "data": { "issue": { "id": "i", "team": { "states": { "nodes": [] } } } } }).to_string(),
            )
        })
        .await;
        let r = issue_ref(Provider::Linear, "ENG-1", "u");
        assert_eq!(
            linear(&base, Some("k")).close(&r).await.unwrap_err(),
            "Linear: ENG-1's team has no completed state"
        );
        let (base, _) = serve(|_| (200, json!({ "data": { "issue": null } }).to_string())).await;
        assert_eq!(
            linear(&base, Some("k")).close(&r).await.unwrap_err(),
            "Linear: no issue ENG-1"
        );
        let other = issue_ref(Provider::Github, "#1", "u");
        assert_eq!(
            linear(&base, Some("k")).close(&other).await.unwrap_err(),
            "#1 isn't a Linear issue"
        );
    }

    #[tokio::test]
    async fn who_am_i_is_the_viewers_name() {
        let (base, _) = serve(|_| (200, json!({ "data": { "viewer": { "name": "Ana B" } } }).to_string())).await;
        assert_eq!(linear(&base, Some("k")).who_am_i().await.unwrap(), "Ana B");
    }
}

mod jira {
    use super::*;
    use crate::issues::jira::jira_key;

    fn jira(_base: &str, site: Option<&str>, key: Option<String>) -> JiraIssues {
        JiraIssues::with_scheme(site.map(String::from), Arc::new(FakeKeys(None, key)), "http")
    }

    // The fake server's `host:port` is the site, so the key's site check is the real one.
    fn key_for(base: &str) -> Option<String> {
        Some(jira_key(base, "ana@x.com:tok"))
    }

    fn filter() -> Filter {
        Filter {
            project: Some("ENG".into()),
            label: Some("retaskable".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn lists_a_projects_labelled_open_issues_with_basic_auth() {
        let (base, seen) = serve(|_| {
            (
                200,
                json!({ "issues": [
                    { "key": "ENG-7", "fields": { "summary": "Do it", "description": "Text", "reporter": { "displayName": "Bo" } } },
                    { "key": "ENG-8", "fields": { "summary": "Bare", "description": null, "reporter": null } }
                ] })
                .to_string(),
            )
        })
        .await;
        let got = jira(&base, Some(&base), key_for(&base)).list(&filter()).await.unwrap();
        assert_eq!(got.issues.len(), 2);
        assert_eq!(
            got.issues[0].issue,
            issue_ref(Provider::Jira, "ENG-7", &format!("https://{base}/browse/ENG-7"))
        );
        assert_eq!(
            (got.issues[0].title.as_str(), got.issues[0].author.as_str()),
            ("Do it", "Bo")
        );
        assert_eq!((got.issues[1].body.as_str(), got.issues[1].author.as_str()), ("", ""));
        let seen = seen.lock().unwrap();
        // base64("ana@x.com:tok")
        assert_eq!(seen[0].auth, "Basic YW5hQHguY29tOnRvaw==");
        assert!(
            seen[0]
                .target
                .starts_with("/rest/api/2/search/jql?jql=project+%3D+%22ENG%22+AND+labels+%3D+%22retaskable%22"),
            "{}",
            seen[0].target
        );
        assert!(seen[0].target.contains("maxResults=50"), "{}", seen[0].target);
    }

    #[tokio::test]
    async fn jql_strings_escape_quotes_and_backslashes() {
        let (base, seen) = serve(|_| (200, json!({ "issues": [] }).to_string())).await;
        let f = Filter {
            project: Some(r#"A"B\C"#.into()),
            label: Some("x".into()),
            ..Default::default()
        };
        jira(&base, Some(&base), key_for(&base)).list(&f).await.unwrap();
        let target = seen.lock().unwrap()[0].target.clone();
        let jql = reqwest::Url::parse(&format!("http://h{target}"))
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "jql")
            .unwrap()
            .1
            .into_owned();
        assert!(jql.starts_with(r#"project = "A\"B\\C" AND labels = "x""#), "{jql}");
    }

    #[tokio::test]
    async fn the_key_goes_only_to_the_site_it_was_checked_on() {
        let (base, seen) = serve(|_| (200, json!({ "issues": [] }).to_string())).await;
        let l = jira(
            &base,
            Some(&base),
            Some(jira_key("acme.atlassian.net", "ana@x.com:tok")),
        );
        let err = l.list(&filter()).await.unwrap_err();
        assert_eq!(
            err,
            format!("Jira: this computer's key is for acme.atlassian.net, not {base}")
        );
        // An issue's own url is as untrusted as the setting: a store draft could name any host.
        let r = issue_ref(Provider::Jira, "ENG-1", "https://evil.example/browse/ENG-1");
        let l = jira(&base, None, key_for(&base));
        assert!(l.comment(&r, "x").await.unwrap_err().contains("not evil.example"));
        assert!(seen.lock().unwrap().is_empty(), "nothing was sent");
    }

    #[tokio::test]
    async fn no_key_a_key_with_no_site_and_a_missing_site_are_refused_or_empty() {
        let (base, seen) = serve(|_| (200, "{}".into())).await;
        assert_eq!(
            jira(&base, Some(&base), None).list(&filter()).await.unwrap_err(),
            crate::issues::jira::NO_KEY
        );
        assert_eq!(
            jira(&base, Some(&base), Some("nospace".into()))
                .list(&filter())
                .await
                .unwrap_err(),
            crate::issues::jira::NO_KEY
        );
        assert_eq!(
            jira(&base, None, key_for(&base)).list(&filter()).await.unwrap(),
            IssueList::default()
        );
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failure_is_jiras_first_message_or_its_status() {
        let (base, _) = serve(|_| (400, json!({ "errorMessages": ["Bad jql\nmore"] }).to_string())).await;
        let l = jira(&base, Some(&base), key_for(&base));
        assert_eq!(l.list(&filter()).await.unwrap_err(), "Jira: Bad jql");
        let (base, _) = serve(|_| (403, json!({ "errors": { "project": "No access" } }).to_string())).await;
        let l = jira(&base, Some(&base), key_for(&base));
        assert_eq!(l.list(&filter()).await.unwrap_err(), "Jira: No access");
        let (base, _) = serve(|_| (500, String::new())).await;
        let l = jira(&base, Some(&base), key_for(&base));
        assert_eq!(l.list(&filter()).await.unwrap_err(), "Jira: HTTP 500");
    }

    #[tokio::test]
    async fn done_is_a_comment_and_the_transition_named_done_else_the_first_into_done() {
        let (base, seen) = serve(|s| {
            if s.method == "GET" {
                (
                    200,
                    json!({ "transitions": [
                    { "id": "11", "name": "Finish", "to": { "statusCategory": { "key": "done" } } },
                    { "id": "31", "name": "DONE", "to": { "statusCategory": { "key": "indeterminate" } } }] })
                    .to_string(),
                )
            } else {
                (204, String::new())
            }
        })
        .await;
        let l = jira(&base, Some(&base), key_for(&base));
        let r = issue_ref(Provider::Jira, "ENG-1", &format!("https://{base}/browse/ENG-1"));
        l.comment(&r, "Built by Retaskable.").await.unwrap();
        l.close(&r).await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(
            (seen[0].method.as_str(), seen[0].target.as_str()),
            ("POST", "/rest/api/2/issue/ENG-1/comment")
        );
        assert_eq!(seen[0].body, json!({ "body": "Built by Retaskable." }).to_string());
        assert_eq!(seen[2].body, json!({ "transition": { "id": "31" } }).to_string());
    }

    #[tokio::test]
    async fn without_a_done_transition_by_name_the_first_into_a_done_status_is_used_or_it_is_refused() {
        let answer = |t: Value| {
            move |s: &Seen| {
                if s.method == "GET" {
                    (200, t.to_string())
                } else {
                    (204, String::new())
                }
            }
        };
        let (base, seen) = serve(answer(json!({ "transitions": [
            { "id": "5", "name": "Ship it", "to": { "statusCategory": { "key": "done" } } }] })))
        .await;
        let r = issue_ref(Provider::Jira, "ENG-1", &format!("https://{base}/browse/ENG-1"));
        jira(&base, None, key_for(&base)).close(&r).await.unwrap();
        assert_eq!(
            seen.lock().unwrap()[1].body,
            json!({ "transition": { "id": "5" } }).to_string()
        );
        let (base, _) = serve(answer(json!({ "transitions": [] }))).await;
        let r = issue_ref(Provider::Jira, "ENG-1", &format!("https://{base}/browse/ENG-1"));
        assert_eq!(
            jira(&base, None, key_for(&base)).close(&r).await.unwrap_err(),
            "Jira: ENG-1 has no Done transition"
        );
        let other = issue_ref(Provider::Github, "#1", "u");
        assert_eq!(
            jira(&base, None, key_for(&base)).close(&other).await.unwrap_err(),
            "#1 isn't a Jira issue"
        );
    }

    #[tokio::test]
    async fn who_am_i_is_the_display_name_and_needs_a_site() {
        let (base, _) = serve(|_| (200, json!({ "displayName": "Ana B" }).to_string())).await;
        assert_eq!(
            jira(&base, Some(&base), key_for(&base)).who_am_i().await.unwrap(),
            "Ana B"
        );
        assert_eq!(
            jira(&base, None, key_for(&base)).who_am_i().await.unwrap_err(),
            "Jira: no site"
        );
    }
}

#[cfg(unix)]
mod github {
    use super::*;
    use crate::test_support::write_executable;

    const ISSUES: &str = r#"[
      { "number": 8, "title": "From a member", "body": "Please.", "html_url": "https://github.com/o/r/issues/8", "user": { "login": "bo" }, "author_association": "MEMBER" },
      { "number": 9, "title": "Stranger", "body": null, "html_url": "https://github.com/o/r/issues/9", "user": null, "author_association": "NONE" },
      { "number": 10, "title": "A pull request", "body": "", "html_url": "https://github.com/o/r/pull/10", "user": { "login": "bo" }, "author_association": "MEMBER", "pull_request": {} }
    ]"#;

    /// A `gh` that logs its arguments and stdin, and answers the list and milestone endpoints.
    fn gh(dir: &std::path::Path, fail: bool) -> GithubIssues {
        let path = dir.join("gh");
        let script = if fail {
            "#!/bin/sh\necho 'gh: HTTP 404: Not Found' >&2\necho 'second line' >&2\nexit 1\n".to_string()
        } else {
            format!(
                "#!/bin/sh\necho \"$*\" >> \"$(dirname \"$0\")/calls.txt\"\nif [ \"$1 $2\" = \"issue comment\" ]; then cat > \"$(dirname \"$0\")/stdin.txt\"; exit 0; fi\ncase \"$*\" in\n*milestones*) echo '[{{\"number\":3,\"title\":\"v1\"}}]';;\n*) cat <<'EOF'\n{ISSUES}\nEOF\n;;\nesac\n"
            )
        };
        write_executable(&path, &script);
        GithubIssues::new(path, dir, "o/r")
    }

    fn calls(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("calls.txt")).unwrap()
    }

    #[tokio::test]
    async fn lists_the_open_issues_without_pull_requests_and_says_who_is_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let got = gh(tmp.path(), false)
            .list(&Filter {
                label: Some("bug".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(got.issues.len(), 2);
        let (a, b) = (&got.issues[0], &got.issues[1]);
        assert_eq!(
            a.issue,
            issue_ref(Provider::Github, "#8", "https://github.com/o/r/issues/8")
        );
        assert_eq!((a.body.as_str(), a.author.as_str(), a.trusted), ("Please.", "bo", true));
        assert_eq!((b.body.as_str(), b.author.as_str(), b.trusted), ("", "", false));
        assert!(calls(tmp.path()).contains("api -X GET repos/o/r/issues -f state=open -f per_page=50 -f labels=bug"));
    }

    #[tokio::test]
    async fn a_milestone_is_found_by_title_and_a_missing_one_is_said() {
        let tmp = tempfile::tempdir().unwrap();
        let g = gh(tmp.path(), false);
        let f = |m: &str| Filter {
            milestone: Some(m.into()),
            ..Default::default()
        };
        g.list(&f("v1")).await.unwrap();
        assert!(calls(tmp.path()).contains("-f milestone=3"));
        let missing = g.list(&f("v9")).await.unwrap();
        assert!(missing.milestone_missing && missing.issues.is_empty());
    }

    #[tokio::test]
    async fn a_comment_goes_on_stdin_and_a_close_is_completed() {
        let tmp = tempfile::tempdir().unwrap();
        let g = gh(tmp.path(), false);
        let r = issue_ref(Provider::Github, "#8", "u");
        g.comment(&r, "Built by Retaskable: https://github.com/o/r/pull/3.")
            .await
            .unwrap();
        g.close(&r).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("stdin.txt")).unwrap(),
            "Built by Retaskable: https://github.com/o/r/pull/3."
        );
        let calls = calls(tmp.path());
        assert!(calls.contains("issue comment 8 --repo o/r --body-file -"), "{calls}");
        assert!(calls.contains("issue close 8 --repo o/r --reason completed"), "{calls}");
    }

    #[tokio::test]
    async fn a_failed_gh_is_its_first_line_and_a_foreign_issue_is_refused_before_gh_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let g = gh(tmp.path(), true);
        assert_eq!(
            g.close(&issue_ref(Provider::Github, "#8", "u")).await.unwrap_err(),
            "gh: HTTP 404: Not Found"
        );
        assert_eq!(
            g.close(&issue_ref(Provider::Linear, "ENG-1", "u")).await.unwrap_err(),
            "ENG-1 isn't a GitHub issue"
        );
        assert_eq!(
            g.comment(&issue_ref(Provider::Github, "#x", "u"), "t")
                .await
                .unwrap_err(),
            "#x isn't a GitHub issue"
        );
    }
}
