//! The store's two issue jobs, against a fake `gh` (GitHub) and a fake HTTP server (Linear): no real network and no waits.
//! `issues` answers the raw list a tracker gave, and `issues_finish` comments and then closes each entry on its own tracker.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use super::finish::{issues_finish_job, run_finish};
use super::import::{comes, issues_job, known, run_list};
use super::keys::{EnvKeys, FakeKeys, Keys};
use super::tests::serve;
use super::*;
use crate::config::ProjectConfig;
use crate::jobs::{JobOutcome, JobSpec, Outcome};

fn spec(kind: &str, args: Value) -> JobSpec {
    JobSpec {
        kind: kind.into(),
        day: "2026-11-17".into(),
        args: args.as_object().unwrap().clone(),
    }
}

fn project(dir: &Path) -> ProjectConfig {
    ProjectConfig {
        repo_path: dir.to_string_lossy().into_owned(),
        github_repo: "o/r".into(),
        base_branch: "main".into(),
        ..Default::default()
    }
}

fn keys() -> Arc<dyn Keys> {
    Arc::new(EnvKeys(crate::env::Env::empty()))
}

fn result(out: JobOutcome) -> Value {
    match out {
        Outcome::Done { result, .. } => result,
        other => panic!("{other:?}"),
    }
}

/// `n` open issues on GitHub: the first by a stranger with a long body, the rest by members.
fn listing(n: usize) -> String {
    let issues: Vec<Value> = (1..=n)
        .map(|i| {
            json!({ "number": i, "title": format!("Issue {i}"), "body": if i == 1 { "x".repeat(9000) } else { "b".into() },
                "html_url": format!("https://github.com/o/r/issues/{i}"), "user": { "login": if i == 1 { "cy" } else { "bo" } },
                "author_association": if i == 1 { "NONE" } else { "MEMBER" } })
        })
        .collect();
    Value::Array(issues).to_string()
}

/// A `gh` that logs its arguments and the comment on stdin, lists `listing`, and fails `issue close` for the issue numbered `close_fails`.
fn gh(dir: &Path, listing: &str, close_fails: Option<&str>) -> PathBuf {
    let path = dir.join("gh");
    let fail = close_fails.map_or(String::new(), |n| {
        format!("case \"$3\" in {n}) echo 'gh: HTTP 403: Forbidden' >&2; echo 'second line' >&2; exit 1;; esac\n")
    });
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> \"$(dirname \"$0\")/calls.txt\"\ncase \"$1 $2\" in\n\"issue comment\") cat >> \"$(dirname \"$0\")/stdin.txt\";;\n\"issue close\") {fail}exit 0;;\n*) cat <<'EOF'\n{listing}\nEOF\n;;\nesac\n"
    );
    crate::test_support::write_executable(&path, &script);
    path
}

fn calls(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls.txt"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

#[tokio::test]
async fn the_issues_job_answers_the_raw_list_with_each_authors_trust_and_decides_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let gh = gh(dir.path(), &listing(3), None);
    let args = json!({ "source": "github", "label": "retaskable" });
    let r = result(issues_job(&project(dir.path()), &gh, &keys(), &spec("issues", args)).await);
    let issues = r["issues"].as_array().unwrap();
    assert_eq!(
        issues.len(),
        3,
        "the stranger's issue is sent too: the store decides what comes in"
    );
    assert_eq!(
        issues[1],
        json!({ "ref": { "provider": "github", "key": "#2", "url": "https://github.com/o/r/issues/2" },
            "title": "Issue 2", "body": "b", "author": "bo", "trusted": true })
    );
    assert_eq!(
        (issues[0]["trusted"].as_bool(), issues[0]["author"].as_str()),
        (Some(false), Some("cy"))
    );
    assert_eq!(
        issues[0]["body"].as_str().unwrap().chars().count(),
        8000,
        "the body is cut"
    );
    assert!(r.get("milestoneMissing").is_none());
    let said = calls(dir.path()).join("\n");
    assert!(said.contains("labels=retaskable"), "the filter is the store's: {said}");
}

#[tokio::test]
async fn at_most_a_hundred_issues_are_answered() {
    let dir = tempfile::tempdir().unwrap();
    let gh = gh(dir.path(), &listing(130), None);
    let r = result(
        issues_job(
            &project(dir.path()),
            &gh,
            &keys(),
            &spec("issues", json!({ "source": "github", "label": "l" })),
        )
        .await,
    );
    assert_eq!(r["issues"].as_array().unwrap().len(), 100);
}

