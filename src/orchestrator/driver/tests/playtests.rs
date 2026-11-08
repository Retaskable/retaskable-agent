//! The minute pass starting each project's try-out grouping round in the background.
use super::*;
use crate::backlog::runs::RunKind;
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

/// A store for a `runs`-mode round: the grouping lock is `lock_take`n as token `L1`, `run_beat` answers `beat`, and
/// every op asked is recorded with its args.
async fn runs_store(beat: Value) -> (String, Arc<Mutex<Vec<(String, Value)>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            let beat = beat.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                let value = match op.as_str() {
                    "list" => json!([]),
                    "tryouts_list" => {
                        let waiting: Vec<Value> = (1..=4)
                            .map(|n| json!({"key": format!("k{n}"), "itemId": "1", "title": "T", "what": "W", "steps": ["S"]}))
                            .collect();
                        json!({"waiting": waiting, "groups": null})
                    }
                    "lock_take" => json!({"held": true, "token": "L1"}),
                    "run_beat" => beat,
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

struct NoRuns;
impl RunStarter for NoRuns {
    fn kinds(&self) -> Vec<RunKind> {
        Vec::new()
    }
    fn start(&self, _request: RunRequest) -> Result<(), String> {
        Err("none".into())
    }
}

fn runs_ops(asked: &Arc<Mutex<Vec<(String, Value)>>>) -> Vec<String> {
    let mut ops: Vec<String> = asked.lock().unwrap().iter().map(|(o, _)| o.clone()).collect();
    ops.retain(|o| o.starts_with("tryout") || o.starts_with("lock_") || o == "chore_lock" || o == "run_beat");
    ops
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never saw {what}");
}

/// A `runs`-mode project whose planner session takes a second, so the lock is held while the loop beats.
#[cfg(unix)]
async fn runs_round(beat: Value) -> (Rig, Arc<Mutex<Vec<(String, Value)>>>) {
    let (base, asked) = runs_store(beat).await;
    let mut rig = live_rig(&base);
    rig.lp(0).mode = OrchestratorMode::Runs;
    let dir = rig._dir.path();
    let bin = dir.join("slow.sh");
    crate::test_support::write_executable(
        &bin,
        &format!(
            "#!/bin/sh\nsleep 1\ncat \"{}\"\nexit 0\n",
            dir.join("out.jsonl").display()
        ),
    );
    let bin = bin.to_string_lossy().into_owned();
    rig.machine.links.env = Env::new(move |k| (k == "RETASKABLE_CLAUDE").then(|| bin.clone()));
    minute(&mut rig).await;
    until("lock_take", || runs_ops(&asked).contains(&"lock_take".to_string())).await;
    (rig, asked)
}

/// One run-loop pass a minute from now, which beats whatever tokens are held.
#[cfg(unix)]
async fn beat(rig: &mut Rig) {
    let m = &mut rig.machine;
    let shared = Shared {
        links: &m.links,
        local: &m.local,
        claim_paused_until: &mut None,
        claude_signed_out_until: &mut None,
        engine_limits: &mut Default::default(),
        stopping: false,
    };
    let now = std::time::Instant::now() + Duration::from_secs(61);
    m.projects[0].run_pass(&mut m.pool, &NoRuns, &shared, now).await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_runs_round_takes_and_drops_the_lock_by_token_and_the_beat_carries_it() {
    let (mut rig, asked) = runs_round(json!({})).await;
    beat(&mut rig).await;
    let running = rig.lp(0).playtest_round_running.clone();
    until("the round's end", || !running.load(Ordering::SeqCst)).await;
    assert_eq!(
        runs_ops(&asked),
        vec!["tryouts_list", "lock_take", "run_beat", "tryout_groups", "lock_drop"]
    );
    let recorded = asked.lock().unwrap();
    let beats: Vec<_> = recorded.iter().filter(|(o, _)| o == "run_beat").collect();
    assert_eq!(beats[0].1["tokens"], json!(["L1"]));
    let take = recorded.iter().find(|(o, _)| o == "lock_take").unwrap();
    assert_eq!(take.1["name"], crate::playtest_groups::TRYOUT_GROUPS_LOCK);
    let drop = recorded.iter().find(|(o, _)| o == "lock_drop").unwrap();
    assert_eq!(drop.1["token"], "L1");
}

#[cfg(unix)]
#[tokio::test]
async fn a_lock_lost_to_a_beat_stop_sends_no_grouping() {
    let (mut rig, asked) = runs_round(json!({ "L1": "stop" })).await;
    beat(&mut rig).await;
    let running = rig.lp(0).playtest_round_running.clone();
    until("the round's end", || !running.load(Ordering::SeqCst)).await;
    assert_eq!(runs_ops(&asked), vec!["tryouts_list", "lock_take", "run_beat"]);
}
