use std::sync::Mutex;

use super::*;
use crate::implementer::publish::ChangeView;

/// A host that knows one change per number and logs what was done to it.
struct Fake {
    views: Vec<(u64, anyhow::Result<ChangeView>)>,
    log: Mutex<Vec<String>>,
    reruns: bool,
}

impl Host for Fake {
    fn view(&self, pr: u64) -> anyhow::Result<ChangeView> {
        match self.views.iter().find(|(n, _)| *n == pr) {
            Some((_, Ok(v))) => Ok(v.clone()),
            Some((_, Err(e))) => Err(anyhow::anyhow!("{e:#}")),
            None => Err(anyhow::anyhow!("no such change")),
        }
    }

    fn close(&self, pr: u64, comment: &str) -> anyhow::Result<()> {
        self.log.lock().unwrap().push(format!("close {pr} {comment}"));
        Ok(())
    }

    fn rerun(&self, pr: u64) -> bool {
        self.log.lock().unwrap().push(format!("rerun {pr}"));
        self.reruns
    }
}

fn view(state: &str, branch: &str, sha: &str, merged: bool) -> anyhow::Result<ChangeView> {
    Ok(ChangeView {
        state: state.into(),
        head_ref_name: branch.into(),
        head_ref_oid: sha.into(),
        merged_at: merged.then(|| "2026-01-01T00:00:00Z".to_string()),
    })
}

fn fake(views: Vec<(u64, anyhow::Result<ChangeView>)>) -> Fake {
    Fake {
        views,
        log: Default::default(),
        reruns: true,
    }
}

fn done(host: &Fake, actions: Value) -> Value {
    let spec = JobSpec {
        kind: "change_actions".into(),
        day: "d".into(),
        args: json!({ "actions": actions }).as_object().unwrap().clone(),
    };
    let Outcome::Done { result, .. } = run_actions(&spec, host) else {
        panic!("always done")
    };
    result["done"].clone()
}

#[test]
fn closing_an_open_agent_change_comments_and_never_touches_the_branch() {
    let h = fake(vec![(7, view("OPEN", "retaskable/pc1-x", "aaa", false))]);
    let out = done(&h, json!([{ "id": "a", "kind": "close", "pr": 7, "comment": "Gone." }]));
    assert_eq!(out, json!([{ "id": "a", "ok": true }]));
    assert_eq!(*h.log.lock().unwrap(), ["close 7 Gone."]);
}

#[test]
fn a_close_with_no_comment_says_closed_by_retaskable_and_a_long_one_is_cut() {
    let h = fake(vec![(7, view("OPEN", "retaskable/x", "a", false))]);
    done(&h, json!([{ "id": "a", "kind": "close", "pr": 7 }]));
    done(
        &h,
        json!([{ "id": "b", "kind": "close", "pr": 7, "comment": "x".repeat(900) }]),
    );
    let log = h.log.lock().unwrap();
    assert_eq!(log[0], "close 7 Closed by Retaskable.");
    assert_eq!(log[1].len(), "close 7 ".len() + 500);
}

#[test]
fn a_change_on_another_branch_is_refused_with_no_write() {
    let h = fake(vec![(7, view("OPEN", "feature/mine", "a", false))]);
    let out = done(
        &h,
        json!([{ "id": "a", "kind": "close", "pr": 7 }, { "id": "b", "kind": "rerun", "pr": 7 }]),
    );
    assert_eq!(
        out,
        json!([
            { "id": "a", "ok": false, "skipped": "not an agent branch" },
            { "id": "b", "ok": false, "skipped": "not an agent branch" }
        ])
    );
    assert!(h.log.lock().unwrap().is_empty());
}

#[test]
fn an_already_closed_change_is_skipped_and_a_merged_one_is_left_alone() {
    let h = fake(vec![
        (1, view("CLOSED", "retaskable/a", "a", false)),
        (2, view("MERGED", "retaskable/b", "b", true)),
        (3, view("CLOSED", "retaskable/c", "c", true)),
    ]);
    let actions: Vec<Value> = (1..=3)
        .map(|n| json!({ "id": format!("a{n}"), "kind": "close", "pr": n }))
        .collect();
    let out = done(&h, Value::Array(actions));
    for e in out.as_array().unwrap() {
        assert_eq!(
            (e["ok"].clone(), e["skipped"].clone()),
            (json!(true), json!("already closed"))
        );
    }
    assert!(h.log.lock().unwrap().is_empty());
}

#[test]
fn a_rerun_starts_ci_on_the_open_change() {
    let h = fake(vec![(7, view("OPEN", "retaskable/a", "abc", false))]);
    let out = done(
        &h,
        json!([{ "id": "a", "kind": "rerun", "pr": 7, "sha": "abc" }, { "id": "b", "kind": "rerun", "pr": 7 }]),
    );
    assert_eq!(out, json!([{ "id": "a", "ok": true }, { "id": "b", "ok": true }]));
    assert_eq!(*h.log.lock().unwrap(), ["rerun 7", "rerun 7"]);
}

#[test]
fn a_rerun_for_a_head_that_moved_does_nothing() {
    let h = fake(vec![(7, view("OPEN", "retaskable/a", "new", false))]);
    let out = done(&h, json!([{ "id": "a", "kind": "rerun", "pr": 7, "sha": "old" }]));
    assert_eq!(out, json!([{ "id": "a", "ok": true, "skipped": "head moved" }]));
    assert!(h.log.lock().unwrap().is_empty());
}

