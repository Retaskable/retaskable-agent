//! The run loop (`runs.rs`) against a fake store and a fake starter.
use super::*;
use crate::backlog::runs::{RunKind, RunOutcome};
use axum::extract::State;
use axum::http::StatusCode;
use std::collections::VecDeque;
use std::time::Instant;

/// What the fake store does: each op's body is logged; replies come from here.
#[derive(Default)]
struct Script {
    log: Vec<Value>,
    drop_fails: bool,
    takes: VecDeque<Value>,
    /// What `merge_next` answers, in turn (`null` once spent).
    merges: VecDeque<Value>,
    /// `merge_beat` says the turn is no longer held.
    turn_lost: bool,
    /// What `merge_open` answers (`null`: no turn held), and whether it fails.
    open: Value,
    open_fails: bool,
    /// `merge_result` fails while this is set (the store can't be reached).
    result_fails: bool,
    beat: Value,
    /// A refusal code for `run_done`.
    done_refused: Option<&'static str>,
}

type Shared_ = Arc<Mutex<Script>>;

impl Script {
    fn ops(&self) -> Vec<String> {
        self.log.iter().map(|b| b["op"].as_str().unwrap().to_string()).collect()
    }
    fn args(&self, op: &str) -> Vec<Value> {
        self.log
            .iter()
            .filter(|b| b["op"] == op)
            .map(|b| b["args"].clone())
            .collect()
    }
}

async fn store() -> (String, Shared_) {
    let script: Shared_ = Arc::default();
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(|State(s): State<Shared_>, Json(body): Json<Value>| async move {
                let mut s = s.lock().unwrap();
                s.log.push(body.clone());
                let ok = |v: Value| (StatusCode::OK, Json(json!({ "value": v })));
                let refuse = |status, code| (status, Json(json!({ "error": code, "message": code })));
                match body["op"].as_str().unwrap() {
                    "runs_drop" if s.drop_fails => refuse(StatusCode::INTERNAL_SERVER_ERROR, "boom"),
                    "run_take" => ok(s.takes.pop_front().unwrap_or(Value::Null)),
                    "run_beat" => ok(s.beat.clone()),
                    "merge_next" => ok(s.merges.pop_front().unwrap_or(Value::Null)),
                    "merge_open" if s.open_fails => refuse(StatusCode::INTERNAL_SERVER_ERROR, "boom"),
                    "merge_open" => ok(json!({ "turn": s.open.clone() })),
                    "merge_result" if s.result_fails => refuse(StatusCode::INTERNAL_SERVER_ERROR, "boom"),
                    "merge_beat" => ok(json!({ "held": !s.turn_lost })),
                    "merge_result" => ok(json!({ "held": true })),
                    "lock_take" => ok(json!({ "held": true, "token": "L1" })),
                    "run_done" if s.done_refused.is_some() => {
                        let code = s.done_refused.unwrap();
                        refuse(StatusCode::CONFLICT, code)
                    }
                    _ => ok(Value::Null),
                }
            }),
        )
        .with_state(script.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), script)
}

fn run(token: &str, kind: &str) -> Value {
    json!({ "token": token, "kind": kind, "piece": { "id": format!("piece-{token}"), "title": token } })
}

struct FakeRuns {
    kinds: Vec<RunKind>,
    started: Mutex<Vec<RunRequest>>,
}

impl FakeRuns {
    fn new(kinds: &[RunKind]) -> Self {
        Self {
            kinds: kinds.to_vec(),
            started: Mutex::default(),
        }
    }
    fn started(&self) -> Vec<String> {
        self.started.lock().unwrap().iter().map(|r| r.token.clone()).collect()
    }
    /// The run `token` finishes with `outcome`.
    fn finish(&self, token: &str, outcome: RunOutcome) {
        let started = self.started.lock().unwrap();
        let request = started.iter().find(|r| r.token == token).unwrap();
        request.ends.send(RunEnd {
            token: token.into(),
            outcome,
            result: json!({}),
        });
    }
}

impl RunStarter for FakeRuns {
    fn kinds(&self) -> Vec<RunKind> {
        self.kinds.clone()
    }
    fn start(&self, request: RunRequest) -> Result<(), String> {
        self.started.lock().unwrap().push(request);
        Ok(())
    }
}

async fn pass(rig: &mut Rig, starter: &dyn RunStarter, now: Instant) {
    let m = &mut rig.machine;
    let shared = Shared {
        links: &m.links,
        local: &m.local,
        claim_paused_until: &mut None,
        claude_signed_out_until: &mut None,
        engine_limits: &mut Default::default(),
        stopping: false,
    };
    m.projects[0].run_pass(&mut m.pool, starter, &shared, now).await;
}

async fn setup(lanes: u32) -> (Rig, Shared_) {
    let (base, script) = store().await;
    (rig(lanes, &[&base]), script)
}

