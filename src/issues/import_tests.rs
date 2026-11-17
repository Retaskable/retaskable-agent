//! The import loop, against a fake store (axum) and a fake `gh`: no real network and no fixed waits.
use std::sync::{Arc, Mutex};

use axum::{Json, Router, routing::post};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use super::config::IssuesConfig;
use super::import::*;
use super::keys::{EnvKeys, Keys};
use super::schedule::Issues;
use super::*;
use crate::backlog::access::AccessTokens;
use crate::link::TokenAnswer;

pub(crate) struct Linked(pub bool);

impl AccessTokens for Linked {
    fn access_token<'a>(&'a self, _stale: Option<&'a str>) -> BoxFuture<'a, TokenAnswer> {
        Box::pin(async move {
            if self.0 {
                TokenAnswer::Token {
                    url: "u".into(),
                    access_token: "tok".into(),
                }
            } else {
                TokenAnswer::Idle
            }
        })
    }

    fn linked(&self) -> bool {
        self.0
    }
}

/// What the fake store answers: its drafts, its lock, and the ops it refuses (with a status).
#[derive(Clone)]
pub(crate) struct Script {
    pub drafts: Value,
    pub lock: Value,
    pub fail: Vec<(&'static str, u16)>,
    /// A `draft_queue` after this many have been taken is refused.
    pub queue_room: usize,
}

impl Script {
    pub fn new() -> Self {
        Self {
            drafts: json!([]),
            lock: json!({ "held": true }),
            fail: vec![],
            queue_room: usize::MAX,
        }
    }
}

/// The store's ops seen, in order, as `{ op, args }`.
pub(crate) type Ops = Arc<Mutex<Vec<Value>>>;

pub(crate) async fn store(script: Script) -> (String, Ops) {
    let ops: Ops = Arc::default();
    let seen = ops.clone();
    let queued = Arc::new(Mutex::new(0usize));
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (script, seen, queued) = (script.clone(), seen.clone(), queued.clone());
            async move {
                let op = body["op"].as_str().unwrap_or_default().to_string();
                seen.lock().unwrap().push(body.clone());
                if let Some((_, status)) = script.fail.iter().find(|(o, _)| *o == op) {
                    return (
                        axum::http::StatusCode::from_u16(*status).unwrap(),
                        Json(json!({ "error": "boom" })),
                    );
                }
                let value = match op.as_str() {
                    "drafts" => script.drafts.clone(),
                    "chore_lock" => script.lock.clone(),
                    "draft_queue" => {
                        let mut n = queued.lock().unwrap();
                        if *n >= script.queue_room {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                Json(json!({ "error": "bad_request" })),
                            );
                        }
                        *n += 1;
                        json!({ "draft": { "id": format!("d{n}") } })
                    }
                    _ => Value::Null,
                };
                (axum::http::StatusCode::OK, Json(json!({ "value": value })))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, ops)
}

pub(crate) fn ops_named(ops: &Ops, name: &str) -> Vec<Value> {
    ops.lock()
        .unwrap()
        .iter()
        .filter(|o| o["op"] == name)
        .map(|o| o["args"].clone())
        .collect()
}

const ISSUES: &str = r#"[
  { "number": 7, "title": "Seen already", "body": "b7", "html_url": "https://github.com/o/r/issues/7", "user": { "login": "ana" }, "author_association": "OWNER" },
  { "number": 8, "title": "From a member", "body": "Please do it.", "html_url": "https://github.com/o/r/issues/8", "user": { "login": "bo" }, "author_association": "MEMBER" },
  { "number": 9, "title": "From a stranger", "body": "x", "html_url": "https://github.com/o/r/issues/9", "user": { "login": "cy" }, "author_association": "NONE" },
  { "number": 10, "title": "A pull request", "body": "", "html_url": "https://github.com/o/r/pull/10", "user": { "login": "bo" }, "author_association": "MEMBER", "pull_request": {} }
]"#;

