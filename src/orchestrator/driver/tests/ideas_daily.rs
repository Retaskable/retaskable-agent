//! The minute pass starting each project's daily round of ideas in the background.
use super::*;
use std::path::Path;

const IDEAS: &str = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"ok","ideas":[{"title":"New one","experience":"e","why":"w","whyNow":"n","for":[]}]},"total_cost_usd":0.1,"session_id":"s"}"#;

type Seen = Arc<Mutex<Vec<String>>>;

/// A store answering `jobs_due` with `due` (or an error), `list` with no items and the lock as free.
async fn store(due: bool, fail_due: bool, lock_delay: Duration) -> (String, Seen) {
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
                if op == "jobs_due" && fail_due {
                    return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))).into_response();
                }
                let value = match op.as_str() {
                    "jobs_due" => json!({ "day": "2026-11-03", "due": if due { json!(["ideas"]) } else { json!([]) } }),
                    "list" => json!([]),
                    "ideas_list" | "ideas_round" => json!({ "round": null, "ideas": [] }),
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

fn fake_claude(dir: &Path) -> String {
    let bin = dir.join(if cfg!(windows) { "claude.cmd" } else { "claude.sh" });
    let out = dir.join("out.jsonl");
    std::fs::write(&out, IDEAS).unwrap();
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

fn live_rig(base: &str, flag: bool) -> Rig {
    let mut rig = rig(2, &[base]);
    rig.go_live();
    let bin = fake_claude(rig._dir.path());
    rig.machine.links.env = Env::new(move |k| (k == "RETASKABLE_CLAUDE").then(|| bin.clone()));
    let cwd = rig._dir.path().to_string_lossy().into_owned();
    rig.lp(0).project.repo_path = cwd;
    rig.lp(0).project.sandbox = crate::config::SandboxMode::Off;
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(if flag { &["ideas-session"] } else { &[] }));
    rig
}

async fn minute(rig: &mut Rig) {
    let steps = rig.machine.pass().await;
    rig.machine.ideas_rounds(&steps).await;
}

fn ideas_ops(asked: &Seen) -> Vec<String> {
    let mut ops = asked.lock().unwrap().clone();
    ops.retain(|o| o.starts_with("ideas") || o == "chore_lock" || o == "jobs_due");
    ops
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(400)).await;
}

#[tokio::test]
async fn a_due_day_runs_one_round_and_the_same_day_isnt_started_twice() {
    let (base, asked) = store(true, false, Duration::ZERO).await;
    let mut rig = live_rig(&base, true);
    minute(&mut rig).await;
    for _ in 0..200 {
        if ideas_ops(&asked).len() >= 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    settle().await;
    minute(&mut rig).await;
    settle().await;
    assert_eq!(
        ideas_ops(&asked),
        vec!["jobs_due", "chore_lock", "ideas_list", "ideas_round", "chore_lock"]
    );
}

#[tokio::test]
async fn the_same_day_isnt_tried_again_once_the_asker_may_ask_again() {
    let (base, asked) = store(true, false, Duration::ZERO).await;
    let mut rig = live_rig(&base, true);
    minute(&mut rig).await;
    settle().await;
    rig.lp(0).ideas_daily.lock().unwrap().forget_asker();
    minute(&mut rig).await;
    settle().await;
    assert_eq!(ideas_ops(&asked).iter().filter(|o| *o == "jobs_due").count(), 2);
    assert_eq!(ideas_ops(&asked).iter().filter(|o| *o == "ideas_round").count(), 1);
}

#[tokio::test]
async fn nothing_runs_when_paused_flag_off_not_due_or_store_down() {
    for (due, fail, flag, paused, asks) in [
        (true, false, true, true, 0),
        (true, false, false, false, 0),
        (false, false, true, false, 1),
        (true, true, true, false, 1),
    ] {
        let (base, asked) = store(due, fail, Duration::ZERO).await;
        let mut rig = live_rig(&base, flag);
        rig.pause("p0", paused);
        minute(&mut rig).await;
        settle().await;
        let ops = ideas_ops(&asked);
        assert_eq!(ops.iter().filter(|o| *o == "jobs_due").count(), asks, "{ops:?}");
        assert!(
            !ops.iter().any(|o| o.starts_with("ideas") || o == "chore_lock"),
            "{ops:?}"
        );
    }
}

#[tokio::test]
async fn a_round_still_running_blocks_another() {
    let (base, asked) = store(true, false, Duration::from_millis(600)).await;
    let mut rig = live_rig(&base, true);
    minute(&mut rig).await;
    settle().await;
    minute(&mut rig).await;
    settle().await;
    assert_eq!(ideas_ops(&asked).iter().filter(|o| *o == "jobs_due").count(), 1);
}