#[tokio::test]
async fn the_first_pass_drops_before_taking_and_a_failed_drop_takes_nothing() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    script.lock().unwrap().drop_fails = true;
    pass(&mut rig, &starter, t0).await;
    assert_eq!(script.lock().unwrap().ops(), vec!["runs_drop"]);

    script.lock().unwrap().drop_fails = false;
    pass(&mut rig, &starter, t0).await;
    pass(&mut rig, &starter, t0).await;
    assert_eq!(
        script.lock().unwrap().ops(),
        vec!["runs_drop", "runs_drop", "run_take", "run_take"],
        "dropped once more after the failure, then only asks"
    );
}

#[tokio::test]
async fn run_take_carries_tags_and_the_starters_kinds_and_stops_asking_when_lanes_are_full() {
    let (mut rig, script) = setup(1).await;
    rig.machine.links.tags = vec!["gpu".to_string()].into();
    let starter = FakeRuns::new(&[RunKind::Build, RunKind::Watch]);
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build")]);
    pass(&mut rig, &starter, Instant::now()).await;

    let takes = script.lock().unwrap().args("run_take");
    assert_eq!(
        takes.len(),
        2,
        "build, then watch alone with the lane full, which gets nothing"
    );
    assert_eq!(takes[0]["kinds"], json!(["build", "watch"]));
    assert_eq!(takes[0]["agent"]["tags"], json!(["gpu"]));
    assert_eq!(takes[0]["agent"]["machine"], "my-pc");
    assert_eq!(takes[1]["kinds"], json!(["watch"]), "a watch run needs no lane");
}

#[tokio::test]
async fn run_take_carries_the_tags_a_save_changed_to() {
    let (mut rig, script) = setup(1).await;
    rig.machine.links.tags = vec!["web".to_string()].into();
    let starter = FakeRuns::new(&[RunKind::Watch]);
    pass(&mut rig, &starter, Instant::now()).await;
    rig.machine.links.tags.set(vec!["gpu".to_string(), "web".to_string()]);
    pass(&mut rig, &starter, Instant::now()).await;
    let takes = script.lock().unwrap().args("run_take");
    assert_eq!(takes[0]["agent"]["tags"], json!(["web"]));
    assert_eq!(takes.last().unwrap()["agent"]["tags"], json!(["gpu", "web"]));
}

#[tokio::test]
async fn no_lane_and_only_lane_kinds_means_no_ask() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build")]);
    pass(&mut rig, &starter, Instant::now()).await;
    assert_eq!(script.lock().unwrap().args("run_take").len(), 1);
}

#[tokio::test]
async fn a_run_starts_with_its_lane_and_waiting_for_stops_asking() {
    let (mut rig, script) = setup(2).await;
    let starter = FakeRuns::new(&[RunKind::Build, RunKind::Watch]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build"), json!({ "waitingFor": ["gpu"] })]);
    pass(&mut rig, &starter, t0).await;
    assert_eq!(starter.started(), vec!["b1"]);
    {
        let started = starter.started.lock().unwrap();
        assert!(started[0].lane.as_ref().unwrap().ends_with("lane-1"));
        assert_eq!(started[0].piece.title, "b1");
    }
    assert_eq!(script.lock().unwrap().args("run_take").len(), 2);

    pass(&mut rig, &starter, t0 + Duration::from_secs(10)).await;
    assert_eq!(script.lock().unwrap().args("run_take").len(), 2, "still waiting");
    pass(&mut rig, &starter, t0 + Duration::from_secs(61)).await;
    assert_eq!(
        script.lock().unwrap().args("run_take").len(),
        2,
        "a beat doesn't end the wait"
    );
    rig.machine.links.work.wake(&rig.machine.projects[0].id);
    pass(&mut rig, &starter, t0 + Duration::from_secs(62)).await;
    assert_eq!(
        script.lock().unwrap().args("run_take").len(),
        3,
        "work_available asks again in the same pass"
    );

    let claim = rig
        .lp(0)
        .events()
        .recent()
        .iter()
        .map(Entry::to_value)
        .collect::<Vec<_>>();
    assert!(
        claim
            .iter()
            .any(|e| e["kind"] == "claim" && e["answer"]["waitingFor"] == json!(["gpu"]))
    );
}

#[tokio::test]
async fn with_the_tags_flag_on_a_wake_ends_a_run_wait_only_when_the_tags_changed() {
    let (mut rig, script) = setup(2).await;
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["tags-reread-on-save"]));
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([json!({ "waitingFor": ["gpu"] })]);
    pass(&mut rig, &starter, t0).await;
    assert_eq!(script.lock().unwrap().args("run_take").len(), 1);
    let id = rig.machine.projects[0].id.clone();
    rig.machine.links.work.wake(&id);
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    pass(&mut rig, &starter, t0 + Duration::from_secs(2)).await;
    assert_eq!(
        script.lock().unwrap().args("run_take").len(),
        1,
        "an unchanged list: no new ask"
    );
    rig.machine.links.tags.set(vec!["gpu".to_string()]);
    rig.machine.links.work.wake(&id);
    rig.machine.links.work.wake(&id);
    pass(&mut rig, &starter, t0 + Duration::from_secs(3)).await;
    let asks = script.lock().unwrap().args("run_take");
    assert_eq!(asks.len(), 2, "one new ask after a change");
    assert_eq!(asks[1]["agent"]["tags"], json!(["gpu"]));
}

