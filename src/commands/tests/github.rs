//! `github_owners` and `github_repos`: answered by the picker's `gh`, even before Setup.
use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::github_pick::{GithubPicker, Out};

fn gh_view() -> AgentView {
    let mut v = view();
    v.github = Arc::new(GithubPicker::with(
        Arc::new(|args: &[&str]| {
            let out = |stdout: &str| {
                Ok(Out {
                    code: 0,
                    stdout: stdout.into(),
                    stderr: String::new(),
                })
            };
            match args {
                ["api", "user"] => out(r#"{"login":"o"}"#),
                ["api", "--paginate", ..] => out("Org\n"),
                ["repo", "list", ..] => out(
                    r#"[{"nameWithOwner":"o/one","visibility":"PUBLIC","pushedAt":"2026-01-01T00:00:00Z"},
                        {"nameWithOwner":"o/two","visibility":"PRIVATE","isFork":true}]"#,
                ),
                other => panic!("{other:?}"),
            }
        }),
        Arc::new(Instant::now),
    ));
    v
}

async fn asked(v: &AgentView, project: &str, name: &str, args: Value) -> (Value, Option<Value>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    v.item_commands.set_out(project, Some(tx));
    let first = run(
        &cmd(name, args),
        project,
        &mut Seen::default(),
        v,
        &|_: i32| {},
        &data_dir(),
    );
    if first["type"] != PENDING {
        return (first, None);
    }
    let later = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    (first, Some(later))
}

#[tokio::test]
async fn the_owners_are_the_acks_result_even_before_setup() {
    for project in ["one", ""] {
        let v = gh_view();
        let (first, later) = asked(&v, project, "github_owners", json!({ "fresh": true })).await;
        assert_eq!(first, json!({ "type": PENDING, "id": "c1" }));
        assert_eq!(
            later.unwrap(),
            json!({ "type": "ack", "id": "c1", "result": { "login": "o", "orgs": ["Org"] } })
        );
    }
}

#[tokio::test]
async fn an_owners_repos_say_which_are_linked_here() {
    let v = gh_view();
    let (_, later) = asked(&v, "", "github_repos", json!({ "owner": "O" })).await;
    let later = later.unwrap();
    assert_eq!(later["type"], "ack");
    let rows = later["result"].as_array().unwrap();
    let linked: Vec<_> = rows
        .iter()
        .map(|r| (r["nameWithOwner"].as_str().unwrap(), r["linked"].as_bool().unwrap()))
        .collect();
    assert_eq!(linked, [("o/one", true), ("o/two", false)]);
    assert_eq!(rows[1]["private"], true);
}

#[tokio::test]
async fn an_owner_gh_did_not_list_is_nacked_and_a_bad_owner_never_asks() {
    let v = gh_view();
    let (_, later) = asked(&v, "one", "github_repos", json!({ "owner": "stranger" })).await;
    assert_eq!(
        later.unwrap(),
        json!({ "type": "nack", "id": "c1", "reason": "stranger isn't you or one of your GitHub organisations." })
    );
    for args in [
        json!({}),
        json!({ "owner": "a/b" }),
        json!({ "owner": "" }),
        json!({ "owner": "x".repeat(40) }),
    ] {
        let (first, later) = asked(&v, "one", "github_repos", args).await;
        assert_eq!(first["type"], "nack");
        assert!(later.is_none());
    }
}
