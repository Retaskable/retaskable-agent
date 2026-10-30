use super::*;
use crate::backlog::Client;
use crate::env::Env;
use crate::item_lifecycle::{ImplementerOutcome, ItemRun, ItemState};
use crate::lane_pool::Holder;
use crate::link::TokenStore;
use crate::orchestrator::guard::PortGuard;
use crate::orchestrator::live::{HeldLane, LiveHandle};
use crate::orchestrator::steps::StepRequest;
use crate::store::{RunRecord, Store};
use crate::telemetry::EventLog;
use crate::test_support::item_lifecycle_cfg;
use crate::view::PauseState;
use anyhow::Result;
use axum::{Json, Router, routing::post};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

#[test]
fn the_mode_defaults_to_off_and_reads_the_three_names() {
    let mode = |v: Option<&str>| match v {
        Some(v) => OrchestratorMode::from_env(&Env::from_pairs([(MODE_VAR, v)])),
        None => OrchestratorMode::from_env(&Env::empty()),
    };
    assert_eq!(mode(None), (OrchestratorMode::Off, None));
    assert_eq!(mode(Some("off")), (OrchestratorMode::Off, None));
    assert_eq!(mode(Some("dry")), (OrchestratorMode::Dry, None));
    assert_eq!(mode(Some(" Claim ")), (OrchestratorMode::Claim, None));
}

#[test]
fn an_unrecognised_mode_is_off_with_a_warning() {
    let (mode, warning) = OrchestratorMode::from_env(&Env::from_pairs([(MODE_VAR, "yes")]));
    assert_eq!(mode, OrchestratorMode::Off);
    assert!(warning.unwrap().contains("stays off"));
}

