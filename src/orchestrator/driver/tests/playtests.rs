//! A project whose playtests setting is Off releases the runs waiting for a person's try-out, once.
use super::*;
use crate::config::Playtests;
use axum::response::IntoResponse;

/// A live rig over `base` whose project is a checkout-less test one.
fn live_rig(base: &str) -> Rig {
    let mut rig = rig(2, &[base]);
    rig.go_live();
    let cwd = rig._dir.path().to_string_lossy().into_owned();
    rig.lp(0).project.repo_path = cwd;
    rig.lp(0).file_project.repo_path = rig.lp(0).project.repo_path.clone();
    rig
}

/// A rig whose items "a", "b", "c" wait for a playtest, each with a queued failure and an owed answer.
fn three_waiting(playtests: Playtests) -> (Rig, Arc<Fake>) {
    let mut rig = rig(2, &["http://127.0.0.1:1"]);
    let fake = rig.go_live();
    rig.queue(0, &["a", "b", "c"]);
    let lp = rig.lp(0);
    lp.project.playtests = playtests;
    lp.file_project.playtests = playtests;
    for id in ["a", "b", "c"] {
        lp.runs.get_mut(id).unwrap().state = ItemState::AwaitingPlaytest;
        lp.playtest_fails.push((id.into(), Some("broken".into())));
        lp.unanswered.insert(id.into(), (false, None));
    }
    (rig, fake)
}

fn released_events(rig: &mut Rig) -> usize {
    rig.lp(0)
        .events()
        .recent()
        .iter()
        .map(Entry::to_value)
        .filter(|e| e["kind"] == "item_state" && e["from"] == "awaiting_playtest")
        .count()
}

#[tokio::test]
async fn playtests_off_release_every_waiting_run_once() {
    let (mut rig, fake) = three_waiting(Playtests::Off);
    rig.machine.poll().await;
    for id in ["a", "b", "c"] {
        assert_eq!(rig.lp(0).runs[id].state, ItemState::Merging);
    }
    assert_eq!(released_events(&mut rig), 3);
    assert!(rig.lp(0).playtest_fails.is_empty());
    assert!(rig.lp(0).unanswered.is_empty());
    assert!(fake.started.lock().unwrap().is_empty());
    rig.machine.poll().await;
    assert_eq!(released_events(&mut rig), 3);
}

#[tokio::test]
async fn playtests_on_or_after_merge_leave_waiting_runs_alone() {
    for playtests in [Playtests::On, Playtests::AfterMerge] {
        let (mut rig, _fake) = three_waiting(playtests);
        rig.lp(0).playtest_fails.clear();
        rig.machine.poll().await;
        for id in ["a", "b", "c"] {
            assert_eq!(rig.lp(0).runs[id].state, ItemState::AwaitingPlaytest);
        }
        assert_eq!(released_events(&mut rig), 0);
    }
}

/// A store counting `release_playtests` calls: the first `fail` of them are refused with a 500, and a taken one
/// lets go of every try-out waiting (`waiting` goes to zero).
async fn release_store(fail: usize, waiting: usize) -> (String, Arc<Mutex<(usize, usize)>>) {
    let state = Arc::new(Mutex::new((0usize, waiting)));
    let seen = state.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                let mut s = seen.lock().unwrap();
                let value = match op.as_str() {
                    "release_playtests" => {
                        s.0 += 1;
                        if s.0 <= fail {
                            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))).into_response();
                        }
                        s.1 = 0;
                        Value::Null
                    }
                    "tryouts_list" => json!({"waiting": [], "groups": null}),
                    _ => json!([]),
                };
                Json(json!({ "value": value })).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), state)
}

fn set_playtests(rig: &mut Rig, value: Playtests) {
    rig.lp(0).file_project.playtests = value;
}

#[tokio::test]
async fn switching_to_off_releases_once_and_later_passes_send_nothing() {
    let (base, state) = release_store(0, 3).await;
    let mut rig = live_rig(&base);
    set_playtests(&mut rig, Playtests::AfterMerge);
    rig.machine.pass().await;
    assert_eq!(state.lock().unwrap().0, 0);
    set_playtests(&mut rig, Playtests::Off);
    rig.machine.pass().await;
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(*state.lock().unwrap(), (1, 0), "one call, and none left waiting");
    assert!(!rig.lp(0).release_owed);
}

#[tokio::test]
async fn a_first_run_already_off_sends_nothing() {
    let (base, state) = release_store(0, 3).await;
    let mut rig = live_rig(&base);
    set_playtests(&mut rig, Playtests::Off);
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(state.lock().unwrap().0, 0);
}

#[tokio::test]
async fn a_failed_release_is_sent_again_then_stops_and_is_not_kept_in_the_state_file() {
    let (base, state) = release_store(1, 3).await;
    let mut rig = live_rig(&base);
    set_playtests(&mut rig, Playtests::On);
    rig.machine.pass().await;
    set_playtests(&mut rig, Playtests::Off);
    rig.machine.pass().await;
    assert_eq!(state.lock().unwrap().0, 1);
    assert!(rig.lp(0).release_owed);
    let saved = std::fs::read_to_string(rig._dir.path().join("0").join("state.json")).unwrap_or_default();
    assert!(!saved.contains("releaseOwed") && !saved.contains("playtestsSeen"));
    rig.machine.pass().await;
    assert_eq!(state.lock().unwrap().0, 2);
    assert!(!rig.lp(0).release_owed);
    rig.machine.pass().await;
    assert_eq!(state.lock().unwrap().0, 2);
}

#[tokio::test]
async fn off_then_on_then_off_sends_twice() {
    let (base, state) = release_store(0, 3).await;
    let mut rig = live_rig(&base);
    set_playtests(&mut rig, Playtests::On);
    rig.machine.pass().await;
    for value in [Playtests::Off, Playtests::On, Playtests::Off] {
        set_playtests(&mut rig, value);
        rig.machine.pass().await;
    }
    assert_eq!(state.lock().unwrap().0, 2);
}
