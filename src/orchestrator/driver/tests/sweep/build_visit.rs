//! **Play this build** of a slice another computer holds (`visit`), and **Restart** by item (`driver/build_play.rs`).
use std::path::{Path, PathBuf};
use std::process::Command;

use super::build_play::{answered, git, nack, play, rig_on, rig_with_item, stop};
use super::*;
use crate::view::ItemCommand;

/// A store that answers `tryouts_list` with `waiting` and every other op with `listed`.
async fn store(waiting: Value) -> String {
    use axum::http::StatusCode;
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let waiting = waiting.clone();
            async move {
                let value = match body["op"].as_str() {
                    Some("tryouts_list") => json!({ "waiting": waiting, "groups": null }),
                    Some("pause_get") => json!({ "paused": null, "log": [] }),
                    Some("facts_get") => json!({}),
                    _ => json!([]),
                };
                (StatusCode::OK, Json(json!({ "value": value })))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn row(item: &str, on_branch: Option<(&str, &str)>) -> Value {
    json!({
        "key": format!("{item}@b"), "itemId": item, "title": item, "what": "w", "steps": [],
        "onBranch": on_branch.map(|(branch, sha)| json!({ "branch": branch, "sha": sha })),
    })
}

fn head(repo: &Path) -> String {
    let out = Command::new("git")
        .current_dir(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A repo whose origin has branch `feature`, two commits long: `first.txt`, then `second.txt`. The first commit's sha.
fn repo_with_feature(tmp: &Path) -> (PathBuf, String) {
    let repo = crate::test_support::repo_with_origin(tmp);
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("first.txt"), "1").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let first = head(&repo);
    std::fs::write(repo.join("second.txt"), "2").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "second"]);
    git(&repo, &["push", "-q", "origin", "feature"]);
    git(&repo, &["checkout", "-q", "main"]);
    (repo, first)
}

fn restart(rig: &Rig, item: &str, id: &str) {
    rig.machine.links.item_commands.push(ItemCommand::RestartItem {
        project: "p0".into(),
        item: item.into(),
        command_id: id.into(),
    });
}

/// The notes on `item`'s timeline, as `item_state` events.
fn notes(rig: &mut Rig, item: &str) -> Vec<String> {
    (rig.lp(0).events().recent().iter())
        .map(|e| e.to_value())
        .filter(|v| v["kind"] == "item_state" && v["item"] == item)
        .filter_map(|v| v["note"].as_str().map(String::from))
        .collect()
}

fn playing_item(rig: &mut Rig) -> Option<String> {
    rig.lp(0).live.snapshot().projects["p0"].playing_item.clone()
}

#[tokio::test]
async fn another_computers_slice_plays_from_its_try_out_row_at_the_approved_commit_and_is_in_the_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, first) = repo_with_feature(tmp.path());
    let base = store(json!([row("v", Some(("feature", &first)))])).await;
    let (mut rig, fake, mut rx) = rig_on(&base, Some(&repo));
    play(&rig, "v", "c1");
    assert_eq!(answered(&mut rig, &mut rx).await, json!({ "type": "ack", "id": "c1" }));
    let (dir, command, lane, closed) = fake.games.lock().unwrap()[0].clone();
    assert_eq!((command.as_str(), lane), ("the-game", 0));
    assert_eq!(dir, crate::lanes::play_path(&repo.parent().unwrap().join("wt")));
    assert!(dir.join("first.txt").exists(), "the row's commit");
    assert!(!dir.join("second.txt").exists(), "not the branch's tip");
    assert_eq!(playing_item(&mut rig).as_deref(), Some("v"));
    assert!(rig.machine.pool.held_lanes().next().is_none());
    // Again while it plays changes nothing; Stop closes it, and the snapshot forgets it.
    play(&rig, "v", "c2");
    assert_eq!(answered(&mut rig, &mut rx).await, json!({ "type": "ack", "id": "c2" }));
    assert_eq!(fake.games.lock().unwrap().len(), 1);
    stop(&rig, "v", "c3");
    assert_eq!(answered(&mut rig, &mut rx).await["type"], "ack");
    assert!(closed.load(Ordering::SeqCst));
    rig.machine.poll().await;
    assert_eq!(playing_item(&mut rig), None);
}

#[tokio::test]
async fn an_item_with_no_on_branch_row_is_refused_and_so_is_a_paused_computer() {
    let base = store(json!([row("merged", None), row("other", Some(("b", "s")))])).await;
    let (mut rig, fake, mut rx) = rig_on(&base, None);
    for (item, id) in [("merged", "c1"), ("nobody", "c2")] {
        play(&rig, item, id);
        assert_eq!(
            answered(&mut rig, &mut rx).await,
            nack(id, &format!("Item {item} isn't waiting for a try-out on its branch."))
        );
    }
    rig.pause("p0", true);
    play(&rig, "other", "c3");
    assert_eq!(
        answered(&mut rig, &mut rx).await,
        nack("c3", "This computer is paused, so it didn't start a build.")
    );
    assert!(fake.games.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_visited_slice_closes_the_build_that_was_playing_first() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, first) = repo_with_feature(tmp.path());
    let base = store(json!([row("v", Some(("feature", &first)))])).await;
    let (mut rig, fake, mut rx) = rig_on(&base, Some(&repo));
    // Item "a" is held here, on the same branch, and plays from the checkout first.
    play(&rig, "a", "c1");
    assert_eq!(answered(&mut rig, &mut rx).await["type"], "ack");
    assert_eq!(playing_item(&mut rig).as_deref(), Some("a"));
    play(&rig, "v", "c2");
    assert_eq!(answered(&mut rig, &mut rx).await, json!({ "type": "ack", "id": "c2" }));
    let games = fake.games.lock().unwrap().clone();
    assert_eq!(games.len(), 2);
    assert!(games[0].3.load(Ordering::SeqCst), "a's build was closed");
    assert!(!games[1].0.join("second.txt").exists());
    assert_eq!(playing_item(&mut rig).as_deref(), Some("v"));
}

