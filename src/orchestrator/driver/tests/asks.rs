//! The ask step (`driver/asks.rs`): a fake store and a fake host whose assistant answer is scripted.
use super::*;
use crate::ask::model::{Action, AssistantResult};
use crate::ask::prompt::select_exchanges;
use crate::commands::{Command, Seen};
use crate::engines::EngineId;
use crate::orchestrator::driver::{AskHost, AskJob, AskRig};
use crate::orchestrator::fresh_start::EngineLimits;
use crate::view::{AgentView, AskCommand};

#[derive(Default)]
struct Host {
    jobs: Mutex<Vec<AskJob>>,
    result: Mutex<Option<AssistantResult>>,
    hold: Arc<AtomicBool>,
    spent: Mutex<f64>,
}

impl AskHost for Host {
    fn run(&self, job: AskJob) -> BoxFuture<'static, Result<AssistantResult>> {
        self.jobs.lock().unwrap().push(job);
        let result = self.result.lock().unwrap().clone().unwrap_or(AssistantResult {
            answer: "Because.".into(),
            actions: vec![],
            dropped: vec![],
        });
        let hold = self.hold.clone();
        async move {
            while hold.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(result)
        }
        .boxed()
    }
    fn spent_today_usd(&self) -> f64 {
        *self.spent.lock().unwrap()
    }
}

/// What the fake store answers: the store's pause, and whether `asks_list` fails.
#[derive(Clone, Default)]
struct Ctl {
    paused: Arc<AtomicBool>,
    fail: Arc<AtomicBool>,
}

async fn store(asks: Value, ctl: Ctl) -> (String, Asked) {
    let asked: Asked = Default::default();
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, asks, ctl) = (seen.clone(), asks.clone(), ctl.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                if op == "asks_list" && ctl.fail.load(Ordering::SeqCst) {
                    return (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({ "error": "down" })),
                    );
                }
                let value = match op.as_str() {
                    "asks_list" => json!({ "asks": asks }),
                    "facts_get" => json!({ "audiences": [{ "id": "devs", "name": "Developers" }] }),
                    "list" => json!([item("a", "building")]),
                    "drafts" => json!([]),
                    "pause_get" if ctl.paused.load(Ordering::SeqCst) => json!({ "paused": { "by": "ada" } }),
                    "pause_get" => json!({ "paused": null }),
                    _ => Value::Null,
                };
                (axum::http::StatusCode::OK, Json(json!({ "value": value })))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

struct Setup {
    rig: Rig,
    fake: Arc<Fake>,
    host: Arc<Host>,
    asked: Asked,
    ctl: Ctl,
    view: Arc<AgentView>,
}

async fn setup(asks: Value) -> Setup {
    let ctl = Ctl::default();
    let (base, asked) = store(asks, ctl.clone()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["ask-claude"]));
    let view = Arc::new(AgentView {
        machine: "my-pc".into(),
        tags: vec![],
        projects: vec![],
        version: "9.9.9".into(),
        lanes: 2,
        chore_lane: None,
        lane_sizes: Default::default(),
        paused: rig.machine.links.paused.clone(),
        item_commands: rig.machine.links.item_commands.clone(),
        work: Default::default(),
        restart: Default::default(),
    });
    view.item_commands.set_claim(true);
    let host = Arc::new(Host::default());
    rig.machine.asks = Some(AskRig {
        host: host.clone(),
        view: view.clone(),
        daily_usd: 5.0,
    });
    Setup {
        rig,
        fake,
        host,
        asked,
        ctl,
        view,
    }
}

fn ask(s: &Setup, id: &str, reply_to: Option<&str>) {
    s.view.item_commands.push_ask(AskCommand {
        project: "p0".into(),
        ask_id: id.into(),
        text: format!("question {id}"),
        reply_to: reply_to.map(Into::into),
        about: None,
        images: String::new(),
    });
}

