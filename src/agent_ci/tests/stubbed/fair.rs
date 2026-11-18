//! docs/specs/agent-ci.md section 3.1 (The slot): free permits are shared fairly across repos.
use super::*;

/// `pr_rig` with a second repo, `o/q`, over the same stubbed origin and `gh`, and `slots` permits.
async fn two_repos(slots: u32) -> Rig {
    let mut r = pr_rig("wait_gate");
    r.cfg.repos.push(Repo::parse("o/q").unwrap());
    r.cfg.slots = slots;
    r.ci.poll(&r.cfg).await;
    r
}

fn keys(from: u64, n: u64) -> Vec<CheckKey> {
    (from..from + n).map(|i| key(i, &format!("h{i}"))).collect()
}

/// Checks holding a permit now, per repo id. Spawned checks haven't run yet when `start` returns on the test's
/// single thread, so this is what that pass took.
fn running(r: &Rig, id: &str) -> usize {
    r.ci.running.lock().unwrap().iter().filter(|(i, _)| i == id).count()
}

async fn finish(r: &Rig, tasks: Vec<tokio::task::JoinHandle<()>>) {
    open_gate(r);
    for t in tasks {
        t.await.unwrap();
    }
}

#[tokio::test]
async fn two_repos_with_more_keys_than_slots_share_them() {
    let mut r = two_repos(4).await;
    let tasks =
        r.ci.start(
            &r.cfg,
            [("o-p".into(), keys(100, 6)), ("o-q".into(), keys(200, 6))].into(),
        )
        .await;
    assert_eq!((running(&r, "o-p"), running(&r, "o-q")), (2, 2));
    finish(&r, tasks).await;
}

#[tokio::test]
async fn a_repo_with_one_waiting_key_gets_a_slot_in_the_first_pass_whoever_goes_first() {
    for turn in [0, 1] {
        let mut r = two_repos(2).await;
        r.ci.turn = turn;
        let tasks =
            r.ci.start(
                &r.cfg,
                [("o-p".into(), keys(100, 10)), ("o-q".into(), keys(200, 1))].into(),
            )
            .await;
        assert_eq!((running(&r, "o-p"), running(&r, "o-q")), (1, 1), "turn {turn}");
        finish(&r, tasks).await;
    }
}

#[tokio::test]
async fn a_lone_repo_uses_every_slot() {
    let mut r = two_repos(3).await;
    let tasks =
        r.ci.start(&r.cfg, [("o-p".into(), keys(100, 10)), ("o-q".into(), vec![])].into())
            .await;
    assert_eq!((running(&r, "o-p"), running(&r, "o-q")), (3, 0));
    finish(&r, tasks).await;
}

#[tokio::test]
async fn the_repo_taken_first_rotates_each_pass() {
    let mut r = two_repos(1).await;
    let all = || [("o-p".into(), keys(100, 3)), ("o-q".into(), keys(200, 3))].into();
    let tasks = r.ci.start(&r.cfg, all()).await;
    assert_eq!((running(&r, "o-p"), running(&r, "o-q")), (1, 0));
    finish(&r, tasks).await;
    let tasks = r.ci.start(&r.cfg, all()).await;
    // The one permit went to the other repo this time (the gate is open, so a check may already be over).
    let seen = r.ci.seen.lock().unwrap().clone();
    assert_eq!(seen.iter().filter(|(i, _)| i == "o-p").count(), 1);
    assert_eq!(seen.iter().filter(|(i, _)| i == "o-q").count(), 1);
    for t in tasks {
        t.await.unwrap();
    }
}
