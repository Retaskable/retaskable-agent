//! A queued `ideas_generate` starting one non-quiet ideas round in the background (`driver/ideas_round.rs`).
use super::*;
use crate::view::ItemCommand;
use axum::response::IntoResponse;
use std::path::Path;

const IDEAS: &str = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"ok","ideas":[{"title":"New one","experience":"e","why":"w","whyNow":"n","for":[]}]},"total_cost_usd":0.1,"session_id":"s"}"#;

type Seen = Arc<Mutex<Vec<(String, Value)>>>;

/// A store with one waiting idea; `lock_delay` holds the lock-taking answer back so a round stays in flight.
async fn store(lock_delay: Duration) -> (String, Seen) {
    let asked: Seen = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                let take = body["args"]["take"].as_bool().unwrap_or(false);
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                let value = match op.as_str() {
                    "list" => json!([]),
                    "ideas_list" | "ideas_round" => json!({ "round": null, "ideas": [{
                        "id": "i1", "title": "Old", "state": "waiting", "at": 1,
                        "idea": { "experience": "e", "why": "w", "whyNow": "n", "for": [], "waited": 0 } }] }),
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

/// A live rig with the flag On and the writer session the fake claude.
fn live_rig(base: &str, flag: bool) -> Rig {
    let mut rig = rig(2, &[base]);
    rig.go_live();
    let dir = rig._dir.path().to_path_buf();
    let bin = fake_claude(&dir);
    let missing_gh = dir.join("no-gh").to_string_lossy().into_owned();
    rig.machine.links.env = Env::new(move |k| match k {
        "RETASKABLE_CLAUDE" => Some(bin.clone()),
        "RETASKABLE_GH" => Some(missing_gh.clone()),
        _ => None,
    });
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(if flag { &["ideas-session"] } else { &[] }));
    rig.lp(0).project.repo_path = dir.to_string_lossy().into_owned();
    rig.lp(0).project.sandbox = crate::config::SandboxMode::Off;
    rig
}

fn send(rig: &Rig, fresh: bool) {
    (rig.machine.links.item_commands).push(ItemCommand::Ideas {
        project: "p0".into(),
        fresh,
    });
}

fn ideas_ops(asked: &Seen) -> Vec<String> {
    let mut ops: Vec<String> = asked.lock().unwrap().iter().map(|(o, _)| o.clone()).collect();
    ops.retain(|o| o.starts_with("ideas") || o == "chore_lock");
    ops
}

async fn until_dropped(asked: &Seen) {
    for _ in 0..200 {
        if ideas_ops(asked).len() >= 4 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("round didn't finish: {:?}", ideas_ops(asked));
}

#[tokio::test]
async fn a_queued_command_runs_one_round_that_keeps_the_waiting_ideas() {
    let (base, asked) = store(Duration::ZERO).await;
    let mut rig = live_rig(&base, true);
    send(&rig, false);
    rig.machine.poll().await;
    until_dropped(&asked).await;
    assert_eq!(
        ideas_ops(&asked),
        vec!["chore_lock", "ideas_list", "ideas_round", "chore_lock"]
    );
    let sent = asked
        .lock()
        .unwrap()
        .iter()
        .find(|(o, _)| o == "ideas_round")
        .unwrap()
        .1
        .clone();
    assert_eq!(
        sent["ideas"].as_array().unwrap().len(),
        2,
        "the waiting idea stays: {sent}"
    );
}

#[tokio::test]
async fn fresh_drops_the_waiting_ideas() {
    let (base, asked) = store(Duration::ZERO).await;
    let mut rig = live_rig(&base, true);
    send(&rig, true);
    rig.machine.poll().await;
    until_dropped(&asked).await;
    let sent = asked
        .lock()
        .unwrap()
        .iter()
        .find(|(o, _)| o == "ideas_round")
        .unwrap()
        .1
        .clone();
    let titles: Vec<&str> = sent["ideas"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["title"].as_str())
        .collect();
    assert_eq!(titles, vec!["New one"]);
}

#[tokio::test]
async fn a_second_command_while_a_round_runs_starts_nothing() {
    let (base, asked) = store(Duration::from_millis(500)).await;
    let mut rig = live_rig(&base, true);
    send(&rig, false);
    rig.machine.poll().await;
    send(&rig, false);
    rig.machine.poll().await;
    until_dropped(&asked).await;
    assert_eq!(ideas_ops(&asked).iter().filter(|o| *o == "ideas_list").count(), 1);
}

#[tokio::test]
async fn nothing_runs_with_the_flag_off() {
    let (base, asked) = store(Duration::ZERO).await;
    let mut rig = live_rig(&base, false);
    send(&rig, false);
    rig.machine.poll().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(ideas_ops(&asked), Vec::<String>::new());
}