#[tokio::test]
async fn each_minute_one_beat_with_every_live_token_and_stop_interrupts() {
    let (mut rig, script) = setup(2).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("a", "build"), run("b", "build")]);
    pass(&mut rig, &starter, t0).await;
    assert!(
        script.lock().unwrap().args("run_beat").is_empty(),
        "no run was live to beat"
    );

    script.lock().unwrap().beat = json!({ "a": "ok", "b": "stop" });
    pass(&mut rig, &starter, t0 + Duration::from_secs(60)).await;
    pass(&mut rig, &starter, t0 + Duration::from_secs(70)).await;
    let s = script.lock().unwrap();
    assert_eq!(s.args("run_beat"), vec![json!({ "tokens": ["a", "b"] })]);
    let done = s.args("run_done");
    assert_eq!(done.len(), 1);
    assert_eq!(
        (done[0]["token"].clone(), done[0]["outcome"].clone()),
        (json!("b"), json!("interrupted"))
    );
    drop(s);
    assert_eq!(rig.machine.pool.held_lanes().count(), 1, "b's lane is free");
}

#[tokio::test]
async fn a_finished_run_sends_its_end_and_frees_its_lane_and_refusals_are_handled() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("a", "build")]);
    pass(&mut rig, &starter, t0).await;
    assert_eq!(rig.machine.pool.held_lanes().count(), 1);

    script.lock().unwrap().done_refused = Some("bad_request");
    starter.finish("a", RunOutcome::PrOpened);
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    assert_eq!(rig.machine.pool.held_lanes().count(), 0);
    let done = |s: &Script| s.args("run_done");
    assert_eq!(done(&script.lock().unwrap())[0]["outcome"], "pr_opened");

    pass(&mut rig, &starter, t0 + Duration::from_secs(30)).await;
    assert_eq!(done(&script.lock().unwrap()).len(), 1, "a refusal waits a minute");
    pass(&mut rig, &starter, t0 + Duration::from_secs(62)).await;
    assert_eq!(done(&script.lock().unwrap()).len(), 2, "and is sent again");

    script.lock().unwrap().done_refused = Some("stale_run");
    pass(&mut rig, &starter, t0 + Duration::from_secs(125)).await;
    assert_eq!(done(&script.lock().unwrap()).len(), 3);
    pass(&mut rig, &starter, t0 + Duration::from_secs(190)).await;
    assert_eq!(done(&script.lock().unwrap()).len(), 3, "a stale run is dropped");
}

#[tokio::test]
async fn asking_by_name_sends_the_piece_and_starts_the_run_it_gets() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([Value::Null, run("b1", "build")]);
    pass(&mut rig, &starter, t0).await;
    let m = &mut rig.machine;
    let shared = Shared {
        links: &m.links,
        local: &m.local,
        claim_paused_until: &mut None,
        claude_signed_out_until: &mut None,
        engine_limits: &mut Default::default(),
        stopping: false,
    };
    let started = m.projects[0]
        .run_take_piece(&mut m.pool, &starter, &shared, "piece-b1", t0)
        .await;
    assert!(started);
    assert_eq!(starter.started(), vec!["b1"]);
    let takes = script.lock().unwrap().args("run_take");
    assert_eq!(takes[1]["piece"], "piece-b1");
    assert!(takes[1].get("kinds").is_none());
    assert!(takes[1]["agent"]["machine"].is_string());
}

/// A machine in `runs` mode: `run_passes` drives the run loop, and the claim loop never runs.
async fn runs_machine(starter: Option<Arc<dyn RunStarter>>) -> (Rig, Shared_) {
    let (mut rig, script) = setup(1).await;
    rig.machine.projects[0].mode = OrchestratorMode::Runs;
    if let Some(s) = starter {
        rig.machine = rig.machine.with_run_starters(vec![s]);
    }
    (rig, script)
}

/// The project's saved state has none of the claim loop's keys (pieces.md section 3.1).
fn assert_no_item_keys(rig: &Rig) {
    let Ok(text) = std::fs::read_to_string(rig._dir.path().join("0").join("state.json")) else {
        return;
    };
    let state: Value = serde_json::from_str(&text).unwrap();
    for key in ["leases", "runs", "unreleased", "unparked"] {
        assert!(state.get(key).is_none(), "{key} in {state}");
    }
}

#[tokio::test]
async fn runs_mode_with_no_runner_drops_old_runs_and_asks_for_nothing() {
    let (mut rig, script) = runs_machine(None).await;
    rig.machine.run_passes().await;
    rig.machine.run_passes().await;
    assert_eq!(script.lock().unwrap().ops(), vec!["runs_drop"]);
    assert_no_item_keys(&rig);
}