/// A fake store that remembers each op it was asked, answering `list` with one todo item.
async fn stub() -> (String, Arc<Mutex<Vec<String>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(body["op"].as_str().unwrap_or("").to_string());
                Json(json!({ "value": [{ "id": "a", "title": "A", "status": "todo", "rank": 0 }] }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

/// The link token the keychain answers with (`None` is no link), and whether the next read panics.
struct Keys {
    token: Mutex<Option<String>>,
    panic_once: AtomicBool,
}

impl TokenStore for Keys {
    fn get(&self, _url: &str) -> Result<Option<String>> {
        if self.panic_once.swap(false, Ordering::SeqCst) {
            panic!("a read that goes wrong");
        }
        Ok(self.token.lock().unwrap().clone())
    }
    fn set(&self, _url: &str, _token: &str) -> Result<()> {
        Ok(())
    }
    fn delete(&self, _url: &str) -> Result<()> {
        Ok(())
    }
}

struct Rig {
    machine: Machine,
    keys: Arc<Keys>,
    live: LiveHandle,
    _dir: tempfile::TempDir,
}

impl Rig {
    fn lp(&mut self, i: usize) -> &mut ProjectLoop {
        &mut self.machine.projects[i]
    }

    fn pause(&self, project: &str, on: bool) {
        self.machine.links.paused.set(project, on.then(|| "test".to_string()));
    }

    /// Puts every project in `claim` mode with a fake session starter.
    fn go_live(&mut self) -> Arc<Fake> {
        let fake = Arc::new(Fake::default());
        self.machine.starter = Some(fake.clone());
        for lp in &mut self.machine.projects {
            lp.mode = OrchestratorMode::Claim;
        }
        fake
    }

    /// Gives a project `ids` as queued items, to be started.
    fn queue(&mut self, i: usize, ids: &[&str]) {
        for id in ids {
            self.lp(i).runs.insert(id.to_string(), ItemRun::new());
            let item = json!({"id": id, "title": id, "status": "claimed", "rank": 1});
            (self.lp(i).items).insert(id.to_string(), serde_json::from_value(item).unwrap());
        }
    }
}

/// One project per base URL, named `p0`, `p1`, ..., on a computer with `lanes` lanes.
fn rig(lanes: u32, bases: &[&str]) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(Keys {
        token: Mutex::new(Some("tok".into())),
        panic_once: AtomicBool::new(false),
    });
    let live = LiveHandle::new(OrchestratorMode::Dry);
    let links = Links {
        backlog_url: "https://backlog.example".into(),
        keychain: keys.clone(),
        paused: PauseState::default(),
        machine: "my-pc".into(),
        environment: "retaskable.com".into(),
    };
    let mut machine = Machine::new(lanes, 8, links);
    for (i, base) in bases.iter().enumerate() {
        let store = Store::load(&dir.path().join(i.to_string())).unwrap();
        let mut lp = ProjectLoop::new(
            format!("p{i}"),
            item_lifecycle_cfg(),
            store,
            EventLog::with_now(|| 0),
            Client::new(*base),
            live.clone(),
        );
        lp.repo = Some(7);
        machine.add(lp);
    }
    Rig {
        machine,
        keys,
        live,
        _dir: dir,
    }
}

fn would(step: &Step) -> Vec<&str> {
    match step {
        Step::Ticked { would } => would.iter().map(String::as_str).collect(),
        Step::Skipped => panic!("skipped"),
    }
}

#[tokio::test]
async fn one_dry_tick_writes_a_tick_event_and_asks_the_store_only_to_list() {
    let (base, asked) = stub().await;
    let mut rig = rig(2, &[&base]);
    let steps = rig.machine.pass().await;
    assert_eq!(
        steps,
        vec![Step::Ticked {
            would: vec!["ask_for_work".into()]
        }]
    );
    assert_eq!(
        *asked.lock().unwrap(),
        vec!["list".to_string()],
        "no claim_next was sent"
    );
    assert_eq!(
        rig.lp(0)
            .events()
            .recent()
            .iter()
            .map(|e| e.to_value())
            .collect::<Vec<_>>(),
        vec![json!({
            "at": "1970-01-01T00:00:00.000Z",
            "kind": "tick",
            "project": "p0",
            "status": "running",
            "lanes": 2,
            "busy": [],
            "free": 2,
            "sessions": 0,
            "states": { "todo": 1 },
            "claim": "not asked",
            "dry": ["ask_for_work"]
        })]
    );
    assert_eq!(rig.lp(0).runs.values().count(), 0);
    assert_eq!(rig.machine.pool.held_lanes().count(), 0, "a dry tick holds no lane");
}

#[tokio::test]
async fn the_live_state_follows_each_pass() {
    let (base, _asked) = stub().await;
    let mut rig = rig(2, &[&base]);
    let proj = |rig: &Rig| rig.live.snapshot().projects["p0"].clone();
    assert_eq!(proj(&rig), Default::default(), "registered before its first pass");

    rig.machine.pass().await;
    let live = proj(&rig);
    assert!(live.last_tick.is_some());
    assert_eq!(live.decision.as_deref(), Some("would ask for work"));
    assert!(live.held.is_empty() && !live.paused && live.store_error.is_none());

    *rig.keys.token.lock().unwrap() = None;
    rig.pause("p0", true);
    rig.machine.pass().await;
    let failed = proj(&rig);
    assert!(failed.store_error.unwrap().message.contains("no link"));
    assert_eq!(
        failed.last_tick, live.last_tick,
        "a failed pass leaves the last tick as it was"
    );

    *rig.keys.token.lock().unwrap() = Some("tok".into());
    rig.machine.pass().await;
    let live = proj(&rig);
    assert!(live.paused && live.store_error.is_none());
    assert_eq!(live.decision.as_deref(), Some("paused: would do nothing"));
}

#[tokio::test]
async fn a_paused_project_ticks_paused_and_would_do_nothing() {
    let (base, _asked) = stub().await;
    let mut rig = rig(2, &[&base]);
    rig.pause("p0", true);
    assert_eq!(rig.machine.pass().await, vec![Step::Ticked { would: vec![] }]);
    assert_eq!(rig.lp(0).events().recent()[0].to_value()["status"], "paused");
}

#[tokio::test]
async fn an_unreachable_backlog_is_a_store_error_and_no_tick() {
    // Port 1 on loopback refuses connections.
    let mut rig = rig(2, &["http://127.0.0.1:1"]);
    assert_eq!(rig.machine.pass().await, vec![Step::Skipped]);
    let kinds: Vec<_> = rig.lp(0).events().recent().iter().map(|e| e.kind()).collect();
    assert_eq!(kinds, vec!["store_error"]);
    assert_eq!(rig.lp(0).events().recent()[0].to_value()["op"], "list");
}

#[tokio::test]
async fn no_token_means_the_store_isnt_asked_and_the_tick_is_skipped() {
    let (base, asked) = stub().await;
    let mut rig = rig(2, &[&base]);
    *rig.keys.token.lock().unwrap() = None;
    assert_eq!(rig.machine.pass().await, vec![Step::Skipped]);
    assert!(asked.lock().unwrap().is_empty());
    assert_eq!(rig.lp(0).events().recent()[0].kind(), "store_error");
}

#[tokio::test]
async fn two_projects_on_one_lane_take_turns_starting() {
    let (base, _asked) = stub().await;
    let mut rig = rig(1, &[&base, &base]);
    rig.queue(0, &["x"]);
    rig.queue(1, &["y"]);
    let first = rig.machine.pass().await;
    assert_eq!(would(&first[0]), ["start_implementer x in lane 1"]);
    assert!(would(&first[1]).is_empty(), "the one lane went to p0");
    let second = rig.machine.pass().await;
    assert_eq!(would(&second[1]), ["start_implementer y in lane 1"]);
    assert!(would(&second[0]).is_empty(), "p1 is served first after p0");
    assert_eq!(rig.machine.pool.held_lanes().count(), 0, "nothing was applied");
}

#[tokio::test]
async fn three_projects_with_work_and_two_lanes_alternate() {
    let (base, _asked) = stub().await;
    let mut rig = rig(2, &[&base, &base, &base]);
    for i in 0..3 {
        rig.queue(i, &["i1", "i2"]);
    }
    let first = rig.machine.pass().await;
    assert_eq!(would(&first[0]), ["start_implementer i1 in lane 1"]);
    assert_eq!(would(&first[1]), ["start_implementer i1 in lane 2"]);
    assert!(would(&first[2]).is_empty());
    let second = rig.machine.pass().await;
    assert_eq!(would(&second[2]), ["start_implementer i1 in lane 1"]);
    assert_eq!(would(&second[0]), ["start_implementer i1 in lane 2"]);
    assert!(would(&second[1]).is_empty());
}

#[tokio::test]
async fn one_projects_unreachable_backlog_leaves_the_others_ticking() {
    let (base, _asked) = stub().await;
    let mut rig = rig(2, &["http://127.0.0.1:1", &base, &base]);
    let steps = rig.machine.pass().await;
    assert_eq!(steps[0], Step::Skipped);
    assert_eq!(would(&steps[1]), ["ask_for_work"]);
    assert_eq!(would(&steps[2]), ["ask_for_work"]);
    let projects = rig.live.snapshot().projects;
    assert!(projects["p0"].store_error.is_some() && projects["p0"].last_tick.is_none());
    assert!(projects["p1"].store_error.is_none() && projects["p2"].last_tick.is_some());
    assert_eq!(rig.lp(0).events().recent()[0].kind(), "store_error");
}

#[tokio::test]
async fn a_panicking_read_fails_only_its_project() {
    let (base, _asked) = stub().await;
    let mut rig = rig(2, &[&base, &base]);
    rig.keys.panic_once.store(true, Ordering::SeqCst);
    let steps = rig.machine.pass().await;
    assert_eq!(steps.iter().filter(|s| **s == Step::Skipped).count(), 1);
    assert_eq!(steps.iter().filter(|s| matches!(s, Step::Ticked { .. })).count(), 1);
    let failed = rig
        .live
        .snapshot()
        .projects
        .into_values()
        .filter(|p| p.store_error.is_some());
    assert!(
        failed
            .map(|p| p.store_error.unwrap().message)
            .all(|m| m.contains("read failed"))
    );
    assert!(
        matches!(rig.machine.pass().await[..], [Step::Ticked { .. }, Step::Ticked { .. }]),
        "the next pass reads both"
    );
}

#[tokio::test]
async fn held_lanes_show_in_live_state_under_the_project_that_holds_them() {
    let (base, _asked) = stub().await;
    let mut rig = rig(3, &[&base, &base]);
    let held = |project: &str, item: &str| Holder::Item {
        project: project.into(),
        item: item.into(),
    };
    assert!(rig.machine.pool.take(1, held("p0", "a")));
    assert!(rig.machine.pool.take(3, held("p1", "b")));
    rig.machine.pass().await;
    let projects = rig.live.snapshot().projects;
    let lane = |lane, item: &str, title: Option<&str>| HeldLane {
        lane,
        item_id: item.into(),
        title: title.map(Into::into),
    };
    assert_eq!(projects["p0"].held, vec![lane(1, "a", Some("A"))]);
    assert_eq!(projects["p1"].held, vec![lane(3, "b", None)]);
    let busy = rig.lp(1).events().recent()[0].to_value()["busy"].clone();
    assert_eq!(busy, json!([1, 3]), "the tick event shows the computer's lanes");
}

async fn ts_agent_port() -> u16 {
    let app = Router::new().route(
        "/api/health",
        axum::routing::get(|| async { Json(json!({ "ok": true, "version": "1" })) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn claim_runs_dry_while_another_agent_answers_and_stays_claim_otherwise() {
    let guard = PortGuard::new(closed_port(), [ts_agent_port().await]);
    assert_eq!(
        effective_mode(OrchestratorMode::Claim, &guard).await,
        OrchestratorMode::Dry
    );
    let quiet = PortGuard::new(closed_port(), [closed_port()]);
    assert_eq!(
        effective_mode(OrchestratorMode::Claim, &quiet).await,
        OrchestratorMode::Claim
    );
    assert_eq!(
        effective_mode(OrchestratorMode::Dry, &guard).await,
        OrchestratorMode::Dry
    );
}

#[tokio::test]
async fn a_claim_mode_tick_still_only_lists_with_or_without_the_guard_blocking() {
    for blocked in [true, false] {
        let (base, asked) = stub().await;
        let mut rig = rig(2, &[&base]);
        let port = if blocked { ts_agent_port().await } else { closed_port() };
        let lp = rig.lp(0);
        lp.guard = Some(PortGuard::new(closed_port(), [port]));
        lp.mode = OrchestratorMode::Claim;
        assert_eq!(
            rig.machine.pass().await,
            vec![Step::Ticked {
                would: vec!["ask_for_work".into()]
            }]
        );
        assert_eq!(*asked.lock().unwrap(), vec!["list".to_string()]);
    }
}

/// A session starter that records what it was asked to start and step, and can panic once, refuse, or end each
/// session at once with the next scripted result.
#[derive(Default)]
struct Fake {
    started: Mutex<Vec<StartRequest>>,
    stepped: Mutex<Vec<StepRequest>>,
    panic_once: AtomicBool,
    refuse: Mutex<Option<String>>,
    script: Mutex<Vec<SessionResult>>,
    /// Sessions it says are running.
    active: AtomicU32,
    /// Where a step leaves its item; `None` keeps its state.
    to: Mutex<Option<ItemState>>,
    /// While set, a step never ends.
    hold: AtomicBool,
    /// The next step panics.
    step_panics: AtomicBool,
}

impl Fake {
    fn step(&self, mut run: ItemRun) -> BoxFuture<'static, ItemRun> {
        if let Some(to) = *self.to.lock().unwrap() {
            run.state = to;
        }
        let (hold, panics) = (
            self.hold.load(Ordering::SeqCst),
            self.step_panics.swap(false, Ordering::SeqCst),
        );
        async move {
            if hold {
                std::future::pending::<()>().await;
            }
            if panics {
                panic!("a step that goes wrong");
            }
            run
        }
        .boxed()
    }
}

impl SessionStarter for Fake {
    fn start(&self, request: StartRequest) -> Result<(), String> {
        self.started.lock().unwrap().push(request.clone());
        if self.panic_once.swap(false, Ordering::SeqCst) {
            panic!("a starter that goes wrong");
        }
        if let Some(why) = self.refuse.lock().unwrap().clone() {
            return Err(why);
        }
        let mut script = self.script.lock().unwrap();
        if !script.is_empty() {
            request.outcomes.send(SessionOutcome {
                project: request.project,
                item_id: request.item_id,
                lane: request.lane,
                result: script.remove(0),
            });
        }
        Ok(())
    }

    fn active(&self) -> u32 {
        self.active.load(Ordering::SeqCst)
    }

    fn advance(&self, request: StepRequest) -> Option<BoxFuture<'static, ItemRun>> {
        let run = request.run;
        self.stepped.lock().unwrap().push(request);
        Some(self.step(run))
    }
}

type Asked = Arc<Mutex<Vec<(String, Value)>>>;

/// A store that answers `list` with `listed` and `claim_next` with `claim` (a value) or, for `None`, an error.
/// A handed-out item is handed out once: every later `claim_next` answers nothing.
async fn claim_stub(listed: Value, claim: Option<Value>) -> (String, Asked) {
    claim_stub_with(listed, claim, Arc::new(AtomicBool::new(false))).await
}

/// `claim_stub` that refuses `release` and `park` with a 500 while `fail_release` is set.
async fn claim_stub_with(listed: Value, claim: Option<Value>, fail_release: Arc<AtomicBool>) -> (String, Asked) {
    use axum::http::StatusCode;
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let served = Arc::new(AtomicBool::new(false));
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, listed, claim, served) = (seen.clone(), listed.clone(), claim.clone(), served.clone());
            let fail_release = fail_release.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                match (op.as_str(), claim) {
                    ("release" | "park", _) if fail_release.load(Ordering::SeqCst) => {
                        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" })))
                    }
                    ("release" | "park", _) => (StatusCode::OK, Json(json!({}))),
                    ("claim_next", None) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" }))),
                    ("claim_next", Some(_)) if served.load(Ordering::SeqCst) => {
                        (StatusCode::OK, Json(json!({ "value": null })))
                    }
                    ("claim_next", Some(v)) => {
                        served.store(!v.is_null(), Ordering::SeqCst);
                        (StatusCode::OK, Json(json!({ "value": v })))
                    }
                    _ => (StatusCode::OK, Json(json!({ "value": listed }))),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

fn item(id: &str, status: &str) -> Value {
    json!({ "id": id, "title": id.to_uppercase(), "status": status, "rank": 0 })
}

fn ops(asked: &Asked) -> Vec<String> {
    asked.lock().unwrap().iter().map(|(op, _)| op.clone()).collect()
}

fn kinds(rig: &mut Rig) -> Vec<String> {
    rig.lp(0).events().recent().iter().map(|e| e.kind()).collect()
}

fn handed_out() -> Option<Value> {
    Some(json!({ "item": item("a", "claimed"), "token": "lease-1" }))
}

#[tokio::test]
async fn claim_mode_claims_while_a_lane_is_free_queues_then_starts_and_holds_the_lane() {
    let (base, asked) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    let steps = rig.machine.pass().await;
    assert_eq!(would(&steps[0]), ["ask_for_work"]);
    assert_eq!(
        ops(&asked),
        ["list", "claim_next", "claim_next"],
        "the second lane was still free after the first claim started, so the store was asked again"
    );
    let agent = asked.lock().unwrap()[1].1["agent"].clone();
    assert_eq!(
        agent,
        json!({ "machine": "my-pc", "environment": "retaskable.com", "baseBranch": "" })
    );
    let session_id = fake.started.lock().unwrap()[0].session_id.clone();
    assert_eq!(
        *fake.started.lock().unwrap(),
        vec![StartRequest {
            project: "p0".into(),
            item_id: "a".into(),
            lane: 1,
            token: "lease-1".into(),
            item: rig.lp(0).items["a"].clone(),
            session_id,
            resume: false,
            outcomes: rig.machine.outcome_sender(),
        }]
    );
    assert_eq!(
        rig.lp(0).runs["a"].state,
        crate::item_lifecycle::ItemState::Implementing
    );
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
    let held = rig.live.snapshot().projects["p0"].held.clone();
    assert_eq!(held.len(), 1);
    assert_eq!(
        (held[0].lane, held[0].item_id.as_str(), held[0].title.as_deref()),
        (1, "a", Some("A"))
    );
    assert_eq!(
        kinds(&mut rig),
        ["lease", "lane_taken", "session_start", "claim", "claim", "tick"]
    );
    assert_eq!(rig.lp(0).events().recent()[3].to_value()["answer"], "a");
    assert_eq!(rig.lp(0).events().recent()[4].to_value()["answer"], "none");
    assert_eq!(
        rig.lp(0).events().recent()[5].to_value()["claim"],
        "none",
        "the tick shows the last answer"
    );
    assert_eq!(
        rig.lp(0).store.leases()["a"],
        "lease-1",
        "the lease is kept for a restart"
    );
}

#[tokio::test]
async fn claim_mode_without_a_starter_sends_no_claim() {
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).mode = OrchestratorMode::Claim;
    rig.machine.pass().await;
    assert_eq!(ops(&asked), ["list"]);
    assert_eq!(rig.machine.pool.held_lanes().count(), 0);
}

#[tokio::test]
async fn a_guard_block_means_no_claim_next() {
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    rig.lp(0).guard = Some(PortGuard::new(closed_port(), [ts_agent_port().await]));
    assert!(matches!(rig.machine.pass().await[..], [Step::Ticked { .. }]));
    assert_eq!(ops(&asked), ["list"]);
    assert!(fake.started.lock().unwrap().is_empty() && rig.lp(0).runs.is_empty());
}

#[tokio::test]
async fn a_store_error_on_claim_is_logged_and_never_read_as_nothing() {
    let (base, asked) = claim_stub(json!([]), None).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    rig.machine.pass().await;
    assert_eq!(ops(&asked), ["list", "claim_next"]);
    assert_eq!(kinds(&mut rig), ["store_error", "claim", "tick"]);
    assert_eq!(rig.lp(0).events().recent()[0].to_value()["op"], "claim_next");
    assert_eq!(rig.lp(0).events().recent()[1].to_value()["answer"], "failed");
    assert!(rig.lp(0).runs.is_empty() && rig.machine.pool.held_lanes().count() == 0);
    assert!(fake.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_restart_takes_back_the_items_it_still_holds_and_drops_the_rest() {
    let listed = json!([item("a", "building"), item("b", "on_main")]);
    let (base, asked) = claim_stub(listed, Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    let held = [("a", "old"), ("b", "older"), ("gone", "x")];
    rig.lp(0).leases = held.iter().map(|(i, t)| (i.to_string(), t.to_string())).collect();
    rig.machine.pass().await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(
        (started.len(), started[0].item_id.as_str(), started[0].token.as_str()),
        (1, "a", "old")
    );
    assert_eq!(rig.lp(0).store.leases().keys().collect::<Vec<_>>(), ["a"]);
    assert_eq!(ops(&asked), ["list", "claim_next"], "the other lane may still claim");
    assert_eq!(rig.lp(0).events().recent().last().unwrap().to_value()["claim"], "none");
}

#[tokio::test]
async fn a_panic_in_the_pass_is_caught_and_the_next_pass_runs() {
    let (base, asked) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    fake.panic_once.store(true, Ordering::SeqCst);
    assert_eq!(rig.machine.pass().await, vec![Step::Skipped]);
    assert!(matches!(rig.machine.pass().await[..], [Step::Ticked { .. }]));
    assert_eq!(ops(&asked).iter().filter(|o| *o == "list").count(), 2);
}

/// Lets the steps spawned so far run to their end (the test runtime is single-threaded).
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

/// Gives project `i` an item `id` in `state`, holding `lane` when given.
fn at(rig: &mut Rig, i: usize, id: &str, state: ItemState, lane: Option<u32>) {
    let mut run = ItemRun::new();
    run.state = state;
    rig.lp(i).runs.insert(id.into(), run);
    if let Some(lane) = lane {
        let holder = Holder::Item {
            project: format!("p{i}"),
            item: id.into(),
        };
        assert!(rig.machine.pool.take(lane, holder));
    }
}

fn kinds_without_claims(rig: &mut Rig) -> Vec<String> {
    kinds(rig).into_iter().filter(|k| k != "claim").collect()
}

#[tokio::test]
async fn a_finished_step_moves_the_item_and_frees_a_lane_it_no_longer_needs() {
    let (base, asked) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    at(&mut rig, 0, "a", ItemState::Reviewing, Some(1));
    *fake.to.lock().unwrap() = Some(ItemState::AwaitingMerge);
    rig.machine.poll().await;
    let stepped = fake.stepped.lock().unwrap().clone();
    assert_eq!(
        (stepped.len(), stepped[0].lane, stepped[0].run.state),
        (1, Some(1), ItemState::Reviewing)
    );
    assert_eq!(rig.lp(0).events().recent()[0].to_value()["step"], "reviewing");
    settle().await;
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::AwaitingMerge);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), None);
    assert_eq!(
        kinds_without_claims(&mut rig),
        ["step_start", "item_state", "lane_freed", "step_start"],
        "awaiting_merge has a step of its own"
    );
    let moved = rig.lp(0).events().recent()[2].to_value();
    assert_eq!(
        (moved["from"].as_str(), moved["to"].as_str()),
        (Some("reviewing"), Some("awaiting_merge"))
    );
    assert!(
        !ops(&asked).contains(&"list".to_string()),
        "a poll never reads the backlog"
    );
}

#[tokio::test]
async fn an_item_gets_one_step_at_a_time() {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    at(&mut rig, 0, "a", ItemState::Checking, Some(1));
    fake.hold.store(true, Ordering::SeqCst);
    for _ in 0..3 {
        rig.machine.poll().await;
        settle().await;
    }
    assert_eq!(fake.stepped.lock().unwrap().len(), 1);
    assert_eq!(
        rig.machine.pool.lane_of("p0", "a"),
        Some(1),
        "a running step keeps its lane"
    );
}

#[tokio::test]
async fn a_paused_project_isnt_advanced_or_claimed_for() {
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    at(&mut rig, 0, "a", ItemState::AwaitingCi, None);
    rig.pause("p0", true);
    rig.machine.poll().await;
    assert!(fake.stepped.lock().unwrap().is_empty());
    assert!(ops(&asked).is_empty());
    rig.pause("p0", false);
    rig.machine.poll().await;
    assert_eq!(fake.stepped.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_panicking_step_is_an_error_event_and_the_item_keeps_its_run() {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    at(&mut rig, 0, "a", ItemState::Checking, Some(1));
    fake.step_panics.store(true, Ordering::SeqCst);
    *fake.to.lock().unwrap() = Some(ItemState::AwaitingMerge);
    rig.machine.poll().await;
    settle().await;
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Checking);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
    let error = (rig.lp(0).events().recent().iter())
        .map(|e| e.to_value())
        .find(|e| e["where"] == "step")
        .expect("an error event");
    assert_eq!(error["item"], "a");
    assert_eq!(
        fake.stepped.lock().unwrap().len(),
        2,
        "the next poll tries the step again"
    );
}

#[tokio::test]
async fn concurrency_caps_starts_and_claims() {
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    rig.machine.concurrency = 1;
    rig.queue(0, &["q"]);
    fake.active.store(1, Ordering::SeqCst);
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty(), "no session slot left");
    assert!(ops(&asked).is_empty(), "and no claim");
    fake.active.store(0, Ordering::SeqCst);
    rig.machine.poll().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert!(ops(&asked).is_empty(), "the one slot went to the queued item");
}

#[tokio::test]
async fn a_do_next_item_starts_without_a_session_slot_and_takes_none() {
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(3, &[&base]);
    let fake = rig.go_live();
    rig.machine.concurrency = 1;
    rig.queue(0, &["plain", "rush"]);
    rig.lp(0).items.get_mut("rush").unwrap().do_next = true;
    fake.active.store(1, Ordering::SeqCst);
    rig.machine.poll().await;
    let started: Vec<_> = fake.started.lock().unwrap().iter().map(|r| r.item_id.clone()).collect();
    assert_eq!(started, ["rush"], "only the do-next item skips the full slots");
    assert!(ops(&asked).is_empty(), "and nothing is claimed without a slot");
    fake.active.store(0, Ordering::SeqCst);
    rig.machine.poll().await;
    assert_eq!(
        fake.started.lock().unwrap().len(),
        2,
        "the rush start took no slot from the plain item"
    );
}

#[tokio::test]
async fn the_fetch_runs_for_live_projects_and_a_failure_is_an_error_event() {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base, &base]);
    let repos = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repo_with_origin(repos.path());
    rig.lp(0).project.repo_path = repo.to_string_lossy().into_owned();
    rig.lp(1).project.repo_path = repos.path().join("missing").to_string_lossy().into_owned();
    rig.machine.fetch().await;
    assert!(rig.lp(1).events().recent().is_empty(), "a dry project isn't fetched");
    rig.go_live();
    rig.pause("p0", true);
    rig.machine.fetch().await;
    assert!(
        rig.lp(0).events().recent().is_empty(),
        "a paused project is fetched too, and it worked"
    );
    let errors: Vec<_> = rig.lp(1).events().recent().iter().map(|e| e.to_value()).collect();
    assert_eq!((errors.len(), errors[0]["where"].as_str()), (1, Some("fetch")));
}

mod reporting;
mod restart;
mod settling;
