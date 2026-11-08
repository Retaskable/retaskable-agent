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
    rig.machine.links.tags = vec!["gpu".into()];
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
    rig.machine.projects[0].project.repo_path = w::repo_with_ci(dir).display().to_string();
    rig.machine.projects[0].project.base_branch = "main".into();
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

#[tokio::test]
async fn a_merge_run_is_asked_for_with_no_lane_free_and_its_lock_beats_with_it() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build, RunKind::Merge]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build"), run("m1", "merge")]);
    pass(&mut rig, &starter, t0).await;
    pass(&mut rig, &starter, t0).await;
    let takes = script.lock().unwrap().args("run_take");
    assert_eq!(takes[1]["kinds"], json!(["merge"]), "the build's lane is the only one");
    assert_eq!(starter.started(), vec!["b1", "m1"]);

    // The merge run takes its lock through the handle the loop gave it.
    let context = starter.started.lock().unwrap()[1].context.clone();
    let taken = context
        .client
        .lock_take(
            context.repo,
            Some(&context.token),
            "merge-main",
            Some("piece-m1 on here"),
        )
        .await
        .unwrap();
    let crate::backlog::runs::LockTake::Held(lock) = taken else {
        panic!("{taken:?}")
    };
    context.locks.add(&lock, "merge-main", Instant::now());

    pass(&mut rig, &starter, t0 + Duration::from_secs(60)).await;
    let beats = script.lock().unwrap().args("run_beat");
    assert_eq!(beats, vec![json!({ "tokens": ["b1", "m1", "L1"] })]);
}

#[tokio::test]
async fn a_failure_from_outside_the_work_says_outside_and_an_ordinary_one_does_not() {
    let (mut rig, script) = setup(2).await;
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["run-limits-and-causes"]));
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

#[tokio::test]
async fn with_the_flag_off_a_failure_from_outside_the_work_sends_no_outside() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("a", "build")]);
    pass(&mut rig, &starter, t0).await;
    starter.finish("a", RunOutcome::Failed("connect ECONNREFUSED 127.0.0.1:443".into()));
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    let done = script.lock().unwrap().args("run_done");
    assert!(done[0]["result"].get("outside").is_none());
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

/// Like `agent_with`, with `engine-placement` On.
async fn placed_agent_with(checks: Option<crate::setup_check::SetupChecks>) -> Value {
    let (mut rig, script) = setup(1).await;
    *rig.machine.links.checks.lock().unwrap() = checks;
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["engine-placement"]));
    pass(&mut rig, &FakeRuns::new(&[RunKind::Build]), Instant::now()).await;
    let takes = script.lock().unwrap().args("run_take");
    takes[0]["agent"].clone()
}

#[tokio::test]
async fn run_take_sends_claude_only_with_placement_off() {
    let signed_in = agent_with(Some(checks_with(Some(true), true)), Default::default()).await;
    assert_eq!(signed_in["engines"], json!(["claude"]));
    let signed_out = agent_with(Some(checks_with(Some(false), true)), Default::default()).await;
    assert_eq!(signed_out["engines"], json!([]));
    let none_yet = agent_with(None, Default::default()).await;
    assert_eq!(none_yet["engines"], json!([]));
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
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["chore-rotation"]));
    rig.lp(0).project.chores = crate::config::OnOff::Off;
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
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["chore-rotation"]));
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
