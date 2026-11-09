//! Play and Stop in an item's lane (`driver/lane_play.rs`).
use super::*;
use crate::lane_pool::Want;
use crate::view::ItemCommand;

/// A live rig whose item "a" waits on CI, holding no lane, last having used lane 1.
fn idle(lanes: u32) -> (Rig, Arc<Fake>) {
    let mut rig = rig(lanes, &["http://127.0.0.1:1"]);
    let fake = rig.go_live();
    rig.queue(0, &["a"]);
    let lp = rig.lp(0);
    lp.runs.get_mut("a").unwrap().state = ItemState::AwaitingCi;
    lp.last_lane.insert("a".into(), 1);
    lp.project.play_command = "the-game".into();
    lp.project.worktrees_dir = "wt".into();
    (rig, fake)
}

fn send(rig: &Rig, command: fn(String, u32) -> ItemCommand, lane: u32) {
    rig.machine.links.item_commands.push(command("p0".into(), lane));
}

fn play(project: String, lane: u32) -> ItemCommand {
    ItemCommand::Play { project, lane }
}

fn stop(project: String, lane: u32) -> ItemCommand {
    ItemCommand::Stop { project, lane }
}

fn restart(project: String, lane: u32) -> ItemCommand {
    ItemCommand::RestartPlay { project, lane }
}

fn want() -> Want {
    Want {
        project: "p0".into(),
        item: "a".into(),
        chore: false,
        last_lane: Some(1),
        fix_queued: false,
    }
}

#[tokio::test]
async fn play_runs_the_command_in_the_lane_and_holds_it_until_stop_frees_it() {
    let (mut rig, fake) = idle(1);
    send(&rig, play, 1);
    rig.machine.poll().await;
    {
        let games = fake.games.lock().unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].0, crate::lanes::lane_path(std::path::Path::new("wt"), 1));
        assert_eq!((games[0].1.as_str(), games[0].2), ("the-game", 1));
    }
    assert!(matches!(rig.machine.pool.holder(1), Some(Holder::Play { item, .. }) if item == "a"));
    assert_eq!(
        rig.machine.pool.claim(&want()),
        None,
        "the item's next step waits for the game"
    );

    send(&rig, stop, 1);
    rig.machine.poll().await;
    assert!(rig.machine.pool.holder(1).is_none());
    assert_eq!(rig.machine.pool.claim(&want()), Some(1), "the item carries on");
}

#[tokio::test]
async fn a_game_that_closes_by_itself_gives_the_lane_back() {
    let (mut rig, fake) = idle(1);
    send(&rig, play, 1);
    rig.machine.poll().await;
    fake.games.lock().unwrap()[0].3.store(true, Ordering::SeqCst);
    rig.machine.poll().await;
    assert!(rig.machine.pool.holder(1).is_none());
}

#[tokio::test]
async fn play_is_refused_on_a_busy_an_empty_or_a_missing_lane_and_while_one_game_runs() {
    let (mut rig, fake) = idle(2);
    let building = Holder::Item {
        project: "p0".into(),
        item: "a".into(),
    };
    assert!(rig.machine.pool.take(1, building));
    send(&rig, play, 1); // a session is building in it
    send(&rig, play, 2); // nothing last used it
    send(&rig, play, 3); // past `lanes`
    rig.machine.poll().await;
    assert!(fake.games.lock().unwrap().is_empty());
    assert!(rig.machine.pool.holder(2).is_none());

    rig.machine.pool.release(
        1,
        &Holder::Item {
            project: "p0".into(),
            item: "a".into(),
        },
    );
    send(&rig, play, 1);
    send(&rig, play, 1);
    rig.machine.poll().await;
    assert_eq!(
        fake.games.lock().unwrap().len(),
        1,
        "the second play of the lane is refused"
    );
}

