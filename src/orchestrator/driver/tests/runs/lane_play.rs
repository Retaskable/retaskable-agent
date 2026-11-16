//! Play, Stop and Restart of a lane in `runs` mode (`driver/lane_play.rs`): the game runs in the lane a piece's run last
//! took, never while a run holds the lane, and none of it needs the store: a Stop in an outage touches no run and
//! loses no finished result.
use super::*;
use crate::view::ItemCommand;

/// A runner of builds whose games are the fake's.
struct Games(FakeRuns, Arc<Fake>);

impl RunStarter for Games {
    fn kinds(&self) -> Vec<RunKind> {
        self.0.kinds()
    }
    fn start(&self, request: RunRequest) -> Result<(), String> {
        self.0.start(request)
    }
    fn play_lane(&self, dir: &std::path::Path, command: &str, lane: u32) -> Result<Box<dyn LanePlay>, String> {
        self.1.play_lane(dir, command, lane)
    }
}

/// A rig with one lane, `playCommand` set, and a build run "a" that took the lane and finished.
async fn after_a_run(lanes: u32) -> (Rig, Shared_, Games, Instant) {
    let (mut rig, script) = setup(lanes).await;
    let lp = rig.lp(0);
    lp.project.play_command = "the-game".into();
    lp.project.worktrees_dir = "wt".into();
    lp.mode = OrchestratorMode::Runs;
    let starter = Games(FakeRuns::new(&[RunKind::Build]), Arc::new(Fake::default()));
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("a", "build")]);
    pass(&mut rig, &starter, t0).await;
    assert_eq!(starter.0.started(), ["a"]);
    starter.0.finish("a", RunOutcome::PrOpened);
    pass(&mut rig, &starter, t0).await;
    assert!(rig.machine.pool.holder(1).is_none(), "the run gave the lane back");
    (rig, script, starter, t0)
}

fn send(rig: &Rig, command: ItemCommand) {
    rig.machine.links.item_commands.push(command);
}

fn play(lane: u32) -> ItemCommand {
    ItemCommand::Play {
        project: "p0".into(),
        lane,
    }
}

fn stop(lane: u32) -> ItemCommand {
    ItemCommand::Stop {
        project: "p0".into(),
        lane,
    }
}

fn restart(lane: u32) -> ItemCommand {
    ItemCommand::RestartPlay {
        project: "p0".into(),
        lane,
    }
}

fn games(starter: &Games) -> usize {
    starter.1.games.lock().unwrap().len()
}

fn close(starter: &Games, n: usize) {
    starter.1.games.lock().unwrap()[n].3.store(true, Ordering::SeqCst);
}

/// The lane the live run on `piece` holds.
fn lane_of(rig: &mut Rig, piece: &str) -> u32 {
    let runs = rig.lp(0).run_loop.runs();
    runs.live()
        .iter()
        .find(|r| r.piece == piece)
        .and_then(|r| r.lane)
        .unwrap()
}

fn playing(rig: &Rig) -> Vec<(u32, bool)> {
    let live = rig.machine.projects[0].live.snapshot();
    live.projects["p0"].held.iter().map(|h| (h.lane, h.playing)).collect()
}

#[tokio::test]
async fn play_runs_the_command_in_the_lane_the_piece_last_took_and_stop_gives_it_back() {
    let (mut rig, _script, starter, t0) = after_a_run(1).await;
    send(&rig, play(1));
    pass(&mut rig, &starter, t0).await;
    {
        let games = starter.1.games.lock().unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].0, crate::lanes::lane_path(std::path::Path::new("wt"), 1));
        assert_eq!((games[0].1.as_str(), games[0].2), ("the-game", 1));
    }
    assert!(matches!(rig.machine.pool.holder(1), Some(Holder::Play { item, .. }) if item == "piece-a"));
    assert_eq!(playing(&rig), [(1, true)], "the snapshot's lane shows it playing");

    send(&rig, stop(1));
    pass(&mut rig, &starter, t0).await;
    assert!(rig.machine.pool.holder(1).is_none());
    assert!(playing(&rig).is_empty());
    assert_eq!(
        rig.machine.pool.claim_run("p0", "piece-b"),
        Some(1),
        "a run may take the lane again"
    );
}

