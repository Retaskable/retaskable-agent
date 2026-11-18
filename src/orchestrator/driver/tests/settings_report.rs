//! The settings report in the loop: what reaches the store as `settings_report`, checked against the store's rules.
use super::*;
use crate::allowed_commands::{AllowedCommands, allowed_commands_file};
use crate::backlog::store_contract::check_settings_report;

/// A store that answers `settings_get` with `settings` and keeps the args of each `settings_report`.
async fn store(settings: Value, refuse: Arc<AtomicBool>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let reports = Arc::new(Mutex::new(Vec::new()));
    let seen = reports.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, settings, refuse) = (seen.clone(), settings.clone(), refuse.clone());
            async move {
                use axum::response::IntoResponse;
                match body["op"].as_str().unwrap_or("") {
                    "settings_get" => Json(json!({ "value": settings })).into_response(),
                    "settings_report" if refuse.load(Ordering::SeqCst) => {
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                    "settings_report" => {
                        seen.lock().unwrap().push(body["args"].clone());
                        Json(json!({ "value": {} })).into_response()
                    }
                    "pause_get" => Json(json!({ "value": { "paused": null, "log": [] } })).into_response(),
                    _ => Json(json!({ "value": [] })).into_response(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), reports)
}

fn settings(version: u64, commands_version: u64) -> Value {
    json!({ "version": version, "commandsVersion": commands_version, "values": {} })
}

async fn rig_with(settings: Value, refuse: bool) -> (Rig, Arc<Mutex<Vec<Value>>>, Arc<AtomicBool>) {
    let refuse = Arc::new(AtomicBool::new(refuse));
    let (base, reports) = store(settings, refuse.clone()).await;
    let mut rig = rig(2, &[&base]);
    let dir = rig._dir.path().to_path_buf();
    rig.machine.links.item_commands = Default::default();
    rig.lp(0).data_dir = Some(dir);
    (rig, reports, refuse)
}

#[tokio::test]
async fn the_computer_tells_the_store_what_it_allows_and_has_applied() {
    let (mut rig, reports, _) = rig_with(settings(9, 4), false).await;
    AllowedCommands {
        commands_version: 4,
        values: Default::default(),
        at: 0,
    }
    .save(&allowed_commands_file(rig._dir.path(), "p0"))
    .unwrap();
    rig.machine.pass().await;
    let sent = reports.lock().unwrap().clone();
    assert_eq!(
        sent,
        vec![json!({ "machine": "my-pc", "allowed": 4, "appliedVersion": 9, "migrated": true })]
    );
    check_settings_report(&sent[0]).unwrap();
    // The same report is not sent again on the next pass.
    rig.machine.pass().await;
    assert_eq!(reports.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_report_the_store_did_not_take_is_sent_again_and_none_goes_before_there_are_settings() {
    let (mut rig, reports, refuse) = rig_with(settings(1, 0), true).await;
    rig.machine.pass().await;
    assert!(reports.lock().unwrap().is_empty());
    refuse.store(false, Ordering::SeqCst);
    rig.machine.pass().await;
    let sent = reports.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["allowed"], 0);
    check_settings_report(&sent[0]).unwrap();

    let (mut rig, reports, _) = rig_with(Value::Null, false).await;
    rig.machine.pass().await;
    assert!(reports.lock().unwrap().is_empty(), "no settings: nothing to report");
}