#[tokio::test]
async fn runs_mode_sends_runs_drop_then_run_take_and_never_claim_next() {
    let starter = Arc::new(FakeRuns::new(&[RunKind::Build]));
    let (mut rig, script) = runs_machine(Some(starter.clone())).await;
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build")]);
    rig.machine.run_passes().await;
    let ops = script.lock().unwrap().ops();
    assert_eq!(ops[..2], ["runs_drop", "run_take"]);
    assert!(!ops.contains(&"claim_next".to_string()), "{ops:?}");
    assert_eq!(starter.started(), vec!["b1"]);
    assert_no_item_keys(&rig);
}

#[tokio::test]
async fn a_claim_project_is_left_to_the_claim_loop() {
    let (mut rig, script) = setup(1).await;
    rig.machine.run_passes().await;
    assert!(script.lock().unwrap().ops().is_empty());
}

#[tokio::test]
async fn while_stopping_the_loop_takes_no_run_but_still_beats_and_keeps_nothing_in_state() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("a", "build")]);
    pass(&mut rig, &starter, t0).await;
    assert_eq!(starter.started(), ["a"]);
    let takes = script.lock().unwrap().args("run_take").len();

    let m = &mut rig.machine;
    let shared = Shared {
        links: &m.links,
        local: &m.local,
        claim_paused_until: &mut None,
        claude_signed_out_until: &mut None,
        engine_limits: &mut Default::default(),
        stopping: true,
    };
    m.projects[0]
        .run_pass(&mut m.pool, &starter, &shared, t0 + Duration::from_secs(60))
        .await;
    let s = script.lock().unwrap();
    assert_eq!(s.args("run_take").len(), takes, "no run_take while stopping");
    assert_eq!(s.args("run_beat"), vec![json!({ "tokens": ["a"] })]);
    drop(s);
    assert_no_item_keys(&rig);
}

#[cfg(unix)]
#[tokio::test]
async fn a_stop_cuts_the_process_tree_the_run_registered() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("a", "build")]);
    pass(&mut rig, &starter, t0).await;

    let mut command = tokio::process::Command::new("sleep");
    command.arg("60");
    crate::process_tree::ProcessTree::prepare(&mut command);
    let mut child = command.spawn().unwrap();
    let tree = crate::process_tree::ProcessTree::join(&child).unwrap();
    starter.started.lock().unwrap()[0].processes.add("a", tree.clone());

    script.lock().unwrap().beat = json!({ "a": "stop" });
    pass(&mut rig, &starter, t0 + Duration::from_secs(60)).await;
    assert!(tree.was_cut());
    tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("the process was killed")
        .unwrap();
    assert_eq!(script.lock().unwrap().args("run_done")[0]["outcome"], "interrupted");
    assert_eq!(rig.machine.pool.held_lanes().count(), 0);
}

#[tokio::test]
async fn a_worker_takes_a_lock_through_its_context_and_the_next_beat_sends_its_token() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Watch]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("w1", "watch")]);
    script.lock().unwrap().beat = json!({});
    pass(&mut rig, &starter, t0).await;

    let ctx = starter.started.lock().unwrap()[0].context.clone();
    assert_eq!(ctx.project, rig.machine.projects[0].id);
    let got = ctx
        .client
        .lock_take(ctx.repo, Some(&ctx.token), "merge-main", Some("piece"))
        .await
        .unwrap();
    let crate::backlog::runs::LockTake::Held(lock) = got else {
        panic!("{got:?}")
    };
    ctx.locks.add(&lock, "merge-main", t0);
    assert!(ctx.locks.held("L1"));

    pass(&mut rig, &starter, t0 + Duration::from_secs(61)).await;
    let beats = script.lock().unwrap().args("run_beat");
    assert_eq!(beats.last().unwrap()["tokens"], json!(["w1", "L1"]));
}

/// A machine whose only lane is busy, a project with a repo whose base branch has CI, and a `watch` starter on a
/// fake `gh` that the store hands one `watch` run.
#[cfg(unix)]
async fn watch_rig(dir: &std::path::Path, poll: Duration) -> (Rig, Shared_, Arc<dyn RunStarter>) {
    use crate::orchestrator::watch_run::{WatchRuns, tests as w};
    let (mut rig, script) = setup(1).await;
    assert!(rig.machine.pool.claim_run("other", "x").is_some(), "the lane is busy");
    let repo = w::repo_with_ci(dir).display().to_string();
    rig.machine.projects[0].project.repo_path = repo.clone();
    rig.machine.projects[0].file_project.repo_path = repo;
    rig.machine.projects[0].project.base_branch = "main".into();
    rig.machine.projects[0].file_project.base_branch = "main".into();
    let mut take = run("w1", "watch");
    take["piece"]["change"] = json!({ "number": 7 });
    script.lock().unwrap().takes = VecDeque::from([take]);
    script.lock().unwrap().beat = json!({});
    let starter: Arc<dyn RunStarter> = WatchRuns::with(w::fake_gh(dir), poll, None);
    (rig, script, starter)
}