#[tokio::test]
async fn a_game_closing_by_itself_gives_the_lane_back() {
    let (mut rig, _script, starter, t0) = after_a_run(1).await;
    send(&rig, play(1));
    pass(&mut rig, &starter, t0).await;
    close(&starter, 0);
    pass(&mut rig, &starter, t0).await;
    assert!(rig.machine.pool.holder(1).is_none());
}

#[tokio::test]
async fn play_is_refused_in_a_lane_no_run_took_one_a_run_holds_or_past_lanes_and_never_twice() {
    let (mut rig, script, starter, t0) = after_a_run(2).await;
    send(&rig, play(2)); // no run took it
    send(&rig, play(3)); // past `lanes`
    pass(&mut rig, &starter, t0).await;
    assert_eq!(games(&starter), 0);

    // A run is in lane 1 now.
    script.lock().unwrap().takes = VecDeque::from([run("b", "build")]);
    pass(&mut rig, &starter, t0).await;
    assert_eq!(starter.0.started(), ["a", "b"]);
    let lane = lane_of(&mut rig, "piece-b");
    send(&rig, play(lane));
    pass(&mut rig, &starter, t0).await;
    assert_eq!(games(&starter), 0, "a run holds the lane");
    starter.0.finish("b", RunOutcome::PrOpened);
    pass(&mut rig, &starter, t0).await;

    send(&rig, play(lane));
    send(&rig, play(lane));
    pass(&mut rig, &starter, t0).await;
    assert_eq!(games(&starter), 1, "the second play of the lane is refused");
}

#[tokio::test]
async fn a_lane_plays_the_piece_that_last_took_it() {
    let (mut rig, script, starter, t0) = after_a_run(1).await;
    script.lock().unwrap().takes = VecDeque::from([run("b", "build")]);
    pass(&mut rig, &starter, t0).await;
    starter.0.finish("b", RunOutcome::PrOpened);
    pass(&mut rig, &starter, t0).await;
    send(&rig, play(1));
    pass(&mut rig, &starter, t0).await;
    assert!(matches!(rig.machine.pool.holder(1), Some(Holder::Play { item, .. }) if item == "piece-b"));
}

#[tokio::test]
async fn stop_of_a_lane_with_a_run_in_it_touches_nothing() {
    let (mut rig, script, starter, t0) = after_a_run(1).await;
    script.lock().unwrap().takes = VecDeque::from([run("b", "build")]);
    pass(&mut rig, &starter, t0).await;
    let ended = script.lock().unwrap().args("run_done").len();
    send(&rig, stop(1));
    send(&rig, restart(1));
    pass(&mut rig, &starter, t0).await;
    assert!(matches!(rig.machine.pool.holder(1), Some(Holder::Run { .. })));
    assert_eq!(rig.lp(0).run_loop.runs().live().len(), 1, "the run goes on");
    assert_eq!(
        script.lock().unwrap().args("run_done").len(),
        ended,
        "and nothing was ended"
    );
    assert_eq!(games(&starter), 0);
}

#[tokio::test]
async fn stop_works_while_paused_and_a_play_queued_while_paused_starts_nothing() {
    let (mut rig, _script, starter, t0) = after_a_run(1).await;
    send(&rig, play(1));
    pass(&mut rig, &starter, t0).await;
    assert!(rig.machine.pool.holder(1).is_some());

    let m = &mut rig.machine;
    let shared = Shared {
        links: &m.links,
        local: &m.local,
        claim_paused_until: &mut None,
        claude_signed_out_until: &mut None,
        engine_limits: &mut Default::default(),
        stopping: true,
    };
    send_to(&shared, stop(1));
    send_to(&shared, play(1));
    m.projects[0].run_pass(&mut m.pool, &starter, &shared, t0).await;
    assert!(m.pool.holder(1).is_none(), "the game was stopped and the play dropped");
    assert_eq!(games(&starter), 1, "no second game");
}