#[tokio::test]
async fn a_failed_read_is_the_first_line_of_the_trackers_words_and_a_tracker_it_does_not_know_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gh");
    crate::test_support::write_executable(
        &path,
        "#!/bin/sh\necho 'gh: HTTP 401: Bad credentials' >&2\necho 'second line' >&2\nexit 1\n",
    );
    let out = issues_job(
        &project(dir.path()),
        &path,
        &keys(),
        &spec("issues", json!({ "source": "github", "label": "l" })),
    )
    .await;
    match out {
        Outcome::Failed { note, .. } => {
            assert!(
                note.contains("HTTP 401") && !note.contains("second line") && !note.contains('\n'),
                "{note}"
            );
        }
        other => panic!("{other:?}"),
    }
    let out = issues_job(
        &project(dir.path()),
        &path,
        &keys(),
        &spec("issues", json!({ "source": "svn" })),
    )
    .await;
    assert_eq!(out, Outcome::refused("This computer doesn't read svn issues."));
    let out = issues_job(
        &ProjectConfig::default(),
        &path,
        &keys(),
        &spec("issues", json!({ "source": "github" })),
    )
    .await;
    assert!(matches!(out, Outcome::Refused(why) if why.contains("no checkout")));
}

#[tokio::test]
async fn a_linear_list_is_the_teams_labelled_issues_all_trusted_and_the_key_stays_here() {
    let (base, seen) = serve(|_| {
        (
            200,
            json!({ "data": { "issues": { "nodes": [
                { "identifier": "ENG-1", "title": "Add it", "description": "Because.", "url": "https://linear.app/x/ENG-1", "creator": { "displayName": "Ana" } }
            ] } } })
            .to_string(),
        )
    })
    .await;
    let src = Source::Linear(LinearIssues::at(
        Arc::new(FakeKeys(Some("lin_key".into()), None)),
        format!("http://{base}/graphql"),
    ));
    let args = json!({ "source": "linear", "team": "ENG", "label": "retaskable" });
    let r = result(run_list(Provider::Linear, &spec("issues", args), &src).await);
    assert_eq!(
        r["issues"][0]["ref"],
        json!({ "provider": "linear", "key": "ENG-1", "url": "https://linear.app/x/ENG-1" })
    );
    assert_eq!(
        (r["issues"][0]["trusted"].as_bool(), r["issues"][0]["author"].as_str()),
        (Some(true), Some("Ana"))
    );
    assert!(!r.to_string().contains("lin_key"), "a key is never in an answer");
    let sent: Value = serde_json::from_str(&seen.lock().unwrap()[0].body).unwrap();
    assert_eq!(sent["variables"]["team"], "ENG");
}

fn entry(id: &str, provider: &str, key: &str, comment: Option<&str>, close: bool) -> Value {
    let mut e = json!({ "id": id, "ref": { "provider": provider, "key": key, "url": format!("https://github.com/o/r/issues/{}", key.trim_start_matches('#')) } });
    if let Some(c) = comment {
        e["comment"] = json!(c);
    }
    if close {
        e["close"] = json!(true);
    }
    e
}

#[tokio::test]
async fn the_finish_job_comments_then_closes_each_entry_and_answers_what_it_did_even_when_one_fails() {
    let dir = tempfile::tempdir().unwrap();
    let gh = gh(dir.path(), "[]", Some("5"));
    let finish = json!([
        entry("d-1", "github", "#4", Some("Built by Retaskable: https://github.com/o/r/pull/9."), true),
        entry("d-2", "github", "#5", Some("Built."), true),
        entry("d-3", "github", "#6", None, true),
        entry("d-4", "github", "#7", Some("Only a comment."), false),
        { "id": "d-5" },
        { "ref": { "provider": "github", "key": "#8", "url": "u" } },
        entry("d-6", "svn", "#9", Some("x"), true),
    ]);
    let out = issues_finish_job(
        &project(dir.path()),
        &gh,
        &keys(),
        &spec("issues_finish", json!({ "finish": finish })),
    )
    .await;
    let r = result(out);
    assert_eq!(
        r["done"],
        json!([
            { "id": "d-1", "commented": true, "closed": true },
            { "id": "d-2", "commented": true, "error": "gh: HTTP 403: Forbidden" },
            { "id": "d-3", "closed": true },
            { "id": "d-4", "commented": true },
        ]),
        "ok with some errors; malformed entries skipped"
    );
    let said = calls(dir.path());
    let order: Vec<&str> = said
        .iter()
        .map(String::as_str)
        .filter(|l| l.starts_with("issue"))
        .collect();
    assert_eq!(
        order[0].split(' ').take(3).collect::<Vec<_>>(),
        ["issue", "comment", "4"],
        "{said:?}"
    );
    assert_eq!(
        order[1].split(' ').take(3).collect::<Vec<_>>(),
        ["issue", "close", "4"],
        "the comment comes first"
    );
    let said_text = std::fs::read_to_string(dir.path().join("stdin.txt")).unwrap();
    assert!(said_text.contains("Built by Retaskable: https://github.com/o/r/pull/9."));
}

