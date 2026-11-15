use std::path::PathBuf;

use super::*;
use crate::backlog::tests::store;
use crate::commands::{Command, Seen};
use crate::telemetry::events::now_ms;
use crate::test_support::repo_with_origin;
use crate::view::{ItemCommand, ProjectView};

#[cfg(windows)]
const LONG_RUNNING: &str = "ping -n 60 127.0.0.1 >nul";
#[cfg(not(windows))]
const LONG_RUNNING: &str = "sleep 60";

fn view(project: &str, repo: PathBuf, command: &str) -> AgentView {
    let v = AgentView {
        machine: "pc".into(),
        tags: Default::default(),
        projects: vec![ProjectView {
            id: project.into(),
            github_repo: "o/one".into(),
            backlog_path: "backlog".into(),
            worktrees_dir: repo.join("worktrees"),
            repo_path: repo,
            base_branch: "main".into(),
            play_command: command.into(),
        }],
        version: "9.9.9".into(),
        lanes: 2,
        chore_lane: None,
        lane_sizes: Default::default(),
        paused: Default::default(),
        store_paused: Default::default(),
        item_commands: Default::default(),
        work: Default::default(),
        restart: Default::default(),
        settings: Default::default(),
        local: Default::default(),
        live: Default::default(),
        checks: Default::default(),
        owner: Default::default(),
    };
    v.item_commands.gates().open_unrecorded(project);
    v
}

fn command(name: &str, args: Value) -> Value {
    let cmd = Command {
        id: "c1".into(),
        name: name.into(),
        args,
        issued_by: "jc".into(),
        issued_at: now_ms(),
        expires_at: now_ms() + 60_000,
    };
    let v = view("one", PathBuf::new(), "");
    crate::commands::run(&cmd, "one", &mut Seen::default(), &v, &|_| {}, &std::env::temp_dir())
}

async fn performed(actions: Vec<Action>, v: &AgentView, items: &[Item], images: &str) -> (Vec<Value>, Vec<Value>) {
    let (base, seen) = store(200, json!({ "value": { "id": "d1" } })).await;
    let client = Client::new(base);
    let cx = Perform {
        view: v,
        project: "one",
        client: &client,
        repo_id: 1,
        token: None,
        ask_id: "a1",
        images_block: images,
        items,
        failures: None,
    };
    let results = perform(&actions, &["bad one".to_string()], &cx).await;
    let sent = seen.lock().unwrap().clone();
    (results, sent)
}

fn item(id: &str) -> Item {
    Item {
        id: id.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn retry_and_skip_outside_claim_mode_are_refused_with_the_commands_text() {
    let v = view("one", PathBuf::new(), "");
    let (results, _) = performed(
        vec![Action::Retry { id: "x".into() }, Action::Skip { id: "x".into() }],
        &v,
        &[],
        "",
    )
    .await;
    let retry = command("retry", json!({ "id": "x" }));
    let skip = command("skip", json!({ "id": "x" }));
    assert_eq!(
        results[0],
        json!({ "type": "retry", "ok": false, "reason": retry["reason"] })
    );
    assert_eq!(
        results[1],
        json!({ "type": "skip", "ok": false, "reason": skip["reason"] })
    );
    assert_eq!(
        results[2],
        json!({ "type": "dropped", "ok": false, "reason": "bad one" })
    );
}

#[tokio::test]
async fn items_lanes_and_pause_run_like_the_commands() {
    let v = view("one", PathBuf::new(), "game");
    v.item_commands.set_claim(true);
    let (results, _) = performed(
        vec![
            Action::Retry { id: "a".into() },
            Action::Skip { id: "b".into() },
            Action::Play { lane: 2 },
            Action::Stop { lane: 1 },
            Action::Play { lane: 3 },
            Action::Pause,
        ],
        &v,
        &[],
        "",
    )
    .await;
    let oks: Vec<_> = results.iter().map(|r| r["ok"].clone()).collect();
    assert_eq!(oks, [true, true, true, true, false, true, false]);
    assert_eq!(results[4]["reason"], "This computer has no lane 3.");
    assert_eq!(v.paused.reason("one"), Some("Paused from the chat".to_string()));
    assert_eq!(v.item_commands.take("one").len(), 2);
    let lanes = v.item_commands.take_lane("one");
    assert!(matches!(
        lanes[..],
        [ItemCommand::Play { lane: 2, .. }, ItemCommand::Stop { lane: 1, .. }]
    ));
    let (results, _) = performed(vec![Action::Resume], &v, &[], "").await;
    assert_eq!(results[0]["ok"], true);
    assert_eq!(v.paused.reason("one"), None);
}

#[tokio::test]
async fn stop_then_play_of_lane_0_is_a_restart() {
    let root = tempfile::tempdir().unwrap();
    let id = "ask-restart";
    let v = view(id, repo_with_origin(root.path()), LONG_RUNNING);
    let (base, _) = store(200, json!({ "value": {} })).await;
    let client = Client::new(base);
    let cx = Perform {
        view: &v,
        project: id,
        client: &client,
        repo_id: 1,
        token: None,
        ask_id: "a1",
        images_block: "",
        items: &[],
        failures: None,
    };
    assert_eq!(perform(&[Action::Play { lane: 0 }], &[], &cx).await[0]["ok"], true);
    assert!(crate::play::running(id));
    let restart = [Action::Stop { lane: 0 }, Action::Play { lane: 0 }];
    let results = perform(&restart, &[], &cx).await;
    assert_eq!(
        results.iter().map(|r| r["ok"].clone()).collect::<Vec<_>>(),
        [true, true]
    );
    assert!(crate::play::running(id));
    crate::play::stop(id).unwrap();
}

fn draft(request: &str, revises: Option<&str>) -> Action {
    Action::DraftSpec {
        request: request.into(),
        titles: vec!["Charts".into()],
        themes: vec!["Reports".into()],
        audience: Some("staff".into()),
        revises: revises.map(Into::into),
    }
}

#[tokio::test]
async fn draft_spec_is_refused_and_nothing_is_sent_to_the_store() {
    let v = view("one", PathBuf::new(), "");
    let (results, sent) = performed(vec![draft("Add charts", None)], &v, &[item("known")], "").await;
    assert_eq!(
        results[0],
        json!({ "type": "draft_spec", "ok": false, "reason": NOT_FILED })
    );
    assert!(NOT_FILED.contains("send it again"));
    assert!(sent.is_empty(), "{sent:?}");
}