fn send_to(shared: &Shared<'_>, command: ItemCommand) {
    shared.links.item_commands.push(command);
}

#[tokio::test]
async fn restart_stops_the_game_holds_the_lane_and_plays_again_once_it_has_closed() {
    let (mut rig, _script, starter, t0) = after_a_run(1).await;
    starter.1.stubborn_games.store(true, Ordering::SeqCst);
    send(&rig, restart(1)); // nothing playing: it just starts
    pass(&mut rig, &starter, t0).await;
    assert_eq!(games(&starter), 1);

    send(&rig, restart(1));
    pass(&mut rig, &starter, t0).await;
    assert!(rig.machine.pool.holder(1).is_some(), "held while the game closes");
    assert_eq!(games(&starter), 1);
    close(&starter, 0);
    pass(&mut rig, &starter, t0).await;
    assert_eq!(games(&starter), 2, "the second game started");
    pass(&mut rig, &starter, t0).await;
    assert_eq!(games(&starter), 2, "only one restart");
}

#[tokio::test]
async fn a_stop_after_a_restart_cancels_it_and_a_game_that_never_closes_drops_it() {
    let (mut rig, _script, starter, t0) = after_a_run(1).await;
    starter.1.stubborn_games.store(true, Ordering::SeqCst);
    send(&rig, play(1));
    send(&rig, restart(1));
    send(&rig, stop(1));
    pass(&mut rig, &starter, t0).await;
    close(&starter, 0);
    pass(&mut rig, &starter, t0).await;
    pass(&mut rig, &starter, t0).await;
    assert_eq!(games(&starter), 1, "no second game");

    send(&rig, play(1));
    pass(&mut rig, &starter, t0).await;
    send(&rig, restart(1));
    pass(&mut rig, &starter, t0).await;
    let old = std::time::Instant::now() - crate::play::STOP_LIMIT - Duration::from_secs(1);
    rig.lp(0).restarting.insert(1, old);
    pass(&mut rig, &starter, t0).await;
    assert!(rig.lp(0).restarting.is_empty());
    close(&starter, 1);
    pass(&mut rig, &starter, t0).await;
    assert_eq!(games(&starter), 2, "the dropped restart starts nothing");
}

#[tokio::test]
async fn lane_play_needs_no_store_and_a_stop_in_an_outage_keeps_a_finished_result() {
    let (mut rig, script, starter, t0) = after_a_run(2).await;
    let la = (1..=2)
        .find(|k| rig.machine.pool.last_run_piece("p0", *k) == Some("piece-a"))
        .unwrap();
    send(&rig, play(la));
    pass(&mut rig, &starter, t0).await;
    assert_eq!(games(&starter), 1);
    script.lock().unwrap().takes = VecDeque::from([run("b", "build")]);
    pass(&mut rig, &starter, t0).await;
    assert_ne!(lane_of(&mut rig, "piece-b"), la, "the game holds its lane");

    // The store goes away while run "b" finishes: its end is owed.
    script.lock().unwrap().down = true;
    starter.0.finish("b", RunOutcome::PrOpened);
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    assert_eq!(rig.lp(0).run_loop.runs().owed().len(), 1, "the result is kept");

    send(&rig, stop(la));
    pass(&mut rig, &starter, t0 + Duration::from_secs(2)).await;
    assert!(rig.machine.pool.holder(la).is_none(), "the stop worked");
    assert_eq!(rig.lp(0).run_loop.runs().owed().len(), 1, "and lost nothing");
    send(&rig, play(la));
    pass(&mut rig, &starter, t0 + Duration::from_secs(3)).await;
    assert_eq!(games(&starter), 2, "a play needs no store");

    script.lock().unwrap().down = false;
    pass(&mut rig, &starter, t0 + Duration::from_secs(70)).await;
    let done = script.lock().unwrap().args("run_done");
    assert_eq!(done.last().unwrap()["token"], "b");
    assert!(
        rig.lp(0).run_loop.runs().owed().is_empty(),
        "sent once the store is back"
    );
}
