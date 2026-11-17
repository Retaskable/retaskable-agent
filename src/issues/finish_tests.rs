//! Finishing: a merged plan from an issue, once its items are built, comments on the issue and closes it. A fake store
//! (axum) and a fake `gh`; no real network and no fixed waits.
use serde_json::{Value, json};

use super::finish::*;
use super::import_tests::{Script, job, ops_named, store, until};
use super::schedule::Issues;
use super::*;

/// A `gh` that logs its arguments and the comment on stdin, and fails `issue close` when `close_fails`.
#[cfg(unix)]
fn gh(dir: &std::path::Path, close_fails: bool) {
    let fail = if close_fails {
        "echo 'gh: HTTP 403: Forbidden' >&2\necho 'second line' >&2\nexit 1\n"
    } else {
        "exit 0\n"
    };
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> \"$(dirname \"$0\")/calls.txt\"\ncase \"$1 $2\" in\n\"issue comment\") cat >> \"$(dirname \"$0\")/stdin.txt\";;\n\"issue close\") {fail};;\nesac\n"
    );
    crate::test_support::write_executable(&dir.join("gh"), &script);
}

fn calls(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls.txt"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

fn plan(id: &str, status: &str, machine: &str, items: &[&str]) -> Value {
    json!({ "id": id, "status": status, "itemIds": items,
        "heldBy": { "agentId": "a", "machine": machine, "login": "ana" },
        "issue": { "provider": "github", "key": "#4", "url": "https://github.com/o/r/issues/4" } })
}

fn item(id: &str, status: &str, pr: Option<&str>) -> Value {
    json!({ "id": id, "title": id, "status": status, "rank": 1, "prUrl": pr })
}

fn script(drafts: Value, items: Value) -> Script {
    Script {
        drafts,
        items,
        details: json!({ "a": { "version": 1, "details": { "mergedPrs": ["https://github.com/o/r/pull/9", "https://github.com/o/r/pull/8"] } } }),
        ..Script::new()
    }
}

#[test]
fn the_implementers_line_names_the_issue_and_the_hosts_word_for_a_pull_request() {
    let r = IssueRef {
        provider: Provider::Github,
        key: "#4".into(),
        url: "https://github.com/o/r/issues/4".into(),
    };
    assert_eq!(
        part_of_line(&r, "PR"),
        "This item comes from issue #4 (https://github.com/o/r/issues/4). Put the line \"Part of #4\" in the PR body, so the issue links to it."
    );
    assert!(part_of_line(&r, "MR").contains("in the MR body"));
}

#[test]
fn a_plan_is_unfinished_until_commented_and_with_close_on_until_closed() {
    let m = |commented, closed| Mark {
        commented,
        closed,
        error: None,
    };
    assert!(unfinished(None, false));
    assert!(unfinished(Some(&m(false, true)), false));
    assert!(!unfinished(Some(&m(true, false)), false));
    assert!(unfinished(Some(&m(true, false)), true));
    assert!(!unfinished(Some(&m(true, true)), true));
}

#[test]
fn the_comment_lists_the_pull_requests_or_stands_alone() {
    assert_eq!(built_comment(&["a".into(), "b".into()]), "Built by Retaskable: a, b.");
    assert_eq!(built_comment(&[]), "Built by Retaskable.");
}

#[cfg(unix)]
mod check {
    use super::*;

    fn stdin(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("stdin.txt")).unwrap_or_default()
    }

    #[tokio::test]
    async fn a_built_plan_is_commented_with_its_pull_requests_and_closed_when_the_setting_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let items = json!([
            item("a", "on_main", Some("https://x/1")),
            item("b", "in_production", Some("https://github.com/o/r/pull/9"))
        ]);
        let (base, ops) = store(script(json!([plan("d1", "merged", "my-pc", &["a", "b"])]), items)).await;
        let got = finish_issues(&job(&base, tmp.path(), true), true, None).await;
        assert_eq!(got, vec!["#4".to_string()]);
        // Item a's details name two pull requests; b's has none, so its own link is used, and nothing is listed twice.
        assert_eq!(
            stdin(tmp.path()),
            "Built by Retaskable: https://github.com/o/r/pull/9, https://github.com/o/r/pull/8."
        );
        let calls = calls(tmp.path());
        assert!(
            calls.iter().any(|c| c == "issue comment 4 --repo o/r --body-file -"),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|c| c == "issue close 4 --repo o/r --reason completed"),
            "{calls:?}"
        );
        assert_eq!(ops_named(&ops, "details_get").len(), 2);
        // Done once: the next check finds nothing to do and doesn't ask the tracker again.
        let again = finish_issues(&job(&base, tmp.path(), true), true, None).await;
        assert!(again.is_empty());
        assert_eq!(super::calls(tmp.path()).len(), 2);
    }

    #[tokio::test]
    async fn without_close_the_issue_is_only_commented_and_a_later_close_setting_closes_it() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let items = json!([item("a", "on_branch", None)]);
        let (base, _) = store(script(json!([plan("d1", "merged", "my-pc", &["a"])]), items)).await;
        let j = job(&base, tmp.path(), true);
        assert_eq!(finish_issues(&j, false, None).await.len(), 1);
        assert_eq!(calls(tmp.path()).len(), 1);
        assert_eq!(
            stdin(tmp.path()),
            "Built by Retaskable: https://github.com/o/r/pull/9, https://github.com/o/r/pull/8."
        );
        // Close issues when done is turned on afterwards: only the close is left.
        assert_eq!(finish_issues(&j, true, None).await.len(), 1);
        let calls = calls(tmp.path());
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[1].starts_with("issue close 4"));
    }

    #[tokio::test]
    async fn an_item_not_built_yet_or_missing_waits_for_the_next_check_without_a_word() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let items = json!([item("a", "on_main", None), item("b", "in_review", None)]);
        let (base, ops) = store(script(json!([plan("d1", "merged", "my-pc", &["a", "b"])]), items)).await;
        assert!(
            finish_issues(&job(&base, tmp.path(), true), true, None)
                .await
                .is_empty()
        );
        assert!(calls(tmp.path()).is_empty());
        assert!(ops_named(&ops, "details_get").is_empty());
        let gone = json!([item("a", "on_main", None)]);
        let (base, _) = store(script(json!([plan("d1", "merged", "my-pc", &["a", "zz"])]), gone)).await;
        assert!(
            finish_issues(&job(&base, tmp.path(), true), true, None)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn only_the_computer_that_holds_a_merged_plan_with_items_from_an_issue_finishes_it() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let mut no_issue = plan("d4", "merged", "my-pc", &["a"]);
        no_issue.as_object_mut().unwrap().remove("issue");
        let drafts = json!([
            plan("d1", "merged", "their-pc", &["a"]),
            plan("d2", "needs_person", "my-pc", &["a"]),
            plan("d3", "merged", "my-pc", &[]),
            no_issue
        ]);
        let (base, ops) = store(script(drafts, json!([item("a", "on_main", None)]))).await;
        assert!(
            finish_issues(&job(&base, tmp.path(), true), true, None)
                .await
                .is_empty()
        );
        assert!(calls(tmp.path()).is_empty());
        assert!(
            ops_named(&ops, "list").is_empty(),
            "nothing to finish, so the backlog isn't read"
        );
    }

    #[tokio::test]
    async fn a_failed_close_keeps_the_comment_is_logged_once_and_is_tried_again() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), true);
        let (base, _) = store(script(
            json!([plan("d1", "merged", "my-pc", &["a"])]),
            json!([item("a", "on_main", None)]),
        ))
        .await;
        let j = job(&base, tmp.path(), true);
        assert!(finish_issues(&j, true, None).await.is_empty());
        let marks = || std::fs::read_to_string(tmp.path().join(FILE)).unwrap();
        let first = marks();
        assert!(
            first.contains("\"commented\": true")
                && first.contains("Couldn't finish issue #4: gh: HTTP 403: Forbidden"),
            "{first}"
        );
        assert!(finish_issues(&j, true, None).await.is_empty());
        assert_eq!(marks(), first, "the same failure is not recorded again");
        // The comment was made once; the close is tried at each check.
        assert_eq!(
            calls(tmp.path())
                .iter()
                .filter(|c| c.starts_with("issue comment"))
                .count(),
            1
        );
        assert_eq!(
            calls(tmp.path())
                .iter()
                .filter(|c| c.starts_with("issue close"))
                .count(),
            2
        );
        gh(tmp.path(), false);
        assert_eq!(finish_issues(&j, true, None).await, vec!["#4".to_string()]);
        assert!(!marks().contains("Couldn't finish"), "the error goes once it is done");
    }

    #[tokio::test]
    async fn a_store_that_cant_be_read_or_no_link_finishes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let drafts = json!([plan("d1", "merged", "my-pc", &["a"])]);
        let items = json!([item("a", "on_main", None)]);
        let (base, _) = store(Script {
            fail: vec![("drafts", 503)],
            ..script(drafts.clone(), items.clone())
        })
        .await;
        assert!(
            finish_issues(&job(&base, tmp.path(), true), true, None)
                .await
                .is_empty()
        );
        let (base, _) = store(Script {
            fail: vec![("list", 503)],
            ..script(drafts.clone(), items.clone())
        })
        .await;
        assert!(
            finish_issues(&job(&base, tmp.path(), true), true, None)
                .await
                .is_empty()
        );
        let (base, _) = store(Script {
            fail: vec![("details_get", 503)],
            ..script(drafts.clone(), items.clone())
        })
        .await;
        assert!(
            finish_issues(&job(&base, tmp.path(), true), true, None)
                .await
                .is_empty()
        );
        let (base, ops) = store(script(drafts, items)).await;
        assert!(
            finish_issues(&job(&base, tmp.path(), false), true, None)
                .await
                .is_empty()
        );
        assert!(ops.lock().unwrap().is_empty());
        assert!(calls(tmp.path()).is_empty(), "no tracker was told anything");
    }

    #[tokio::test]
    async fn the_tick_finishes_without_any_import_setting_and_then_waits_ten_minutes() {
        let tmp = tempfile::tempdir().unwrap();
        gh(tmp.path(), false);
        let (base, ops) = store(script(
            json!([plan("d1", "merged", "my-pc", &["a"])]),
            json!([item("a", "on_main", None)]),
        ))
        .await;
        let issues = Issues::default();
        issues.tick(job(&base, tmp.path(), true), &json!({}));
        until(|| calls(tmp.path()).iter().any(|c| c.starts_with("issue comment"))).await;
        until(|| !issues.finishing("p")).await;
        issues.tick(job(&base, tmp.path(), true), &json!({}));
        assert!(!issues.finishing("p"), "not time yet");
        assert_eq!(ops_named(&ops, "drafts").len(), 1);
    }
}

#[test]
fn a_plan_from_an_issue_is_quoted_and_named_for_the_drafter_and_any_other_is_left_alone() {
    let issue = json!({ "provider": "linear", "key": "ENG-4", "url": "https://linear.app/x/issue/ENG-4" });
    let text = drafter_request(Some(&issue), "Dark mode\n\nPlease.");
    assert!(
        text.starts_with("This plan comes from Linear issue ENG-4 (https://linear.app/x/issue/ENG-4). The issue's text is below, quoted;"),
        "{text}"
    );
    assert!(text.ends_with("\n\n> Dark mode\n> \n> Please."), "{text}");
    assert_eq!(drafter_request(None, "Just this"), "Just this");
    assert_eq!(
        drafter_request(Some(&json!({ "provider": "svn" })), "Just this"),
        "Just this"
    );
}