#[cfg(unix)]
#[tokio::test]
async fn a_watch_run_is_asked_for_with_every_lane_busy_and_sends_its_outcome() {
    use crate::orchestrator::watch_run::tests as w;
    let dir = tempfile::tempdir().unwrap();
    w::set(dir.path(), "checks", "t\tpass\t1s\thttps://x/t\t\n");
    let (mut rig, script, starter) = watch_rig(dir.path(), Duration::from_millis(5)).await;
    let t0 = Instant::now();
    for _ in 0..200 {
        pass(&mut rig, &*starter, t0).await;
        if !script.lock().unwrap().args("run_done").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let s = script.lock().unwrap();
    assert_eq!(s.args("run_take")[0]["kinds"], json!(["watch"]));
    let done = s.args("run_done");
    assert_eq!(
        (done[0]["token"].clone(), done[0]["outcome"].clone()),
        (json!("w1"), json!("green"))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_beat_stop_cuts_the_watch_runs_gh_and_ends_it_interrupted() {
    use crate::orchestrator::watch_run::tests as w;
    let dir = tempfile::tempdir().unwrap();
    w::set(dir.path(), "hang", "");
    w::set(dir.path(), "checks", "");
    let (mut rig, script, starter) = watch_rig(dir.path(), Duration::from_millis(5)).await;
    let t0 = Instant::now();
    pass(&mut rig, &*starter, t0).await;
    for _ in 0..200 {
        if !w::calls(dir.path()).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!w::calls(dir.path()).is_empty(), "gh started");
    script.lock().unwrap().beat = json!({ "w1": "stop" });
    let started = Instant::now();
    pass(&mut rig, &*starter, t0 + Duration::from_secs(61)).await;
    assert!(started.elapsed() < Duration::from_secs(20), "the 30-second gh was cut");
    let done = script.lock().unwrap().args("run_done");
    assert_eq!(
        (done[0]["token"].clone(), done[0]["outcome"].clone()),
        (json!("w1"), json!("interrupted"))
    );
    assert!(rig.machine.projects[0].run_loop.runs().live().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn a_beat_stop_between_polls_ends_the_run_and_starts_no_more_gh() {
    use crate::orchestrator::watch_run::tests as w;
    let dir = tempfile::tempdir().unwrap();
    w::set(dir.path(), "checks", "t\tpending\t0\thttps://x/t\t\n");
    let (mut rig, script, starter) = watch_rig(dir.path(), Duration::from_secs(1)).await;
    let t0 = Instant::now();
    pass(&mut rig, &*starter, t0).await;
    for _ in 0..200 {
        if !w::calls(dir.path()).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // The poll's gh has finished and the task sleeps.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = w::calls(dir.path()).len();
    script.lock().unwrap().beat = json!({ "w1": "stop" });
    pass(&mut rig, &*starter, t0 + Duration::from_secs(61)).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    pass(&mut rig, &*starter, t0 + Duration::from_secs(62)).await;
    assert_eq!(w::calls(dir.path()).len(), before, "no gh after the stop");
    let done = script.lock().unwrap().args("run_done");
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0]["outcome"], json!("interrupted"));
}

fn turn(token: &str) -> Value {
    json!({ "turn": { "token": token, "leaseMs": 300000, "entry": "e",
        "piece": { "id": format!("piece-{token}"), "title": token } } })
}

#[tokio::test]
async fn a_piece_merge_comes_from_merge_next_with_no_lane_and_its_turn_beats_with_merge_beat() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build, RunKind::Merge]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build")]);
    script.lock().unwrap().merges = VecDeque::from([turn("m1")]);
    pass(&mut rig, &starter, t0).await;
    pass(&mut rig, &starter, t0).await;
    {
        let s = script.lock().unwrap();
        assert_eq!(s.args("merge_next")[0], json!({ "base": "", "piece": true }));
        assert!(s.args("run_take").iter().all(|t| t["kinds"] != json!(["merge"])));
        assert_eq!(
            s.args("run_take")[0]["kinds"],
            json!(["build"]),
            "run_take never asks for a merge"
        );
    }
    assert_eq!(starter.started(), vec!["m1", "b1"]);
    let context = starter.started.lock().unwrap()[0].context.clone();
    assert!(context.locks.held("m1"), "the turn is in the run's locks");

    pass(&mut rig, &starter, t0 + Duration::from_secs(60)).await;
    let s = script.lock().unwrap();
    assert_eq!(s.args("run_beat"), vec![json!({ "tokens": ["b1"] })]);
    assert_eq!(s.args("merge_beat"), vec![json!({ "token": "m1", "phase": "merge" })]);
    assert_eq!(s.args("merge_next").len(), 1, "no more asked while the turn is held");
}

#[tokio::test]
async fn a_waiting_answer_starts_nothing_and_is_asked_again() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Merge]);
    let t0 = Instant::now();
    script.lock().unwrap().merges = VecDeque::from([json!({ "waiting": { "place": 2, "ahead": 1 } }), turn("m1")]);
    pass(&mut rig, &starter, t0).await;
    assert!(starter.started().is_empty());
    pass(&mut rig, &starter, t0 + Duration::from_secs(15)).await;
    assert_eq!(starter.started(), vec!["m1"]);
}

