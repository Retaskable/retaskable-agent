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

async fn pass(rig: &mut Rig, starter: &FakeRuns, now: Instant) {
    let m = &mut rig.machine;
    let shared = Shared {
        links: &m.links,
        claim_paused_until: &mut None,
        claude_signed_out_until: &mut None,
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
        3,
        "the minute's pass asks again"
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