#[tokio::test]
async fn each_entry_is_finished_on_its_own_providers_tracker_and_at_most_twenty_a_task() {
    let (base, seen) = serve(|s| {
        let found = if s.body.contains("issues(") || s.body.contains("issue(") {
            json!({ "data": { "issues": { "nodes": [] } } })
        } else {
            json!({ "data": {} })
        };
        (200, found.to_string())
    })
    .await;
    let linear = |p: Provider| match p {
        Provider::Linear => Source::Linear(LinearIssues::at(
            Arc::new(FakeKeys(Some("k".into()), None)),
            format!("http://{base}/graphql"),
        )),
        other => panic!("{other:?} was not asked"),
    };
    let finish: Vec<Value> = (0..25)
        .map(|i| entry(&format!("d-{i}"), "linear", &format!("ENG-{i}"), None, false))
        .collect();
    let out = run_finish(&spec("issues_finish", json!({ "finish": finish })), &linear).await;
    assert_eq!(result(out)["done"].as_array().unwrap().len(), 20);
    let _ = seen;
}

#[test]
fn the_skip_rule_is_the_stores_and_show_matches_asks_it() {
    let drafts: Vec<crate::backlog::Draft> = serde_json::from_value(json!([
        { "id": "x", "status": "discarded", "issue": { "provider": "github", "key": "#7", "url": "u" } },
        { "id": "y", "status": "needs_person", "detail": "imported", "issue": { "provider": "github", "key": "#8", "url": "u" } }
    ]))
    .unwrap();
    let (named, waiting) = known(&drafts);
    assert_eq!(waiting, 1);
    assert_eq!(comes("github:#7", true, &named, false).as_str(), "seen");
    assert_eq!(comes("github:#9", false, &named, false).as_str(), "untrusted");
    assert_eq!(comes("github:#9", false, &named, true).as_str(), "yes");
    assert_eq!(comes("github:#9", true, &named, false).as_str(), "yes");
}

#[test]
fn the_implementers_line_names_the_issue_and_the_hosts_word_for_a_pull_request() {
    let r = IssueRef {
        provider: Provider::Github,
        key: "#4".into(),
        url: "https://github.com/o/r/issues/4".into(),
    };
    assert_eq!(
        finish::part_of_line(&r, "PR"),
        "This item comes from issue #4 (https://github.com/o/r/issues/4). Put the line \"Part of #4\" in the PR body, so the issue links to it."
    );
    assert!(finish::part_of_line(&r, "MR").contains("in the MR body"));
}

#[test]
fn a_plan_from_an_issue_is_quoted_and_named_for_the_drafter_and_any_other_is_left_alone() {
    let issue = json!({ "provider": "linear", "key": "ENG-4", "url": "https://linear.app/x/issue/ENG-4" });
    let text = finish::drafter_request(Some(&issue), "Dark mode\n\nPlease.");
    assert!(
        text.starts_with("This plan comes from Linear issue ENG-4 (https://linear.app/x/issue/ENG-4). The issue's text is below, quoted;"),
        "{text}"
    );
    assert!(text.ends_with("\n\n> Dark mode\n> \n> Please."), "{text}");
    assert_eq!(finish::drafter_request(None, "Just this"), "Just this");
    assert_eq!(
        finish::drafter_request(Some(&json!({ "provider": "svn" })), "Just this"),
        "Just this"
    );
}