#[tokio::test]
async fn a_lost_turn_stops_the_run_and_a_finished_one_ends_with_merge_result_not_run_done() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Merge]);
    let t0 = Instant::now();
    script.lock().unwrap().merges = VecDeque::from([turn("m1"), turn("m2")]);
    pass(&mut rig, &starter, t0).await;
    script.lock().unwrap().turn_lost = true;
    pass(&mut rig, &starter, t0 + Duration::from_secs(60)).await;
    let context = starter.started.lock().unwrap()[0].context.clone();
    assert!(!context.locks.held("m1"));
    assert!(starter.started.lock().unwrap()[0].stop.stopped());
    {
        let s = script.lock().unwrap();
        assert_eq!(
            s.args("merge_result"),
            vec![json!({ "token": "m1", "outcome": "released" })]
        );
        assert!(s.args("run_done").is_empty());
    }

    script.lock().unwrap().turn_lost = false;
    pass(&mut rig, &starter, t0 + Duration::from_secs(61)).await;
    let request = starter.started.lock().unwrap()[1].clone();
    request.ends.send(RunEnd {
        token: "m2".into(),
        outcome: RunOutcome::Merged,
        result: json!({ "commit": "abc" }),
    });
    pass(&mut rig, &starter, t0 + Duration::from_secs(62)).await;
    let s = script.lock().unwrap();
    let results = s.args("merge_result");
    assert_eq!(
        results[1],
        json!({ "token": "m2", "outcome": "merged", "mergeCommit": "abc" })
    );
    assert!(s.args("run_done").is_empty());
}

#[tokio::test]
async fn a_store_that_cant_be_asked_for_a_turn_starts_no_merge() {
    let (rig_, script) = setup(1).await;
    let mut rig = rig_;
    let starter = FakeRuns::new(&[RunKind::Merge]);
    script.lock().unwrap().merges = VecDeque::from([json!({ "oops": 1 })]);
    pass(&mut rig, &starter, Instant::now()).await;
    assert!(starter.started().is_empty());
}

#[tokio::test]
async fn a_failure_from_outside_the_work_says_outside_and_an_ordinary_one_does_not() {
    let (mut rig, script) = setup(2).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("a", "build"), run("b", "build")]);
    pass(&mut rig, &starter, t0).await;
    starter.finish("a", RunOutcome::Failed("connect ECONNREFUSED 127.0.0.1:443".into()));
    starter.finish("b", RunOutcome::Failed("checks still fail: test".into()));
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    let done = script.lock().unwrap().args("run_done");
    let result = |t: &str| done.iter().find(|d| d["token"] == t).unwrap()["result"].clone();
    assert_eq!(result("a")["outside"], true);
    assert_eq!(result("a")["why"], "connect ECONNREFUSED 127.0.0.1:443");
    assert!(result("b").get("outside").is_none());
}

fn check(ok: Option<bool>) -> crate::setup_check::SetupCheck {
    crate::setup_check::SetupCheck {
        ok,
        detail: String::new(),
        fix: None,
    }
}

/// What the first `run_take`'s `agent` says, with `checks` as the latest setup checks.
async fn agent_with(checks: Option<crate::setup_check::SetupChecks>, choice: EngineChoiceConfig) -> Value {
    let (mut rig, script) = setup(1).await;
    *rig.machine.links.checks.lock().unwrap() = checks;
    rig.machine.links.engine_choice = choice;
    pass(&mut rig, &FakeRuns::new(&[RunKind::Build]), Instant::now()).await;
    let takes = script.lock().unwrap().args("run_take");
    takes[0]["agent"].clone()
}

fn checks_with(claude_signed_in: Option<bool>, cursor: bool) -> crate::setup_check::SetupChecks {
    crate::setup_check::SetupChecks {
        git: check(Some(true)),
        gh_installed: check(Some(true)),
        gh: check(Some(true)),
        claude: check(Some(true)),
        claude_signed_in: check(claude_signed_in),
        cursor: check(Some(cursor)),
        pi: Some(check(Some(true))),
        codex: Some(check(Some(true))),
        docker: None,
    }
}

/// Like `agent_with`, .
async fn placed_agent_with(checks: Option<crate::setup_check::SetupChecks>) -> Value {
    let (mut rig, script) = setup(1).await;
    *rig.machine.links.checks.lock().unwrap() = checks;
    pass(&mut rig, &FakeRuns::new(&[RunKind::Build]), Instant::now()).await;
    let takes = script.lock().unwrap().args("run_take");
    takes[0]["agent"].clone()
}