/// A `gh` that logs its arguments and lists `ISSUES`, or fails.
#[cfg(unix)]
pub(crate) fn gh(dir: &std::path::Path, fail: bool) -> std::path::PathBuf {
    let path = dir.join("gh");
    let script = if fail {
        "#!/bin/sh\necho \"$*\" >> \"$(dirname \"$0\")/calls.txt\"\necho 'gh: HTTP 401: Bad credentials' >&2\necho 'second line' >&2\nexit 1\n".to_string()
    } else {
        format!("#!/bin/sh\necho \"$*\" >> \"$(dirname \"$0\")/calls.txt\"\ncat <<'EOF'\n{ISSUES}\nEOF\n")
    };
    crate::test_support::write_executable(&path, &script);
    path
}

pub(crate) fn job(base: &str, dir: &std::path::Path, linked: bool) -> Job {
    Job {
        id: "p".into(),
        client: Arc::new(Client::new(base)),
        repo: Some(7),
        access: Arc::new(Linked(linked)),
        project: ProjectConfig {
            repo_path: dir.to_string_lossy().into_owned(),
            github_repo: "o/r".into(),
            base_branch: "main".into(),
            ..Default::default()
        },
        gh: dir.join("gh"),
        machine: "my-pc".into(),
        dir: dir.to_path_buf(),
        keys: Arc::new(EnvKeys(crate::env::Env::empty())) as Arc<dyn Keys>,
    }
}

pub(crate) fn github_setting(extra: Value) -> Value {
    let mut issues = json!({ "source": "github", "label": "retaskable" });
    for (k, v) in extra.as_object().unwrap() {
        issues[k] = v.clone();
    }
    json!({ "issues": issues })
}

fn setting(extra: Value) -> IssuesConfig {
    IssuesConfig::of(&github_setting(extra)).unwrap()
}

fn seen_draft() -> Value {
    json!({ "id": "x", "status": "discarded", "issue": { "provider": "github", "key": "#7", "url": "u" } })
}