#[tokio::test]
async fn play_is_refused_while_the_items_step_is_running() {
    let (mut rig, fake) = idle(1);
    fake.hold.store(true, Ordering::SeqCst);
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::Merging;
    *fake.to.lock().unwrap() = None;
    rig.machine.poll().await;
    assert!(rig.lp(0).running.busy("a"), "a step is running for the item");
    send(&rig, play, 1);
    rig.machine.poll().await;
    assert!(fake.games.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stop_works_while_paused_and_a_play_queued_while_paused_starts_nothing() {
    let (mut rig, fake) = idle(1);
    send(&rig, play, 1);
    rig.machine.poll().await;
    assert!(rig.machine.pool.holder(1).is_some());

    rig.pause("p0", true);
    send(&rig, stop, 1);
    rig.machine.poll().await;
    assert!(rig.machine.pool.holder(1).is_none(), "the lane is freed while paused");

    send(&rig, play, 1);
    rig.machine.poll().await;
    assert!(rig.machine.pool.holder(1).is_none());
    assert_eq!(fake.games.lock().unwrap().len(), 1, "no second game");
}

#[tokio::test]
async fn restart_stops_the_game_holds_the_lane_and_plays_again_once_it_has_closed() {
    let (mut rig, fake) = idle(1);
    fake.stubborn_games.store(true, Ordering::SeqCst);
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::AwaitingPlaytest;
    send(&rig, play, 1);
    rig.machine.poll().await;
    send(&rig, restart, 1);
    rig.machine.poll().await;
    assert_eq!(fake.games.lock().unwrap().len(), 1);
    assert!(
        rig.machine.pool.holder(1).is_some(),
        "the lane stays held while the game closes"
    );
    rig.machine.poll().await;
    assert_eq!(fake.games.lock().unwrap().len(), 1, "still waiting");

    fake.games.lock().unwrap()[0].3.store(true, Ordering::SeqCst);
    rig.machine.poll().await;
    assert_eq!(fake.games.lock().unwrap().len(), 2, "the second game started");
    assert!(matches!(rig.machine.pool.holder(1), Some(Holder::Play { .. })));
    rig.machine.poll().await;
    assert_eq!(fake.games.lock().unwrap().len(), 2, "only one restart");
}

#[tokio::test]
async fn restart_of_an_idle_lane_plays_and_of_an_empty_or_busy_lane_starts_nothing() {
    let (mut rig, fake) = idle(2);
    send(&rig, restart, 2); // nothing last used it
    rig.machine.poll().await;
    assert!(fake.games.lock().unwrap().is_empty());
    send(&rig, restart, 1);
    rig.machine.poll().await;
    assert_eq!(fake.games.lock().unwrap().len(), 1);

    let (mut rig, fake) = idle(1);
    let building = Holder::Item {
        project: "p0".into(),
        item: "a".into(),
    };
    assert!(rig.machine.pool.take(1, building));
    send(&rig, restart, 1);
    rig.machine.poll().await;
    assert!(fake.games.lock().unwrap().is_empty());
}

#[tokio::test]
async fn restart_while_paused_stops_the_game_and_starts_no_new_one() {
    let (mut rig, fake) = idle(1);
    send(&rig, play, 1);
    rig.machine.poll().await;
    rig.pause("p0", true);
    send(&rig, restart, 1);
    rig.machine.poll().await;
    rig.machine.poll().await;
    assert!(rig.machine.pool.holder(1).is_none());
    assert_eq!(fake.games.lock().unwrap().len(), 1);
    rig.pause("p0", false);
    rig.machine.poll().await;
    assert_eq!(
        fake.games.lock().unwrap().len(),
        1,
        "the paused restart isn't remembered"
    );
}

#[tokio::test]
async fn a_restart_whose_game_never_closes_is_dropped_after_the_limit() {
    let (mut rig, fake) = idle(1);
    fake.stubborn_games.store(true, Ordering::SeqCst);
    send(&rig, play, 1);
    rig.machine.poll().await;
    send(&rig, restart, 1);
    rig.machine.poll().await;
    let old = std::time::Instant::now() - crate::play::STOP_LIMIT - std::time::Duration::from_secs(1);
    rig.lp(0).restarting.insert(1, old);
    rig.machine.poll().await;
    assert!(rig.lp(0).restarting.is_empty());
    fake.games.lock().unwrap()[0].3.store(true, Ordering::SeqCst);
    rig.machine.poll().await;
    assert_eq!(fake.games.lock().unwrap().len(), 1, "no second game");
}
