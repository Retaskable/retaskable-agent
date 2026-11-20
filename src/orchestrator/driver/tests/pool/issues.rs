//! The store's issue jobs as pool tasks (`driver/job_tasks.rs`, `issues/`): `can` says which trackers this computer reads
//! (never a key), an `issues` task answers the raw list and an `issues_finish` task what it did, and this computer keeps no
//! timer, lock or list of its own: it asks the store nothing about issues and reads the tracker only when a task says.
use super::jobs::{job_task, set, until_ended};
use super::*;
use crate::jobs::host::JobHost;
use crate::test_support::write_executable;

struct NoTools;
impl JobHost for NoTools {}

/// A `gh` that logs its arguments and lists two issues, one by a member and one by a stranger.
fn fake_gh(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("gh");
    let issues = r#"[
      { "number": 8, "title": "From a member", "body": "Please do it.", "html_url": "https://github.com/o/r/issues/8", "user": { "login": "bo" }, "author_association": "MEMBER" },
      { "number": 9, "title": "From a stranger", "body": "x", "html_url": "https://github.com/o/r/issues/9", "user": { "login": "cy" }, "author_association": "NONE" }
    ]"#;
    write_executable(
        &path,
        &format!(
            "#!/bin/sh\necho \"$*\" >> \"$(dirname \"$0\")/calls.txt\"\ncase \"$1 $2\" in \"pr view\") case \"$3\" in 7) echo '{{\"state\":\"OPEN\",\"headRefName\":\"retaskable/a\",\"headRefOid\":\"abc\"}}';; *) echo '{{\"state\":\"OPEN\",\"headRefName\":\"feature/b\",\"headRefOid\":\"def\"}}';; esac;; \"issue comment\") cat >/dev/null;; \"issue close\") ;; *) cat <<'EOF'\n{issues}\nEOF\n;; esac\n"
        ),
    );
    path
}