fn calls(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("calls.txt"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(unix)]
mod loop_ {
    use super::*;

    #[tokio::test]
    async fn each_new_trusted_issue_becomes_a_plan_request_once() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let (base, ops) = store(Script {
            drafts: json!([seen_draft()]),
            ..Script::new()
        })
        .await;
        let got = import_issues(&job(&base, tmp.path(), true), &setting(json!({})), false).await;
        assert_eq!(got, Checked::Imported(vec!["#8".into()]));
        assert_eq!(
            ops_named(&ops, "draft_queue"),
            vec![json!({
                "imported": true,
                "request": "From a member\n\nPlease do it.",
                "titles": ["From a member"],
                "issue": { "provider": "github", "key": "#8", "url": "https://github.com/o/r/issues/8", "author": "bo" },
                "intake": { "note": "Imported from issue #8 by @bo" }
            })]
        );
        assert_eq!(
            ops_named(&ops, "chore_lock"),
            vec![json!({ "name": "issue-import", "take": true, "staleMs": 600000, "fresh": true })]
        );
        assert!(calls(tmp.path()).contains("labels=retaskable"));
    }

    #[tokio::test]
    async fn anyones_issues_brings_in_an_untrusted_author_too() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let (base, ops) = store(Script {
            drafts: json!([seen_draft()]),
            ..Script::new()
        })
        .await;
        let got = import_issues(
            &job(&base, tmp.path(), true),
            &setting(json!({ "anyone": true })),
            false,
        )
        .await;
        assert_eq!(got, Checked::Imported(vec!["#8".into(), "#9".into()]));
        assert_eq!(ops_named(&ops, "draft_queue").len(), 2);
    }

    #[tokio::test]
    async fn a_changed_setting_takes_the_lock_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let (base, ops) = store(Script::new()).await;
        import_issues(&job(&base, tmp.path(), true), &setting(json!({})), true).await;
        assert_eq!(ops_named(&ops, "chore_lock")[0]["staleMs"], 0);
    }

    #[tokio::test]
    async fn at_most_twenty_plan_requests_wait_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let waiting: Vec<Value> = (0..19)
            .map(|i| json!({ "id": format!("w{i}"), "status": "needs_person", "detail": "imported" }))
            .collect();
        let (base, ops) = store(Script {
            drafts: json!(waiting),
            ..Script::new()
        })
        .await;
        let got = import_issues(
            &job(&base, tmp.path(), true),
            &setting(json!({ "anyone": true })),
            false,
        )
        .await;
        assert_eq!(got, Checked::Imported(vec!["#7".into()]), "one more fits");
        assert_eq!(ops_named(&ops, "draft_queue").len(), 1);
    }

    #[tokio::test]
    async fn another_computers_lock_or_one_that_cant_be_taken_checks_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let (base, ops) = store(Script {
            lock: json!({ "held": false, "by": "jo" }),
            ..Script::new()
        })
        .await;
        let got = import_issues(&job(&base, tmp.path(), true), &setting(json!({})), false).await;
        assert_eq!(got, Checked::Skipped);
        let (down, ops_down) = store(Script {
            fail: vec![("chore_lock", 503)],
            ..Script::new()
        })
        .await;
        let got = import_issues(&job(&down, tmp.path(), true), &setting(json!({})), false).await;
        assert_eq!(got, Checked::Skipped);
        for o in [&ops, &ops_down] {
            assert!(ops_named(o, "drafts").is_empty() && ops_named(o, "draft_queue").is_empty());
        }
        assert_eq!(calls(tmp.path()), "", "no tracker was asked");
    }

    #[tokio::test]
    async fn no_link_or_an_unread_store_checks_nothing_and_blames_no_tracker() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let (base, ops) = store(Script::new()).await;
        assert_eq!(
            import_issues(&job(&base, tmp.path(), false), &setting(json!({})), false).await,
            Checked::Skipped
        );
        assert!(ops.lock().unwrap().is_empty());
        let (base, _) = store(Script {
            fail: vec![("drafts", 500)],
            ..Script::new()
        })
        .await;
        assert_eq!(
            import_issues(&job(&base, tmp.path(), true), &setting(json!({})), false).await,
            Checked::Skipped
        );
        assert_eq!(calls(tmp.path()), "");
        let mut no_repo = job(&base, tmp.path(), true);
        no_repo.repo = None;
        assert_eq!(
            import_issues(&no_repo, &setting(json!({})), false).await,
            Checked::Skipped
        );
    }

    #[tokio::test]
    async fn a_tracker_that_cant_be_read_is_the_pages_error_in_the_first_line() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), true);
        let (base, ops) = store(Script::new()).await;
        let got = import_issues(&job(&base, tmp.path(), true), &setting(json!({})), false).await;
        assert_eq!(
            got,
            Checked::Failed("Couldn't read GitHub issues: gh: HTTP 401: Bad credentials".into())
        );
        assert!(ops_named(&ops, "draft_queue").is_empty());
    }

    #[tokio::test]
    async fn a_store_that_stops_taking_plan_requests_ends_the_check_with_what_it_took() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let (base, ops) = store(Script {
            queue_room: 1,
            ..Script::new()
        })
        .await;
        let got = import_issues(
            &job(&base, tmp.path(), true),
            &setting(json!({ "anyone": true })),
            false,
        )
        .await;
        assert_eq!(got, Checked::Imported(vec!["#7".into()]));
        assert_eq!(
            ops_named(&ops, "draft_queue").len(),
            2,
            "the second was refused and the third never asked"
        );
    }
}

#[test]
fn a_plan_request_keeps_a_title_a_cut_body_and_who_asked() {
    let issue = |title: &str, body: &str, author: &str| Issue {
        issue: IssueRef {
            provider: Provider::Linear,
            key: "ENG-1".into(),
            url: "https://l/ENG-1".into(),
        },
        title: title.into(),
        body: body.into(),
        author: author.into(),
        trusted: true,
    };
    let long = draft_args(&issue("  T  ", &"x".repeat(9000), ""));
    assert_eq!(long["request"].as_str().unwrap().chars().count(), "T\n\n".len() + 8000);
    assert_eq!(long["titles"], json!(["T"]));
    assert_eq!(
        long["issue"],
        json!({ "provider": "linear", "key": "ENG-1", "url": "https://l/ENG-1" })
    );
    assert_eq!(long["intake"]["note"], "Imported from issue ENG-1");
    let untitled = draft_args(&issue("   ", "b", "ana"));
    assert_eq!(
        untitled["titles"],
        json!(["ENG-1"]),
        "the key stands in for a missing title"
    );
    assert_eq!(untitled["issue"]["author"], "ana");
    let huge = draft_args(&issue(&"t".repeat(400), "", ""));
    assert_eq!(huge["titles"][0].as_str().unwrap().chars().count(), 300);
}

