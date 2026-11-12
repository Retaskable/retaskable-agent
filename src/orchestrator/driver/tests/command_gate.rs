//! The approval gate in the loop (`driver/project.rs`'s `take_allowed_commands`): a project's commands are the
//! allowed ones, never the store's until approved.
use super::*;
use crate::allowed_commands::{AllowedCommands, allowed_commands_file};
use axum::response::IntoResponse;

/// A store that lists nothing and answers `settings_get` with `settings` (a 500 for `None`), noting each op.
async fn store(settings: Arc<Mutex<Option<Value>>>) -> (String, Arc<Mutex<Vec<String>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, settings) = (seen.clone(), settings.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push(op.clone());
                match (op.as_str(), settings.lock().unwrap().clone()) {
                    ("settings_get", None) => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    ("settings_get", Some(v)) => Json(json!({ "value": v })).into_response(),
                    ("facts_get", _) => Json(json!({ "value": { "paused": false } })).into_response(),
                    _ => Json(json!({ "value": [] })).into_response(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

fn record(version: u64, command: &str) -> Value {
    json!({
        "version": version,
        "commandsVersion": version,
        "values": { "checks": [{ "name": "t", "command": command }], "playCommand": format!("play {command}") },
    })
}

fn commands(rig: &mut Rig) -> (Vec<String>, String) {
    let p = &rig.lp(0).project;
    (
        p.checks.iter().map(|c| c.command.clone()).collect(),
        p.play_command.clone(),
    )
}

/// A rig whose project's config.json has `file`'s commands
async fn gated(settings: Option<Value>) -> (Rig, Arc<Mutex<Option<Value>>>, Arc<Mutex<Vec<String>>>) {
    let settings = Arc::new(Mutex::new(settings));
    let (base, asked) = store(settings.clone()).await;
    let mut rig = rig(2, &[&base]);
    let dir = rig._dir.path().to_path_buf();
    // The loop opens the gate itself, from `data_dir`, after what is allowed has been saved.
    rig.machine.links.item_commands = Default::default();
    let lp = rig.lp(0);
    lp.data_dir = Some(dir);
    lp.file_project.checks = serde_json::from_value(json!([{ "name": "t", "command": "file test" }])).unwrap();
    lp.file_project.play_command = "file play".into();
    lp.project = lp.file_project.clone();
    (rig, settings, asked)
}

fn allow(rig: &Rig, version: u64, command: &str) {
    let values = record(version, command)["values"].as_object().unwrap().clone();
    let file = allowed_commands_file(rig._dir.path(), "p0");
    AllowedCommands {
        commands_version: version,
        values,
        at: 0,
    }
    .save(&file)
    .unwrap();
}

#[tokio::test]
async fn with_nothing_allowed_no_command_runs() {
    let (mut rig, _, asked) = gated(Some(record(1, "store test"))).await;
    rig.machine.pass().await;
    assert_eq!(commands(&mut rig), (vec![], String::new()));
    assert!(asked.lock().unwrap().contains(&"settings_get".to_string()));
}

#[tokio::test]
async fn before_the_first_read_answers_nothing_runs_either() {
    let (mut rig, _, _) = gated(None).await;
    rig.machine.pass().await;
    assert_eq!(commands(&mut rig), (vec![], String::new()));
}

#[tokio::test]
async fn an_allowed_version_runs_while_a_changed_store_command_waits_until_approved() {
    let (mut rig, _, _) = gated(Some(record(2, "new test"))).await;
    allow(&rig, 1, "old test");
    rig.machine.pass().await;
    assert_eq!(commands(&mut rig), (vec!["old test".into()], "play old test".into()));
    let gates = rig.machine.links.item_commands.gates().clone();
    gates.approve("p0", 2).unwrap();
    rig.machine.pass().await;
    assert_eq!(commands(&mut rig), (vec!["new test".into()], "play new test".into()));
}

#[tokio::test]
async fn a_failed_settings_read_keeps_the_allowed_commands() {
    let (mut rig, settings, _) = gated(Some(record(1, "old test"))).await;
    allow(&rig, 1, "old test");
    rig.machine.pass().await;
    *settings.lock().unwrap() = None;
    rig.machine.pass().await;
    assert_eq!(commands(&mut rig), (vec!["old test".into()], "play old test".into()));
}

#[tokio::test]
async fn a_changed_command_waits_the_old_one_runs_and_approve_commands_makes_the_new_one_run() {
    let (mut rig, _, _) = gated(Some(record(2, "new test"))).await;
    allow(&rig, 1, "old test");
    rig.machine.pass().await;
    assert_eq!(commands(&mut rig), (vec!["old test".into()], "play old test".into()));
    let mut view = crate::view::AgentView::blank();
    view.item_commands = rig.machine.links.item_commands.clone();
    view.work = rig.machine.links.work.clone();
    view.owner.set("jc".into()).unwrap();
    let now = crate::telemetry::events::now_ms();
    let cmd = crate::commands::Command {
        id: "a1".into(),
        name: "approve_commands".into(),
        args: json!({ "version": 2 }),
        issued_by: "jc".into(),
        issued_at: now,
        expires_at: now + 60_000,
    };
    let answer = crate::commands::run(&cmd, "p0", &mut Default::default(), &view, &|_| {}, rig._dir.path());
    assert_eq!(answer, json!({ "type": "ack", "id": "a1" }));
    rig.machine.pass().await;
    assert_eq!(commands(&mut rig), (vec!["new test".into()], "play new test".into()));
}
