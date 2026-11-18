//! **Play this build** and its **Stop** (`driver/build_play.rs`): an item's lane when it has a free one, else the play checkout.
use std::path::Path;
use std::process::Command;

use super::*;
use crate::view::ItemCommand;

pub(super) fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A live rig whose item "a" waits for a try-out, holding no lane (it last used lane 1), on branch "feature"; the project has a play
/// command, and, with a `repo`, a real checkout whose origin has that branch.
pub(super) fn rig_with_item(repo: Option<&Path>) -> (Rig, Arc<Fake>, tokio::sync::mpsc::UnboundedReceiver<Value>) {
    rig_on("http://127.0.0.1:1", repo)
}

/// [`rig_with_item`] with the store at `base`.
pub(super) fn rig_on(base: &str, repo: Option<&Path>) -> (Rig, Arc<Fake>, tokio::sync::mpsc::UnboundedReceiver<Value>) {
    let mut rig = rig(2, &[base]);
    let fake = rig.go_live();
    rig.queue(0, &["a"]);
    let lp = rig.lp(0);
    lp.runs.get_mut("a").unwrap().state = ItemState::AwaitingPlaytest;
    lp.items.get_mut("a").unwrap().branch = Some("feature".into());
    for project in [&mut lp.project, &mut lp.file_project] {
        project.play_command = "the-game".into();
        project.worktrees_dir = "wt".into();
        if let Some(repo) = repo {
            project.repo_path = repo.to_string_lossy().into_owned();
            project.worktrees_dir = repo.parent().unwrap().join("wt").to_string_lossy().into_owned();
        }
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    rig.machine.links.item_commands.set_out("p0", Some(tx));
    (rig, fake, rx)
}

fn set_play_command(rig: &mut Rig, command: &str) {
    let lp = rig.lp(0);
    for project in [&mut lp.project, &mut lp.file_project] {
        project.play_command = command.into();
    }
}

pub(super) fn play(rig: &Rig, item: &str, id: &str) {
    rig.machine.links.item_commands.push(ItemCommand::PlayItem {
        project: "p0".into(),
        item: item.into(),
        command_id: id.into(),
    });
}

pub(super) fn stop(rig: &Rig, item: &str, id: &str) {
    rig.machine.links.item_commands.push(ItemCommand::StopItem {
        project: "p0".into(),
        item: item.into(),
        command_id: id.into(),
    });
}

/// Polls until the command is answered: the checkout is made on its own thread, so the answer comes on a later poll.
pub(super) async fn answered(rig: &mut Rig, rx: &mut tokio::sync::mpsc::UnboundedReceiver<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            rig.machine.poll().await;
            if let Ok(answer) = rx.try_recv() {
                return answer;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the command was answered")
}

pub(super) fn nack(id: &str, why: &str) -> Value {
    json!({ "type": "nack", "id": id, "reason": why })
}

#[tokio::test]
async fn an_item_with_a_free_lane_it_last_used_plays_there() {
    let (mut rig, fake, mut rx) = rig_with_item(None);
    rig.lp(0).last_lane.insert("a".into(), 1);
    play(&rig, "a", "c1");
    assert_eq!(answered(&mut rig, &mut rx).await, json!({ "type": "ack", "id": "c1" }));
    let (command, lane) = {
        let games = fake.games.lock().unwrap();
        (games[0].1.clone(), games[0].2)
    };
    assert_eq!((command.as_str(), lane), ("the-game", 1));
    assert!(matches!(rig.machine.pool.holder(1), Some(Holder::Play { item, .. }) if item == "a"));
    // Stop by item ends that game and gives the lane back.
    stop(&rig, "a", "c2");
    assert_eq!(answered(&mut rig, &mut rx).await, json!({ "type": "ack", "id": "c2" }));
    assert!(rig.machine.pool.holder(1).is_none());
}

#[tokio::test]
async fn it_is_refused_in_ts_words_when_the_item_is_building_has_no_branch_or_a_step_runs() {
    let (mut rig, fake, mut rx) = rig_with_item(None);
    let building = Holder::Item {
        project: "p0".into(),
        item: "a".into(),
    };
    assert!(rig.machine.pool.take(2, building));
    play(&rig, "a", "c1");
    assert_eq!(
        answered(&mut rig, &mut rx).await,
        nack("c1", "Something is building in lane 2; try again when it's idle.")
    );
    rig.machine.pool.release(
        2,
        &Holder::Item {
            project: "p0".into(),
            item: "a".into(),
        },
    );
    play(&rig, "nope", "c2");
    // An item this computer doesn't hold is another's slice: with the store out of reach nothing is played (fail closed).
    let refused = answered(&mut rig, &mut rx).await;
    assert!(
        refused["reason"]
            .as_str()
            .unwrap()
            .starts_with("Couldn't read the shared backlog's try-out queue: "),
        "{refused}"
    );
    rig.lp(0).items.get_mut("a").unwrap().branch = None;
    play(&rig, "a", "c3");
    assert_eq!(
        answered(&mut rig, &mut rx).await,
        nack("c3", "Item a has no build to play yet.")
    );
    rig.lp(0).items.get_mut("a").unwrap().branch = Some("feature".into());
    rig.lp(0)
        .running
        .spawn("a".into(), ItemState::Checking, Box::pin(std::future::pending()));
    play(&rig, "a", "c4");
    assert_eq!(
        answered(&mut rig, &mut rx).await,
        nack("c4", "A step is running for this item; try again in a moment.")
    );
    set_play_command(&mut rig, " ");
    play(&rig, "a", "c5");
    assert_eq!(
        answered(&mut rig, &mut rx).await,
        nack(
            "c5",
            "No play command set. Add one in Settings → Project settings, under How to try it."
        )
    );
    assert!(fake.games.lock().unwrap().is_empty());
    // A paused computer starts nothing (a Stop still works).
    set_play_command(&mut rig, "the-game");
    rig.pause("p0", true);
    play(&rig, "a", "c6");
    assert_eq!(
        answered(&mut rig, &mut rx).await,
        nack("c6", "This computer is paused, so it didn't start a build.")
    );
    stop(&rig, "a", "c7");
    assert_eq!(answered(&mut rig, &mut rx).await, json!({ "type": "ack", "id": "c7" }));
}

#[tokio::test]
async fn a_build_in_no_lane_plays_from_the_play_checkout_on_its_branchs_tip() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repo_with_origin(tmp.path());
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("feature.txt"), "the build").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "the slice"]);
    git(&repo, &["push", "-q", "origin", "feature"]);
    git(&repo, &["checkout", "-q", "main"]);
    let (mut rig, fake, mut rx) = rig_with_item(Some(&repo));
    play(&rig, "a", "c1");
    assert_eq!(answered(&mut rig, &mut rx).await, json!({ "type": "ack", "id": "c1" }));
    let (dir, command, lane, closed) = fake.games.lock().unwrap()[0].clone();
    assert_eq!(
        (command.as_str(), lane),
        ("the-game", 0),
        "lane 0 is the play checkout's: RETASKABLE_LANE=play"
    );
    assert_eq!(dir, crate::lanes::play_path(&repo.parent().unwrap().join("wt")));
    assert_eq!(std::fs::read_to_string(dir.join("feature.txt")).unwrap(), "the build");
    assert!(
        rig.machine.pool.held_lanes().next().is_none(),
        "no work lane is held for it"
    );
    // Playing the item again while it plays changes nothing.
    play(&rig, "a", "c2");
    assert_eq!(answered(&mut rig, &mut rx).await, json!({ "type": "ack", "id": "c2" }));
    assert_eq!(fake.games.lock().unwrap().len(), 1);
    // Stop closes it.
    stop(&rig, "a", "c3");
    assert_eq!(answered(&mut rig, &mut rx).await, json!({ "type": "ack", "id": "c3" }));
    assert!(closed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn another_items_build_is_stopped_first_and_a_checkout_that_fails_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repo_with_origin(tmp.path());
    for branch in ["one", "two"] {
        git(&repo, &["checkout", "-q", "-b", branch]);
        std::fs::write(repo.join(format!("{branch}.txt")), branch).unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", branch]);
        git(&repo, &["push", "-q", "origin", branch]);
        git(&repo, &["checkout", "-q", "main"]);
    }
    let (mut rig, fake, mut rx) = rig_with_item(Some(&repo));
    rig.queue(0, &["b"]);
    for (item, branch) in [("a", "one"), ("b", "two")] {
        let lp = rig.lp(0);
        lp.runs.get_mut(item).unwrap().state = ItemState::AwaitingPlaytest;
        lp.items.get_mut(item).unwrap().branch = Some(branch.into());
    }
    play(&rig, "a", "c1");
    assert_eq!(answered(&mut rig, &mut rx).await["type"], "ack");
    play(&rig, "b", "c2");
    assert_eq!(answered(&mut rig, &mut rx).await, json!({ "type": "ack", "id": "c2" }));
    let games = fake.games.lock().unwrap().clone();
    assert_eq!(games.len(), 2);
    assert!(games[0].3.load(Ordering::SeqCst), "the first build was closed");
    assert_eq!(std::fs::read_to_string(games[1].0.join("two.txt")).unwrap(), "two");
    // A branch origin has none of can't be checked out, and the answer says which.
    rig.lp(0).items.get_mut("a").unwrap().branch = Some("gone".into());
    stop(&rig, "b", "c3");
    assert_eq!(answered(&mut rig, &mut rx).await["type"], "ack");
    play(&rig, "a", "c4");
    let failed = answered(&mut rig, &mut rx).await;
    assert!(
        failed["reason"]
            .as_str()
            .unwrap()
            .starts_with("Couldn't check out gone: "),
        "{failed}"
    );
}

#[tokio::test]
async fn a_stubborn_build_that_does_not_close_in_time_refuses_the_next() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repo_with_origin(tmp.path());
    for branch in ["one", "two"] {
        git(&repo, &["checkout", "-q", "-b", branch]);
        git(&repo, &["push", "-q", "origin", branch]);
        git(&repo, &["checkout", "-q", "main"]);
    }
    let (mut rig, fake, mut rx) = rig_with_item(Some(&repo));
    fake.stubborn_games.store(true, Ordering::SeqCst);
    rig.queue(0, &["b"]);
    for (item, branch) in [("a", "one"), ("b", "two")] {
        let lp = rig.lp(0);
        lp.runs.get_mut(item).unwrap().state = ItemState::AwaitingPlaytest;
        lp.items.get_mut(item).unwrap().branch = Some(branch.into());
    }
    play(&rig, "a", "c1");
    assert_eq!(answered(&mut rig, &mut rx).await["type"], "ack");
    play(&rig, "b", "c2");
    rig.machine.poll().await;
    // While it waits for the first to close, a third press is refused.
    play(&rig, "b", "c3");
    assert_eq!(
        answered(&mut rig, &mut rx).await,
        nack("c3", "A build is being checked out to play; try again in a moment.")
    );
}