#[tokio::test]
async fn run_take_sends_every_passing_engine_with_placement_on() {
    let all = placed_agent_with(Some(checks_with(Some(true), true))).await;
    assert_eq!(all["engines"], json!(["claude", "cursor", "pi", "codex"]));
    let no_cursor = placed_agent_with(Some(checks_with(Some(true), false))).await;
    assert_eq!(no_cursor["engines"], json!(["claude", "pi", "codex"]));
    let mut failing = checks_with(Some(true), true);
    failing.codex = Some(check(Some(false)));
    failing.pi = None;
    let some = placed_agent_with(Some(failing)).await;
    assert_eq!(some["engines"], json!(["claude", "cursor"]));
    let signed_out = placed_agent_with(Some(checks_with(Some(false), true))).await;
    assert_eq!(signed_out["engines"], json!(["cursor", "pi", "codex"]));
    let none_yet = placed_agent_with(None).await;
    assert_eq!(none_yet["engines"], json!([]));
}

#[tokio::test]
async fn run_take_uses_follow_the_configured_roles() {
    let choice: EngineChoiceConfig = serde_json::from_value(json!({ "roles": { "implementer": "cursor" } })).unwrap();
    let agent = agent_with(None, choice).await;
    assert_eq!(
        agent["uses"],
        json!({ "plan": "claude", "build": "cursor", "fix": "cursor", "look": "claude" })
    );
}

fn chore_run(token: &str, kind: &str) -> Value {
    let mut r = run(token, kind);
    r["piece"]["chore"] = json!({ "kind": token });
    r
}

#[tokio::test]
async fn chores_off_is_sent_and_a_stray_rotation_chore_is_given_back_but_fix_main_runs() {
    let (mut rig, script) = setup(2).await;
    rig.lp(0).project.chores = crate::config::OnOff::Off;
    rig.lp(0).file_project.chores = crate::config::OnOff::Off;
    let starter = FakeRuns::new(&[RunKind::Build]);
    script.lock().unwrap().takes = VecDeque::from([chore_run("tidy", "build"), chore_run("fix_main", "build")]);
    pass(&mut rig, &starter, Instant::now()).await;

    let s = script.lock().unwrap();
    assert_eq!(s.args("run_take")[0]["agent"]["chores"], json!(false));
    let done = s.args("run_done");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0]["outcome"], "interrupted");
    assert_eq!(starter.started(), vec!["fix_main"], "the tidy chore ran no session");
}

#[tokio::test]
async fn chores_on_sends_no_key_and_the_stores_count_is_kept_for_the_status() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    let mut r = chore_run("tidy", "build");
    r["chores"] = json!({ "today": 2, "limit": 3 });
    script.lock().unwrap().takes = VecDeque::from([r]);
    let id = rig.machine.projects[0].id.clone();
    assert_eq!(rig.live.snapshot().projects[&id].chores, None);
    pass(&mut rig, &starter, Instant::now()).await;

    assert!(
        script.lock().unwrap().args("run_take")[0]["agent"]
            .get("chores")
            .is_none()
    );
    assert_eq!(starter.started(), vec!["tidy"]);
    let c = rig.live.snapshot().projects[&id].chores.unwrap();
    assert_eq!((c.today, c.limit), (2, 3));
}

/// A pass with Claude signed out for a minute and `limited` engines at their limit.
async fn start_rules_pass(
    rig: &mut Rig,
    starter: &dyn RunStarter,
    limited: &[crate::engines::EngineId],
    claude_signed_out: bool,
) {
    start_pass_with(rig, starter, limited, claude_signed_out).await;
}

async fn start_pass_with(
    rig: &mut Rig,
    starter: &dyn RunStarter,
    limited: &[crate::engines::EngineId],
    claude_signed_out: bool,
) {
    let now = Instant::now();
    let limits = crate::orchestrator::fresh_start::EngineLimits::default();
    for e in limited {
        limits.hit(*e, now + Duration::from_secs(600));
    }
    let mut out = claude_signed_out.then(|| now + Duration::from_secs(600));
    let m = &mut rig.machine;
    let mut limits = limits;
    let shared = Shared {
        links: &m.links,
        local: &m.local,
        claim_paused_until: &mut None,
        claude_signed_out_until: &mut out,
        engine_limits: &mut limits,
        stopping: false,
    };
    m.projects[0].run_pass(&mut m.pool, starter, &shared, now).await;
}

fn both_engines(rig: &mut Rig, choice: Value) {
    *rig.machine.links.checks.lock().unwrap() = Some(checks_with(Some(true), true));
    rig.machine.links.engine_choice = serde_json::from_value(choice).unwrap();
}

#[tokio::test]
async fn with_claude_limited_run_take_leaves_out_claude_and_uses_name_cursor() {
    use crate::engines::EngineId::Claude;
    let (mut rig, script) = setup(1).await;
    both_engines(
        &mut rig,
        json!({ "roles": { "implementer": "cursor" }, "default": "claude" }),
    );
    rig.machine.links.engine_choice =
        serde_json::from_value(json!({ "default": "claude", "lanes": ["cursor"] })).unwrap();
    start_rules_pass(
        &mut rig,
        &FakeRuns::new(&[RunKind::Build, RunKind::Watch]),
        &[Claude],
        false,
    )
    .await;
    let takes = script.lock().unwrap().args("run_take");
    assert_eq!(takes[0]["agent"]["engines"], json!(["cursor", "pi", "codex"]));
    assert_eq!(takes[0]["agent"]["uses"]["build"], "cursor");
    assert_eq!(
        takes[0]["agent"]["uses"]["plan"], "cursor",
        "Claude's planner falls over to Cursor"
    );
    assert_eq!(takes[0]["kinds"], json!(["build", "watch"]));
}