fn gh_calls(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls.txt"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

async fn with_gh(tasks: Vec<Value>) -> (Rig, Asked, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let gh = fake_gh(dir.path());
    let (mut rig, _fake, asked, _plan) = live(tasks).await;
    rig.machine.jobs = Some(Arc::new(NoTools));
    let repo = dir.path().display().to_string();
    set(&mut rig, |p| {
        p.repo_path = repo.clone();
        p.github_repo = "o/r".into();
    });
    rig.machine.links.env = Env::from_pairs([("RETASKABLE_GH", gh.to_str().unwrap())]);
    (rig, asked, dir)
}

#[tokio::test]
async fn can_names_the_trackers_this_computer_reads_and_never_a_key() {
    let (mut rig, _fake, asked, _plan) = live(vec![]).await;
    rig.machine.jobs = Some(Arc::new(NoTools));
    set(&mut rig, |p| p.repo_path = "/work/p".into());
    rig.machine.pass().await;
    assert_eq!(
        sent(&asked, "health").last().unwrap()["can"],
        json!(["chrome", "repo", "engine"]),
        "no repo on GitHub, no key"
    );
    set(&mut rig, |p| p.github_repo = "o/r".into());
    rig.machine.links.env = Env::from_pairs([("LINEAR_API_KEY", "lin_secret"), ("JIRA_API_KEY", "  ")]);
    rig.lp(0).pool.every = Some(Duration::ZERO);
    rig.machine.pass().await;
    let health = sent(&asked, "health").last().unwrap().clone();
    assert_eq!(
        health["can"],
        json!(["chrome", "repo", "engine", "issues:github", "issues:linear", "changes"]),
        "a blank key is none"
    );
    assert!(!health.to_string().contains("lin_secret"));
    rig.machine.links.env = Env::from_pairs([("JIRA_API_KEY", "acme.atlassian.net a@b.c:tok")]);
    rig.lp(0).pool.every = Some(Duration::ZERO);
    rig.machine.pass().await;
    let health = sent(&asked, "health").last().unwrap().clone();
    assert_eq!(
        health["can"],
        json!(["chrome", "repo", "engine", "issues:github", "issues:jira", "changes"])
    );
    assert!(!health.to_string().contains("a@b.c"));
}

#[tokio::test]
async fn an_issues_task_answers_the_raw_list_once_and_the_computer_reads_again_only_when_told() {
    let args = json!({ "source": "github", "label": "retaskable" });
    let (mut rig, asked, dir) = with_gh(vec![job_task("task-1", "issues", args)]).await;
    let ends = until_ended(&mut rig, &asked, 1).await;
    assert_eq!(ends[0]["ok"], true, "{}", ends[0]);
    let issues = ends[0]["result"]["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0]["ref"]["key"], "#8");
    assert_eq!(
        (issues[1]["trusted"].as_bool(), issues[1]["author"].as_str()),
        (Some(false), Some("cy"))
    );
    // No timer, lock, list or cap of its own: more passes read nothing and say nothing about issues to the store.
    let reads = gh_calls(dir.path()).len();
    for _ in 0..5 {
        rig.lp(0).pool.every = Some(Duration::ZERO);
        rig.machine.pass().await;
    }
    assert_eq!(gh_calls(dir.path()).len(), reads, "no read the store did not ask for");
    let asked_ops: Vec<String> = asked.lock().unwrap().iter().map(|(op, _)| op.clone()).collect();
    for op in ["chore_lock", "draft_queue", "drafts", "fact_set"] {
        assert!(!asked_ops.contains(&op.to_string()), "{op}: {asked_ops:?}");
    }
    assert_eq!(sent(&asked, "task_end").len(), 1);
}

#[tokio::test]
async fn an_issues_task_for_a_tracker_this_computer_has_no_access_to_ends_in_the_first_line_of_why() {
    let dir = tempfile::tempdir().unwrap();
    let (mut rig, _fake, asked, _plan) = live(vec![job_task(
        "task-1",
        "issues",
        json!({ "source": "linear", "team": "ENG", "label": "l" }),
    )])
    .await;
    rig.machine.jobs = Some(Arc::new(NoTools));
    let repo = dir.path().display().to_string();
    set(&mut rig, |p| p.repo_path = repo.clone());
    rig.machine.links.env = Env::empty();
    let ends = until_ended(&mut rig, &asked, 1).await;
    assert_eq!(ends[0]["ok"], false);
    let note = ends[0]["note"].as_str().unwrap();
    assert!(note.contains("Linear key") && !note.contains('\n'), "{note}");
}

#[tokio::test]
async fn an_issues_finish_task_does_each_entry_and_answers_ok_with_what_was_done() {
    let finish = json!([
        { "id": "d-1", "ref": { "provider": "github", "key": "#4", "url": "https://github.com/o/r/issues/4" }, "comment": "Built by Retaskable.", "close": true },
        { "id": "d-2" }
    ]);
    let (mut rig, asked, dir) = with_gh(vec![job_task("task-1", "issues_finish", json!({ "finish": finish }))]).await;
    let ends = until_ended(&mut rig, &asked, 1).await;
    assert_eq!(ends[0]["ok"], true, "{}", ends[0]);
    assert_eq!(
        ends[0]["result"]["done"],
        json!([{ "id": "d-1", "commented": true, "closed": true }])
    );
    let calls = gh_calls(dir.path());
    assert!(
        calls[0].starts_with("issue comment 4") && calls[1].starts_with("issue close 4"),
        "{calls:?}"
    );
}

#[tokio::test]
async fn a_change_actions_task_closes_an_agent_change_and_answers_what_was_done() {
    let actions = json!([
        { "id": "x-1", "kind": "close", "pr": 7, "comment": "Superseded." },
        { "id": "x-2", "kind": "close", "pr": 9 }
    ]);
    let (mut rig, asked, dir) = with_gh(vec![job_task(
        "task-1",
        "change_actions",
        json!({ "actions": actions }),
    )])
    .await;
    let ends = until_ended(&mut rig, &asked, 1).await;
    assert_eq!(ends[0]["ok"], true, "{}", ends[0]);
    assert_eq!(
        ends[0]["result"]["done"],
        json!([{ "id": "x-1", "ok": true }, { "id": "x-2", "ok": false, "skipped": "not an agent branch" }])
    );
    let calls = gh_calls(dir.path());
    assert!(
        calls.contains(&"pr close 7 --comment Superseded.".to_string()),
        "{calls:?}"
    );
    assert!(!calls.iter().any(|c| c.starts_with("pr close 9")), "{calls:?}");
}