#[test]
fn a_host_that_cannot_rerun_or_read_is_an_error_entry_never_a_success() {
    let mut h = fake(vec![
        (7, view("OPEN", "retaskable/a", "a", false)),
        (8, Err(anyhow::anyhow!("gh pr view failed: boom\nsecond line"))),
    ]);
    h.reruns = false;
    let out = done(
        &h,
        json!([{ "id": "a", "kind": "rerun", "pr": 7 }, { "id": "b", "kind": "close", "pr": 8 }, { "id": "c", "kind": "close", "pr": 9 }]),
    );
    assert_eq!(out[0]["ok"], false);
    assert!(out[0]["error"].as_str().unwrap().contains("couldn't start CI"));
    assert_eq!(
        out[1],
        json!({ "id": "b", "ok": false, "error": "gh pr view failed: boom" })
    );
    assert_eq!(out[2]["ok"], false);
}

#[test]
fn at_most_ten_actions_are_done_and_a_bad_one_is_refused_by_name() {
    let h = fake(vec![(7, view("OPEN", "retaskable/a", "a", false))]);
    let mut actions: Vec<Value> = (0..12)
        .map(|i| json!({ "id": format!("a{i}"), "kind": "close", "pr": 7 }))
        .collect();
    actions[1] = json!({ "id": "a1", "kind": "merge", "pr": 7 });
    let out = done(&h, Value::Array(actions));
    assert_eq!(out.as_array().unwrap().len(), 10);
    assert_eq!(out[1]["ok"], false);
    assert_eq!(h.log.lock().unwrap().len(), 9);
}

#[cfg(unix)]
mod gh {
    use super::*;
    use crate::config::ProjectConfig;

    /// A `gh` that logs its arguments, answers `pr view` from `view.json` and `pr checks` from `checks.json`, and fails
    /// everything when `fail` exists.
    fn fake_gh(dir: &Path, view: &str) -> PathBuf {
        std::fs::write(dir.join("view.json"), view).unwrap();
        std::fs::write(
            dir.join("checks.json"),
            r#"[{"name":"ci","bucket":"fail","link":"https://github.com/o/r/actions/runs/55/job/1"}]"#,
        )
        .unwrap();
        let path = dir.join("gh");
        crate::test_support::write_executable(
            &path,
            "#!/bin/sh\nD=\"$(dirname \"$0\")\"\necho \"$*\" >> \"$D/calls.txt\"\n[ -e \"$D/fail\" ] && { echo boom >&2; exit 1; }\ncase \"$1 $2\" in \"pr view\") cat \"$D/view.json\";; \"pr checks\") cat \"$D/checks.json\";; esac\n",
        );
        path
    }

    fn calls(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls.txt"))
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    }

    fn project(dir: &Path) -> ProjectConfig {
        let mut p: ProjectConfig = serde_json::from_value(json!({ "githubRepo": "o/r" })).unwrap();
        p.repo_path = dir.display().to_string();
        p
    }

    async fn job(dir: &Path, gh: &Path, actions: Value) -> JobOutcome {
        let spec = JobSpec {
            kind: "change_actions".into(),
            day: "d".into(),
            args: json!({ "actions": actions }).as_object().unwrap().clone(),
        };
        change_actions_job(&project(dir), gh, &spec).await
    }

    const OPEN: &str = r#"{"state":"OPEN","headRefName":"retaskable/pc1-x","headRefOid":"abc","mergedAt":null}"#;

    #[tokio::test]
    async fn closing_runs_gh_pr_close_with_the_comment_and_no_branch_delete() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(dir.path(), OPEN);
        let Outcome::Done { result, .. } = job(dir.path(), &gh, json!([{ "id": "a", "kind": "close", "pr": 7 }])).await
        else {
            panic!()
        };
        assert_eq!(result["done"], json!([{ "id": "a", "ok": true }]));
        let calls = calls(dir.path());
        assert!(calls[0].starts_with("pr view 7 --json"), "{calls:?}");
        assert_eq!(calls[1], "pr close 7 --comment Closed by Retaskable.");
        assert!(
            calls
                .iter()
                .all(|c| !c.contains("--delete-branch") && !c.starts_with("pr merge"))
        );
    }

    #[tokio::test]
    async fn a_rerun_reruns_the_failed_runs_of_the_change() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(dir.path(), OPEN);
        let Outcome::Done { result, .. } = job(
            dir.path(),
            &gh,
            json!([{ "id": "a", "kind": "rerun", "pr": 7, "sha": "abc" }]),
        )
        .await
        else {
            panic!()
        };
        assert_eq!(result["done"], json!([{ "id": "a", "ok": true }]));
        assert!(calls(dir.path()).iter().any(|c| c.starts_with("run rerun 55 --failed")));
    }

    #[tokio::test]
    async fn a_failing_gh_is_an_error_entry_with_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(dir.path(), OPEN);
        std::fs::write(dir.path().join("fail"), "").unwrap();
        let Outcome::Done { result, .. } = job(dir.path(), &gh, json!([{ "id": "a", "kind": "close", "pr": 7 }])).await
        else {
            panic!()
        };
        assert_eq!(result["done"][0]["ok"], false);
        assert!(
            result["done"][0]["error"]
                .as_str()
                .unwrap()
                .contains("gh pr view failed")
        );
    }

    #[tokio::test]
    async fn a_project_off_github_refuses_the_job() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(dir.path(), OPEN);
        let mut p = project(dir.path());
        p.github_repo = String::new();
        let spec = JobSpec {
            kind: "change_actions".into(),
            ..Default::default()
        };
        assert!(matches!(change_actions_job(&p, &gh, &spec).await, Outcome::Refused(_)));
    }
}