#[tokio::test]
async fn with_cursor_limited_run_take_leaves_out_cursor() {
    use crate::engines::EngineId::Cursor;
    let (mut rig, script) = setup(1).await;
    both_engines(&mut rig, json!({ "default": "claude" }));
    start_rules_pass(&mut rig, &FakeRuns::new(&[RunKind::Build]), &[Cursor], false).await;
    let takes = script.lock().unwrap().args("run_take");
    assert_eq!(takes[0]["agent"]["engines"], json!(["claude", "pi", "codex"]));
}

/// A Cursor implementer whose Cursor is at its limit, on a computer whose planner runs Claude; one build is handed out.
async fn cursor_limited_build() -> FakeRuns {
    let (mut rig, script) = setup(1).await;
    both_engines(
        &mut rig,
        json!({ "default": "cursor", "roles": { "planner": "claude" } }),
    );
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build")]);
    let starter = FakeRuns::new(&[RunKind::Build]);
    start_pass_with(&mut rig, &starter, &[crate::engines::EngineId::Cursor], false).await;
    starter
}

#[tokio::test]
async fn a_build_started_while_cursor_is_limited_is_placed_on_claude() {
    use crate::engines::EngineId::Claude;
    let starter = cursor_limited_build().await;
    let started = starter.started.lock().unwrap();
    assert_eq!(started[0].engine, Some(Claude));
    assert!(
        started[0]
            .context
            .fall
            .as_ref()
            .unwrap()
            .out
            .contains(&crate::engines::EngineId::Cursor)
    );
}

#[tokio::test]
async fn with_every_engine_out_no_run_take_is_sent() {
    use crate::engines::EngineId::{Claude, Codex, Cursor, Pi};
    let (mut rig, script) = setup(1).await;
    both_engines(&mut rig, json!({ "default": "claude", "lanes": ["cursor"] }));
    start_rules_pass(
        &mut rig,
        &FakeRuns::new(&[RunKind::Build]),
        &[Claude, Cursor, Pi, Codex],
        false,
    )
    .await;
    assert!(script.lock().unwrap().args("run_take").is_empty());
}

#[tokio::test]
async fn a_build_takes_a_cursor_lane_when_claude_is_signed_out_and_claude_lanes_first_otherwise() {
    let lane = |rig: &Rig, s: &FakeRuns| {
        let want = crate::lanes::lane_path(std::path::Path::new(&rig.machine.projects[0].project.worktrees_dir), 1);
        let got = s.started.lock().unwrap()[0].lane.clone().unwrap();
        got == want
    };
    let (mut rig, script) = setup(2).await;
    both_engines(&mut rig, json!({ "default": "claude", "lanes": ["claude", "cursor"] }));
    assert!(rig.machine.pool.claim_run("other", "x").is_some(), "lane 1 is busy");
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build")]);
    let starter = FakeRuns::new(&[RunKind::Build]);
    start_rules_pass(&mut rig, &starter, &[], true).await;
    assert_eq!(starter.started(), vec!["b1"], "the Cursor lane takes it");
    assert!(!lane(&rig, &starter));

    let (mut rig, script) = setup(2).await;
    both_engines(&mut rig, json!({ "default": "claude", "lanes": ["claude", "cursor"] }));
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build")]);
    let starter = FakeRuns::new(&[RunKind::Build]);
    start_rules_pass(&mut rig, &starter, &[], false).await;
    assert!(lane(&rig, &starter), "Claude's lane first");
}

/// Claude at its usage limit holds a plan or build only without the model or
/// sign-in; with it they are asked for like any run (they start on the local model).
#[tokio::test]
async fn at_the_limit_runs_are_asked_for_with_the_fallback_and_held_without_it() {
    use crate::engines::EngineId::Claude;
    use crate::local_model::LocalModel;
    for (model, signed_out, asks) in [
        (LocalModel::up(), false, true),
        (LocalModel::off(), false, false),
        (LocalModel::up(), true, false),
    ] {
        let (mut rig, script) = setup(1).await;
        both_engines(&mut rig, json!({ "default": "claude" }));
        rig.machine.local = model;
        let starter = FakeRuns::new(&[RunKind::Build, RunKind::Plan]);
        start_rules_pass(&mut rig, &starter, &[Claude], signed_out).await;
        let takes = script.lock().unwrap().args("run_take");
        let case = signed_out;
        assert_eq!(takes.is_empty(), !asks, "{case:?}");
        if asks {
            assert_eq!(takes[0]["kinds"], json!(["build", "plan"]));
            assert!(
                takes[0]["agent"]["engines"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("claude"))
            );
        }
    }
}

mod merged;
mod restart;
