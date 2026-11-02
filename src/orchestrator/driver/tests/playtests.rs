//! The minute pass starting each project's try-out grouping round in the background.
use super::*;
use crate::config::Playtests;
use std::path::Path;

const GROUPED: &str = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"groups":[{"title":"Build","why":"same place","keys":["k1","k2","k3","k4"]}]},"total_cost_usd":0.1,"session_id":"s"}"#;

/// A store answering `list` with no items, `tryouts_list` with `waiting` (4 by default) and the
/// lock as free; `lock_delay` holds the lock-taking answer back so a round stays in flight.
async fn store(waiting: usize, lock_delay: Duration, fail_list: bool) -> (String, Seen) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                let take = body["args"]["take"].as_bool().unwrap_or(false);
                seen.lock().unwrap().push(op.clone());
                if op == "list" && fail_list {
                    return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))).into_response();
                }
                let value = match op.as_str() {
                    "list" => json!([]),
                    "tryouts_list" => {
                        let waiting: Vec<Value> = (1..=waiting)
                            .map(|n| json!({"key": format!("k{n}"), "itemId": "1", "title": "T", "what": "W", "steps": ["S"]}))
                            .collect();
                        json!({"waiting": waiting, "groups": null})
                    }
                    "chore_lock" => {
                        if take {
                            tokio::time::sleep(lock_delay).await;
                        }
                        json!({"held": take, "by": "my-pc"})
                    }
                    _ => Value::Null,
                };
                Json(json!({ "value": value })).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

use axum::response::IntoResponse;
type Seen = Arc<Mutex<Vec<String>>>;

fn fake_claude(dir: &Path) -> String {
    let bin = dir.join(if cfg!(windows) { "claude.cmd" } else { "claude.sh" });
    let out = dir.join("out.jsonl");
    std::fs::write(&out, GROUPED).unwrap();
    if cfg!(windows) {
        std::fs::write(
            &bin,
            format!("@echo off\r\ntype \"{}\"\r\nexit /b 0\r\n", out.display()),
        )
        .unwrap();
    } else {
        crate::test_support::write_executable(&bin, &format!("#!/bin/sh\ncat \"{}\"\nexit 0\n", out.display()));
    }
    bin.to_string_lossy().into_owned()
}

/// A live rig over `base` whose planner session is the fake claude.
fn live_rig(base: &str) -> Rig {
    let mut rig = rig(2, &[base]);
    rig.go_live();
    let bin = fake_claude(rig._dir.path());
    rig.machine.links.env = Env::new(move |k| (k == "RETASKABLE_CLAUDE").then(|| bin.clone()));
    let cwd = rig._dir.path().to_string_lossy().into_owned();
    rig.lp(0).project.repo_path = cwd;
    rig.lp(0).project.sandbox = crate::config::SandboxMode::Off;
    rig
}

async fn minute(rig: &mut Rig) {
    let steps = rig.machine.pass().await;
    rig.machine.playtest_rounds(&steps).await;
}

fn grouping_ops(asked: &Seen) -> Vec<String> {
    let mut ops = all_ops(asked);
    ops.retain(|o| o.starts_with("tryout") || o == "chore_lock");
    ops
}

async fn until_dropped(asked: &Seen) {
    for _ in 0..200 {
        if grouping_ops(asked).len() >= 4 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("round didn't finish: {:?}", grouping_ops(asked));
}

async fn settled_without_round(asked: &Seen) {
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(grouping_ops(asked), Vec::<String>::new());
}

#[tokio::test]
async fn a_claim_pass_with_enough_waiting_runs_one_round() {
    let (base, asked) = store(4, Duration::ZERO, false).await;
    let mut rig = live_rig(&base);
    minute(&mut rig).await;
    until_dropped(&asked).await;
    assert_eq!(
        grouping_ops(&asked),
        vec!["tryouts_list", "chore_lock", "tryout_groups", "chore_lock"]
    );
}

#[tokio::test]
async fn playtests_off_runs_no_round() {
    let (base, asked) = store(4, Duration::ZERO, false).await;
    let mut rig = live_rig(&base);
    rig.lp(0).project.playtests = Playtests::Off;
    minute(&mut rig).await;
    settled_without_round(&asked).await;
}

#[tokio::test]
async fn a_paused_project_runs_no_round() {
    let (base, asked) = store(4, Duration::ZERO, false).await;
    let mut rig = live_rig(&base);
    rig.pause("p0", true);
    minute(&mut rig).await;
    settled_without_round(&asked).await;
}

#[tokio::test]
async fn dry_mode_runs_no_round() {
    let (base, asked) = store(4, Duration::ZERO, false).await;
    let mut rig = live_rig(&base);
    rig.lp(0).mode = OrchestratorMode::Dry;
    minute(&mut rig).await;
    settled_without_round(&asked).await;
}

#[tokio::test]
async fn a_failed_read_runs_no_round() {
    let (base, asked) = store(4, Duration::ZERO, true).await;
    let mut rig = live_rig(&base);
    minute(&mut rig).await;
    settled_without_round(&asked).await;
}

#[tokio::test]
async fn a_round_in_flight_isnt_started_again_and_never_blocks_the_pass() {
    let (base, asked) = store(4, Duration::from_millis(500), false).await;
    let mut rig = live_rig(&base);
    let started = std::time::Instant::now();
    minute(&mut rig).await;
    minute(&mut rig).await;
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "the pass waited for the round"
    );
    until_dropped(&asked).await;
    assert_eq!(all_ops(&asked).iter().filter(|o| *o == "tryouts_list").count(), 1);
}

fn all_ops(seen: &Seen) -> Vec<String> {
    seen.lock().unwrap().clone()
}