#[tokio::test]
async fn a_restart_with_no_game_plays_and_one_in_a_lane_or_the_play_checkout_is_stopped_and_played_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, _) = repo_with_feature(tmp.path());
    let (mut rig, fake, mut rx) = rig_with_item(Some(&repo));
    rig.lp(0).items.get_mut("a").unwrap().branch = Some("feature".into());
    // Nothing is playing: Restart is Play (from the play checkout, the item holding no lane).
    restart(&rig, "a", "c1");
    assert_eq!(answered(&mut rig, &mut rx).await, json!({ "type": "ack", "id": "c1" }));
    assert_eq!(fake.games.lock().unwrap().len(), 1);
    // Playing: stopped, then played again.
    restart(&rig, "a", "c2");
    assert_eq!(answered(&mut rig, &mut rx).await, json!({ "type": "ack", "id": "c2" }));
    let games = fake.games.lock().unwrap().clone();
    assert_eq!(games.len(), 2);
    assert!(games[0].3.load(Ordering::SeqCst), "the first game was closed");
    assert!(!games[1].3.load(Ordering::SeqCst), "the second runs");
    assert_eq!(playing_item(&mut rig).as_deref(), Some("a"));
    // In its lane: the same, and the lane is held again for the new game.
    stop(&rig, "a", "c3");
    assert_eq!(answered(&mut rig, &mut rx).await["type"], "ack");
    rig.lp(0).last_lane.insert("a".into(), 1);
    play(&rig, "a", "c4");
    assert_eq!(answered(&mut rig, &mut rx).await["type"], "ack");
    restart(&rig, "a", "c5");
    assert_eq!(answered(&mut rig, &mut rx).await, json!({ "type": "ack", "id": "c5" }));
    let games = fake.games.lock().unwrap().clone();
    assert_eq!(games.len(), 4);
    assert_eq!((games[2].2, games[3].2), (1, 1));
    assert!(games[2].3.load(Ordering::SeqCst) && !games[3].3.load(Ordering::SeqCst));
    assert!(matches!(rig.machine.pool.holder(1), Some(Holder::Play { item, .. }) if item == "a"));
}

#[tokio::test]
async fn a_restart_waits_for_a_game_that_is_slow_to_close_and_a_stop_cancels_it_and_a_pause_only_stops() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, _) = repo_with_feature(tmp.path());
    let (mut rig, fake, mut rx) = rig_with_item(Some(&repo));
    rig.lp(0).items.get_mut("a").unwrap().branch = Some("feature".into());
    fake.stubborn_games.store(true, Ordering::SeqCst);
    play(&rig, "a", "c1");
    assert_eq!(answered(&mut rig, &mut rx).await["type"], "ack");
    let first_closed = fake.games.lock().unwrap()[0].3.clone();
    // The game ignores the stop (it is stubborn), so the restart stays pending and starts nothing.
    restart(&rig, "a", "c2");
    for _ in 0..3 {
        rig.machine.poll().await;
    }
    assert!(rx.try_recv().is_err(), "not answered while the game is open");
    assert_eq!(fake.games.lock().unwrap().len(), 1);
    // A Stop of the item cancels it, in words.
    stop(&rig, "a", "c3");
    let first = answered(&mut rig, &mut rx).await;
    let second = answered(&mut rig, &mut rx).await;
    let answers = [first, second];
    assert!(answers.contains(&nack("c2", "It was stopped, so it wasn't started again.")));
    assert!(answers.contains(&json!({ "type": "ack", "id": "c3" })));
    first_closed.store(true, Ordering::SeqCst);
    rig.machine.poll().await;
    assert_eq!(
        fake.games.lock().unwrap().len(),
        1,
        "the cancelled restart played nothing"
    );
    // Paused, a restart of a running game only stops it.
    fake.stubborn_games.store(false, Ordering::SeqCst);
    play(&rig, "a", "c4");
    assert_eq!(answered(&mut rig, &mut rx).await["type"], "ack");
    rig.pause("p0", true);
    restart(&rig, "a", "c5");
    assert_eq!(
        answered(&mut rig, &mut rx).await,
        nack("c5", "This computer is paused, so it didn't start a build.")
    );
    assert!(fake.games.lock().unwrap()[1].3.load(Ordering::SeqCst));
    assert_eq!(fake.games.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn play_started_is_noted_on_an_item_this_computer_holds_not_on_a_visited_one() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, first) = repo_with_feature(tmp.path());
    let base = store(json!([row("v", Some(("feature", &first)))])).await;
    let (mut rig, _fake, mut rx) = rig_on(&base, Some(&repo));
    rig.lp(0).items.get_mut("a").unwrap().branch = Some("feature".into());
    play(&rig, "a", "c1");
    assert_eq!(answered(&mut rig, &mut rx).await["type"], "ack");
    play(&rig, "v", "c2");
    assert_eq!(answered(&mut rig, &mut rx).await["type"], "ack");
    rig.lp(0).last_lane.insert("a".into(), 1);
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::AwaitingPlaytest;
    stop(&rig, "a", "c3");
    assert_eq!(answered(&mut rig, &mut rx).await["type"], "ack");
    play(&rig, "a", "c4");
    assert_eq!(answered(&mut rig, &mut rx).await["type"], "ack");
    assert_eq!(notes(&mut rig, "a"), ["Play started", "Play started in lane 1"]);
    assert!(notes(&mut rig, "v").is_empty());
}