fn reports(asked: &Asked) -> Vec<Value> {
    let all = asked.lock().unwrap();
    all.iter()
        .filter(|(op, _)| op == "ask_report")
        .map(|(_, a)| a.clone())
        .collect()
}

/// Polls until `n` reports have gone to the store.
async fn until(s: &mut Setup, n: usize) -> Vec<Value> {
    for _ in 0..200 {
        s.rig.machine.poll().await;
        if reports(&s.asked).len() >= n {
            return reports(&s.asked);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the ask never reported {n} times: {:?}", reports(&s.asked));
}

/// A few polls with nothing expected to come of them.
async fn idle(s: &mut Setup) {
    for _ in 0..5 {
        s.rig.machine.poll().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn waiting(id: &str) -> Value {
    json!({ "id": id, "waiting": "Paused: I'll answer when someone resumes" })
}

#[tokio::test]
async fn an_ask_command_acks_and_is_answered_with_the_answer_and_each_actions_result() {
    let mut s = setup(json!([])).await;
    *s.host.result.lock().unwrap() = Some(AssistantResult {
        answer: "Paused and retried.".into(),
        actions: vec![Action::Pause, Action::Retry { id: "a".into() }],
        dropped: vec!["no such action".into()],
    });
    let now = crate::telemetry::events::now_ms();
    let cmd = Command {
        id: "c1".into(),
        name: "ask".into(),
        args: json!({ "askId": "a1", "text": "pause and retry a" }),
        issued_by: "ada".into(),
        issued_at: now,
        expires_at: now + 60_000,
    };
    let acked = crate::commands::run(&cmd, "p0", &mut Seen::default(), &s.view, &|_| {}, s._dir());
    assert_eq!(acked, json!({ "type": "ack", "id": "c1" }));
    let sent = until(&mut s, 1).await;
    assert_eq!(sent[0]["id"], "a1");
    assert_eq!(sent[0]["status"], "done");
    assert_eq!(sent[0]["answer"], "Paused and retried.");
    assert_eq!(
        sent[0]["actions"],
        json!([
            { "type": "pause", "ok": true },
            { "type": "retry", "ok": true },
            { "type": "dropped", "ok": false, "reason": "no such action" },
        ])
    );
    assert!(ops(&s.asked).contains(&"facts_get".to_string()));
    let jobs = s.host.jobs.lock().unwrap();
    assert_eq!(jobs[0].question, "pause and retry a");
    assert_eq!(jobs[0].audiences[0].id, "devs");
    assert_eq!(jobs[0].state.items[0].id, "a");
}

impl Setup {
    fn _dir(&self) -> &std::path::Path {
        self.rig._dir.path()
    }
}

#[tokio::test]
async fn a_reply_builds_its_prompt_from_its_thread() {
    let asks = json!([
        { "id": "a0", "text": "first", "answer": "one" },
        { "id": "a1", "text": "second", "answer": "two", "replyTo": "a0" },
        { "id": "a2", "text": "unrelated", "answer": "three" },
        { "id": "a3", "text": "question a3" },
    ]);
    let mut s = setup(asks).await;
    ask(&s, "a3", Some("a1"));
    until(&mut s, 1).await;
    let jobs = s.host.jobs.lock().unwrap();
    assert_eq!(jobs[0].reply_to.as_deref(), Some("a1"));
    assert!(jobs[0].asks.iter().all(|a| a.id != "a3"), "not itself");
    let ids: Vec<_> = select_exchanges(&jobs[0].asks, jobs[0].reply_to.as_deref())
        .iter()
        .map(|a| a.id.as_str())
        .collect();
    assert_eq!(ids, ["a0", "a1"]);
}

#[tokio::test]
async fn an_ask_runs_with_every_slot_busy_and_holds_one_while_it_runs() {
    let mut s = setup(json!([])).await;
    s.rig.machine.concurrency = 1;
    s.fake.active.store(1, Ordering::SeqCst);
    s.rig.queue(0, &["q"]);
    s.host.hold.store(true, Ordering::SeqCst);
    ask(&s, "a1", None);
    s.rig.machine.poll().await;
    for _ in 0..50 {
        if !s.host.jobs.lock().unwrap().is_empty() {
            break;
        }
        s.rig.machine.poll().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(s.host.jobs.lock().unwrap().len(), 1, "started with no slot free");
    // The session slot is the ask's while it runs: the queued item waits, even with the starter idle.
    s.fake.active.store(0, Ordering::SeqCst);
    s.rig.machine.poll().await;
    assert!(s.fake.started.lock().unwrap().is_empty());
    s.host.hold.store(false, Ordering::SeqCst);
    until(&mut s, 1).await;
    for _ in 0..50 {
        s.rig.machine.poll().await;
        if !s.fake.started.lock().unwrap().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the item never started once the ask ended");
}

/// Asks while `block` holds: waits, reports so once, runs nothing; then `lift` and it's answered.
async fn waits_then_answers(mut s: Setup, block: impl FnOnce(&mut Setup), lift: impl FnOnce(&mut Setup)) {
    block(&mut s);
    ask(&s, "a1", None);
    until(&mut s, 1).await;
    idle(&mut s).await;
    assert_eq!(reports(&s.asked), vec![waiting("a1")], "told once");
    assert!(s.host.jobs.lock().unwrap().is_empty());
    lift(&mut s);
    let sent = until(&mut s, 2).await;
    assert_eq!(sent[1]["answer"], "Because.");
    assert_eq!(s.host.jobs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_paused_computer_holds_an_ask_until_resumed() {
    waits_then_answers(
        setup(json!([])).await,
        |s| s.rig.pause("p0", true),
        |s| s.rig.pause("p0", false),
    )
    .await;
}

#[tokio::test]
async fn a_project_paused_on_the_store_holds_an_ask_until_resumed() {
    let ctl = |s: &Setup| s.ctl.paused.clone();
    waits_then_answers(
        setup(json!([])).await,
        |s| ctl(s).store(true, Ordering::SeqCst),
        |s| ctl(s).store(false, Ordering::SeqCst),
    )
    .await;
}

#[tokio::test]
async fn going_over_the_daily_budget_holds_an_ask_until_there_is_room() {
    waits_then_answers(
        setup(json!([])).await,
        |s| *s.host.spent.lock().unwrap() = 5.0,
        |s| *s.host.spent.lock().unwrap() = 1.0,
    )
    .await;
}

#[tokio::test]
async fn the_usage_limit_holds_an_ask_until_it_lifts() {
    waits_then_answers(
        setup(json!([])).await,
        |s| {
            s.rig
                .machine
                .engine_limits
                .hit(EngineId::Claude, std::time::Instant::now() + Duration::from_secs(3600))
        },
        |s| s.rig.machine.engine_limits = EngineLimits::default(),
    )
    .await;
}

#[tokio::test]
async fn a_store_that_cant_be_read_leaves_the_ask_unanswered_until_it_can() {
    let mut s = setup(json!([])).await;
    s.ctl.fail.store(true, Ordering::SeqCst);
    ask(&s, "a1", None);
    idle(&mut s).await;
    assert!(s.host.jobs.lock().unwrap().is_empty());
    assert!(reports(&s.asked).is_empty(), "nothing answered, nothing said");
    s.ctl.fail.store(false, Ordering::SeqCst);
    let sent = until(&mut s, 1).await;
    assert_eq!(sent[0]["answer"], "Because.");
}

#[tokio::test]
async fn with_the_flag_off_an_ask_is_never_answered() {
    let mut s = setup(json!([])).await;
    s.rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&[]));
    ask(&s, "a1", None);
    idle(&mut s).await;
    assert!(s.host.jobs.lock().unwrap().is_empty());
    assert!(reports(&s.asked).is_empty());
}
