//! `link_project`: a GitHub repo `gh` lists, added as Add project adds it, answered before the agent restarts.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;

use super::github::gh_view;
use super::*;
use crate::add_project::AddProject;
use crate::env::Env;

struct Rig {
    dir: tempfile::TempDir,
    restarts: Arc<AtomicUsize>,
    restarted: Arc<Notify>,
    view: AgentView,
}

fn rig(clone_fails: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    std::fs::write(
        dir.path().join("data/config.json"),
        json!({ "version": 2, "port": 4701, "projects": [] }).to_string(),
    )
    .unwrap();
    let (restarts, restarted) = (Arc::new(AtomicUsize::new(0)), Arc::new(Notify::new()));
    let (r, n) = (restarts.clone(), restarted.clone());
    let add = AddProject::with(
        dir.path().join("data"),
        Env::from_pairs([("HOME", dir.path().join("home").to_string_lossy().into_owned())]),
        crate::repo_picker::RepoPicker::with(
            Box::new(|| Ok(vec![])),
            Box::new(|_| Ok(vec![])),
            Box::new(std::time::Instant::now),
        ),
        Box::new(move |repo, dest| {
            if clone_fails {
                anyhow::bail!("Couldn't clone {repo}: remote: Repository not found.");
            }
            crate::add_project::tests::make_repo(dest, None);
            Ok(())
        }),
        Box::new(|_| Some("trunk".to_string())),
        Box::new(|job| {
            std::thread::spawn(job);
        }),
        Box::new(move || {
            r.fetch_add(1, Ordering::SeqCst);
            n.notify_one();
        }),
    );
    let view = gh_view();
    view.add_project.set(add);
    Rig {
        dir,
        restarts,
        restarted,
        view,
    }
}

async fn linked(rig: &Rig, project: &str, args: Value) -> (Value, Option<Value>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rig.view.item_commands.set_out(project, Some(tx));
    let first = run(
        &cmd("link_project", args),
        project,
        &mut Seen::default(),
        &rig.view,
        &|_: i32| {},
        &data_dir(),
    );
    if first["type"] != PENDING {
        return (first, None);
    }
    let later = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
        .await
        .unwrap()
        .unwrap();
    (first, Some(later))
}

#[tokio::test]
async fn a_repo_is_added_paused_answered_with_its_id_and_only_then_the_agent_restarts() {
    let rig = rig(false);
    // Before Setup too: the computer's own hub has no project yet.
    let (first, later) = linked(&rig, "", json!({ "repo": "o/two", "organisationId": 3 })).await;
    assert_eq!(first, json!({ "type": PENDING, "id": "c1" }));
    let later = later.unwrap();
    assert_eq!(later["type"], "ack", "{later}");
    let id = later["result"]["id"].as_str().unwrap().to_string();
    assert!(!id.is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(30), rig.restarted.notified())
        .await
        .expect("the agent restarts once the answer has gone");
    assert_eq!(rig.restarts.load(Ordering::SeqCst), 1);
    let config: Value =
        serde_json::from_str(&std::fs::read_to_string(rig.dir.path().join("data/config.json")).unwrap()).unwrap();
    assert_eq!(config["projects"][0]["id"], id);
    assert_eq!(config["projects"][0]["githubRepo"], "o/two");
    assert_eq!(config["projects"][0]["organisationId"], 3);
}

#[tokio::test]
async fn a_repo_that_is_linked_already_not_listed_or_badly_named_is_refused_and_nothing_restarts() {
    let rig = rig(false);
    for (args, reason) in [
        (json!({ "repo": "o/one" }), "o/one is already linked on this machine."),
        (json!({ "repo": "o/zzz" }), "o/zzz isn't one of o's repos."),
        (
            json!({ "repo": "stranger/app" }),
            "stranger isn't you or one of your GitHub organisations.",
        ),
    ] {
        let (_, later) = linked(&rig, "one", args).await;
        assert_eq!(later.unwrap(), json!({ "type": "nack", "id": "c1", "reason": reason }));
    }
    for (args, reason) in [
        (json!({}), "Name a repo to link."),
        (json!({ "repo": "nonsense" }), "repo must be owner/name."),
    ] {
        let (first, later) = linked(&rig, "one", args).await;
        assert_eq!(first["reason"], reason);
        assert!(later.is_none());
    }
    assert_eq!(rig.restarts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_other_ways_to_link_are_refused_in_words() {
    let rig = rig(false);
    for field in ["local", "remote", "folder", "create", "scriptId", "join"] {
        let (first, _) = linked(&rig, "", json!({ "repo": "o/two", field: "x" })).await;
        assert!(
            first["reason"].as_str().unwrap().starts_with(&format!(
                "This agent links only a GitHub repo from retaskable.com, not a project with {field}"
            )),
            "{field}: {first}"
        );
    }
    // A view with no Add project flow can't link.
    let bare = view();
    let answer = run(
        &cmd("link_project", json!({ "repo": "o/two" })),
        "",
        &mut Seen::default(),
        &bare,
        &|_: i32| {},
        &data_dir(),
    );
    assert_eq!(answer["reason"], "This agent can't link a project from retaskable.com.");
}

#[tokio::test]
async fn a_clone_that_fails_is_the_nack_with_its_words_and_no_restart() {
    let rig = rig(true);
    let (_, later) = linked(&rig, "", json!({ "repo": "o/two" })).await;
    let later = later.unwrap();
    assert_eq!(later["type"], "nack");
    assert!(
        later["reason"].as_str().unwrap().contains("Repository not found"),
        "{later}"
    );
    assert_eq!(rig.restarts.load(Ordering::SeqCst), 0);
}
