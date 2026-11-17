use axum::{Json, Router, extract::State, routing::post};
use serde_json::json;

use super::*;
use crate::kept_facts::KeptFacts;
use crate::read_kinds::tests::reading;
use crate::test_support::{git, repo_with_origin, run};

/// A store whose `drafts` lists `drafts`.
async fn store(drafts: Value) -> String {
    async fn ops(State(drafts): State<Value>, Json(body): Json<Value>) -> Json<Value> {
        Json(json!({ "value": if body["op"] == "drafts" { drafts } else { Value::Null } }))
    }
    let app = Router::new().route("/v1/repos/{id}/ops", post(ops)).with_state(drafts);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn commit(dir: &std::path::Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    run(git(dir).args(["add", file]));
    // A clone has no identity of its own, and CI's container has no global one.
    run(git(dir).args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "--quiet",
        "-m",
        file,
    ]));
}

/// A project whose origin has the plan's branch, written on another computer (the checkout here never had it).
fn rig() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(tmp.path());
    let worktrees = tmp.path().join("worktrees");
    let other = tmp.path().join("other");
    run(std::process::Command::new("git")
        .args(["clone", "--quiet"])
        .arg(tmp.path().join("origin.git"))
        .arg(&other));
    run(git(&other).args(["checkout", "--quiet", "-b", "retaskable/spec-d1-snooze"]));
    commit(&other, "snooze.md", "# Snooze\n\nKind: new\n");
    run(git(&other).args(["push", "--quiet", "origin", "retaskable/spec-d1-snooze"]));
    (tmp, repo, worktrees)
}

fn cx(repo: &std::path::Path, worktrees: &std::path::Path, base: &str) -> Reading {
    let mut cx = reading(repo.to_str().unwrap(), base);
    cx.project.worktrees_dir = worktrees.to_string_lossy().into_owned();
    cx
}

#[tokio::test]
async fn a_plan_written_on_another_computer_is_read_from_its_branch_with_no_checkout() {
    let (_tmp, repo, worktrees) = rig();
    let base = store(json!([{ "id": "d1", "branch": "retaskable/spec-d1-snooze" }])).await;
    let got = draft_files(&cx(&repo, &worktrees, &base), &json!({ "draftId": "d1" }))
        .await
        .unwrap();
    assert_eq!(
        got,
        json!({ "files": [{ "path": "snooze.md", "isNew": true, "content": "# Snooze\n\nKind: new\n" }] })
    );
    assert!(!worktrees.exists(), "no checkout was made for it");
}

#[tokio::test]
async fn the_plans_worktree_here_answers_and_keeps_its_files_for_the_site() {
    let (_tmp, repo, worktrees) = rig();
    let dir = draft_worktree::create(&repo, &worktrees, "d2", "retaskable/spec-d2-x", "main").unwrap();
    commit(&dir, "a.md", "one\n");
    let facts = KeptFacts::default();
    let mut c = cx(&repo, &worktrees, "http://127.0.0.1:1");
    c.facts = Some(facts.clone());
    let got = draft_files(&c, &json!({ "draftId": "d2" })).await.unwrap();
    assert_eq!(got["files"][0]["path"], "a.md");
    assert_eq!(got["files"][0]["isNew"], true);
    assert_eq!(
        facts.owed("draft_files/d2").unwrap()[0]["path"],
        "a.md",
        "kept for the site"
    );
}

#[tokio::test]
async fn files_this_computer_owes_the_store_answer_when_there_is_no_worktree_and_say_the_stores_words_otherwise() {
    let (_tmp, repo, worktrees) = rig();
    let facts = KeptFacts::default();
    facts.keep_draft_files("d3", &json!([{ "path": "kept.md", "isNew": true, "content": "x" }]));
    let base = store(json!([{ "id": "d4" }, { "id": "d1", "branch": "" }])).await;
    let mut c = cx(&repo, &worktrees, &base);
    c.facts = Some(facts);
    assert_eq!(
        draft_files(&c, &json!({ "draftId": "d3" })).await.unwrap(),
        json!({ "files": [{ "path": "kept.md", "isNew": true, "content": "x" }] })
    );
    assert_eq!(
        draft_files(&c, &json!({ "draftId": "nope" })).await.unwrap_err(),
        ReadError::Refused("No draft called nope.".into())
    );
    assert_eq!(
        draft_files(&c, &json!({ "draftId": "d4" })).await.unwrap_err(),
        ReadError::Refused("That plan has no branch yet, so there are no files to show.".into())
    );
    assert_eq!(
        draft_files(&c, &json!({ "draftId": "d1" })).await.unwrap_err().why(),
        "That plan has no branch yet, so there are no files to show."
    );
    for bad in [json!({}), json!({ "draftId": "../x" }), json!({ "draftId": " " })] {
        assert_eq!(
            draft_files(&c, &bad).await.unwrap_err().why(),
            "draft_files needs the draft's id."
        );
    }
}
