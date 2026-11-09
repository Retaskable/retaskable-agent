//! `approve-commands-gate` in the loop (`driver/project.rs`'s `take_allowed_commands`): a project's commands are the
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

/// A rig whose project's config.json has `file`'s commands and whose gate flag is `on`.
async fn gated(on: bool, settings: Option<Value>) -> (Rig, Arc<Mutex<Option<Value>>>, Arc<Mutex<Vec<String>>>) {
    let settings = Arc::new(Mutex::new(settings));
    let (base, asked) = store(settings.clone()).await;
    let mut rig = rig(2, &[&base]);
    let dir = rig._dir.path().to_path_buf();
    let lp = rig.lp(0);
    lp.flags = Arc::new(crate::flags::Flags::fixed(if on {
        &["approve-commands-gate"]
    } else {
        &[]
    }));
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
    let (mut rig, _, asked) = gated(true, Some(record(1, "store test"))).await;
    rig.machine.pass().await;
    assert_eq!(commands(&mut rig), (vec![], String::new()));
    assert!(asked.lock().unwrap().contains(&"settings_get".to_string()));
}

#[tokio::test]
async fn before_the_first_read_answers_nothing_runs_either() {
    let (mut rig, _, _) = gated(true, None).await;
    rig.machine.pass().await;
    assert_eq!(commands(&mut rig), (vec![], String::new()));
}

#[tokio::test]
async fn an_allowed_version_runs_while_a_changed_store_command_waits_until_approved() {
    let (mut rig, _, _) = gated(true, Some(record(2, "new test"))).await;
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
    let (mut rig, settings, _) = gated(true, Some(record(1, "old test"))).await;
    allow(&rig, 1, "old test");
    rig.machine.pass().await;
    *settings.lock().unwrap() = None;
    rig.machine.pass().await;
    assert_eq!(commands(&mut rig), (vec!["old test".into()], "play old test".into()));
}

#[tokio::test]
async fn with_the_flag_off_config_json_runs_and_settings_are_not_read() {
    let (mut rig, _, asked) = gated(false, Some(record(1, "store test"))).await;
    rig.machine.pass().await;
    assert_eq!(commands(&mut rig), (vec!["file test".into()], "file play".into()));
    assert!(!asked.lock().unwrap().contains(&"settings_get".to_string()));
}
