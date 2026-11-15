//! The runner flag the CI watcher reads: true only while the store's last poll answer made this computer the runner.
use super::*;

fn flagged(f: &Fake) -> (Rollout<Fake>, Runners) {
    let runners = Runners::default();
    (rollout(f).with_runner(runners.flag("p")), runners)
}

#[tokio::test(start_paused = true)]
async fn the_flag_follows_the_polls_answers() {
    let f = Fake::default();
    let (mut r, runners) = flagged(&f);
    assert!(!runners.is_runner("p"), "false until a poll says so");
    tell(&f, json!({ "idle": true, "epoch": 4 }));
    r.step().await;
    assert!(runners.is_runner("p"));
    // A store error says nothing: the flag stays.
    f.0.lock().unwrap().answers.push_back(Err("down".into()));
    r.step().await;
    assert!(runners.is_runner("p"));
    // An idle answer to another computer carries no epoch.
    tell(&f, json!({ "idle": true, "why": "another computer holds it" }));
    r.step().await;
    assert!(!runners.is_runner("p"));
    tell(&f, json!({ "idle": true, "epoch": 5 }));
    r.step().await;
    assert!(runners.is_runner("p"));
    tell(&f, json!({ "held": false }));
    r.step().await;
    assert!(!runners.is_runner("p"));
}

#[tokio::test(start_paused = true)]
async fn a_beat_that_says_not_held_clears_it() {
    let f = Fake::default();
    f.0.lock().unwrap().run_for = Duration::from_secs(40);
    let (mut r, runners) = flagged(&f);
    let mut told = order("a", "traffic", SHA, Some(5));
    told["epoch"] = json!(7);
    tell(&f, told);
    r.step().await;
    assert!(!runners.is_runner("p"));
}

#[test]
fn each_project_has_its_own_flag() {
    let runners = Runners::default();
    runners.flag("a").store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(runners.is_runner("a") && !runners.is_runner("b"));
}