#[test]
fn the_setting_is_read_per_tracker_and_a_foreign_one_is_none() {
    let linear = IssuesConfig::of(&json!({ "issues": { "source": "linear", "team": "ENG", "label": "rt", "milestone": "ignored", "closeWhenDone": true } })).unwrap();
    assert_eq!(
        linear.filter(),
        Filter {
            team: Some("ENG".into()),
            label: Some("rt".into()),
            ..Default::default()
        }
    );
    assert!(linear.close_when_done && !linear.anyone);
    let jira = IssuesConfig::of(
        &json!({ "issues": { "source": "jira", "site": "a.atlassian.net", "project": "P", "label": " rt " } }),
    )
    .unwrap();
    assert_eq!(
        jira.filter(),
        Filter {
            project: Some("P".into()),
            label: Some("rt".into()),
            ..Default::default()
        }
    );
    assert_eq!(jira.site.as_deref(), Some("a.atlassian.net"));
    let github = IssuesConfig::of(&json!({ "issues": { "source": "github", "milestone": "v1" } })).unwrap();
    assert_eq!(
        github.filter(),
        Filter {
            milestone: Some("v1".into()),
            ..Default::default()
        }
    );
    for none in [
        json!({}),
        json!({ "issues": null }),
        json!({ "issues": { "source": "gitlab" } }),
        json!({ "issues": { "label": "x" } }),
    ] {
        assert_eq!(IssuesConfig::of(&none), None, "{none}");
    }
}

#[cfg(unix)]
mod ticks {
    use super::*;

    /// Waits for `done` without a fixed sleep: yields until it holds (the test's timeout is the bound).
    async fn until(done: impl Fn() -> bool) {
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while !done() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the check didn't finish");
    }

    #[tokio::test]
    async fn a_tick_imports_once_and_waits_ten_minutes_for_the_next_check() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let (base, ops) = store(Script::new()).await;
        let issues = Issues::default();
        let values = github_setting(json!({}));
        issues.tick(job(&base, tmp.path(), true), &values);
        until(|| !ops_named(&ops, "draft_queue").is_empty()).await;
        until(|| !issues.running("p")).await;
        // Another tick of the same setting, now: not time yet, so no task is started to take the lock again.
        issues.tick(job(&base, tmp.path(), true), &values);
        assert!(!issues.running("p"));
        assert_eq!(ops_named(&ops, "chore_lock").len(), 1);
    }

    #[tokio::test]
    async fn a_tracker_failure_is_kept_for_the_page_and_cleared_by_the_next_good_check() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), true);
        let (base, _) = store(Script::new()).await;
        let issues = Issues::default();
        issues.tick(job(&base, tmp.path(), true), &github_setting(json!({})));
        until(|| issues.import_error("p").is_some()).await;
        assert_eq!(
            issues.import_error("p").as_deref(),
            Some("Couldn't read GitHub issues: gh: HTTP 401: Bad credentials")
        );
        // The setting changes (so it is checked at once) and gh works now.
        gh(tmp.path(), false);
        issues.tick(job(&base, tmp.path(), true), &github_setting(json!({ "anyone": true })));
        until(|| issues.import_error("p").is_none()).await;
    }

    #[tokio::test]
    async fn without_the_setting_nothing_is_asked_and_no_error_shows() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let (base, ops) = store(Script::new()).await;
        let issues = Issues::default();
        issues.tick(job(&base, tmp.path(), true), &json!({ "checks": [] }));
        tokio::task::yield_now().await;
        assert!(ops.lock().unwrap().is_empty());
        assert_eq!(issues.import_error("p"), None);
    }
}
