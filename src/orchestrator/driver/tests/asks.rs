//! The ask step (`driver/asks.rs`): a fake store and a fake host whose assistant answers are scripted.
use super::*;
use crate::ask::model::{Action, AssistantResult};
use crate::ask::prompt::build_prompt;
use crate::ask::session::TEMPLATE;
use crate::orchestrator::driver::{AskHost, AskJob, Spend};
use crate::view::{AgentView, Ask};

#[derive(Default)]
struct Host {
    jobs: Mutex<Vec<AskJob>>,
    hold: AtomicBool,
    spend: Mutex<Spend>,
    actions: Mutex<Vec<Action>>,
}

impl AskHost for Host {
    fn run(&self, job: AskJob) -> BoxFuture<'static, Result<AssistantResult>> {
        self.jobs.lock().unwrap().push(job);
        let hold = self.hold.load(Ordering::SeqCst);
        let actions = self.actions.lock().unwrap().clone();
        async move {
            if hold {
                std::future::pending::<()>().await;
            }
            Ok(AssistantResult {
                answer: "Step 5 waits for a person.".into(),
                actions,
                dropped: vec!["unknown action \"bogus\"".into()],
            })
        }
        .boxed()
    }

    fn spend(&self) -> Spend {
        *self.spend.lock().unwrap()
    }
}

/// What the fake store answers: the project's pause and whether `asks_list` fails.
#[derive(Default)]
struct Switches {
    project_paused: AtomicBool,
    asks_fail: AtomicBool,
}

async fn store(switches: Arc<Switches>) -> (String, Asked) {
    let asked: Asked = Default::default();
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, switches) = (seen.clone(), switches.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                if op == "asks_list" && switches.asks_fail.load(Ordering::SeqCst) {
                    return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" })));
                }
                let value = match op.as_str() {
                    "asks_list" => json!({ "asks": [
                        { "id": "a0", "text": "what is lane 1?", "answer": "An item's lane." },
                        { "id": "a9", "text": "unrelated", "answer": "Other." },
                        { "id": "a1", "text": "and lane 2?" }
                    ] }),
                    "facts_get" => json!({ "audiences": [] }),
                    "pause_get" => json!({ "paused": switches.project_paused.load(Ordering::SeqCst).then(|| json!({ "by": "ada" })) }),
                    "list" | "drafts" => json!([]),
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
    host: Arc<Host>,
    asked: Asked,
    switches: Arc<Switches>,
}

async fn setup() -> Setup {
    let switches = Arc::new(Switches::default());
    let (base, asked) = store(switches.clone()).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["ask-claude"]));
    let host = Arc::new(Host::default());
    let view = AgentView {
        machine: "pc".into(),
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
    };
    rig.machine.asks = Some((host.clone(), view));
    Setup {
        rig,
        host,
        asked,
        switches,
    }
}

fn send(s: &Setup, id: &str, reply_to: Option<&str>) {
    s.rig.machine.links.item_commands.push_ask(Ask {
        project: "p0".into(),
        id: id.into(),
        text: "why is step 5 stuck?".into(),
        reply_to: reply_to.map(String::from),
        about: None,
        images_block: String::new(),
    });
}

fn reports(asked: &Asked) -> Vec<Value> {
    (asked.lock().unwrap().iter())
        .filter(|(op, _)| op == "ask_report")
        .map(|(_, a)| a.clone())
        .collect()
}

/// Passes until `n` ask reports are in.
async fn until_reports(s: &mut Setup, n: usize) -> Vec<Value> {
    for _ in 0..200 {
        s.rig.machine.pass().await;
        if reports(&s.asked).len() >= n {
            return reports(&s.asked);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never {n} ask reports: {:?}", ops(&s.asked));
}

#[tokio::test]
async fn an_ask_is_answered_with_the_answer_and_each_actions_result() {
    let mut s = setup().await;
    *s.host.actions.lock().unwrap() = vec![Action::Pause, Action::Retry { id: "".into() }];
    send(&s, "a1", None);
    let sent = until_reports(&mut s, 1).await;
    assert_eq!(sent[0]["id"], "a1");
    assert_eq!(sent[0]["answer"], "Step 5 waits for a person.");
    assert_eq!(sent[0]["status"], "done");
    let results = sent[0]["actions"].as_array().unwrap();
    assert_eq!(results[0], json!({ "type": "pause", "ok": true }));
    assert_eq!(results[1]["type"], "retry");
    assert_eq!(results[1]["ok"], false);
    assert_eq!(results[2]["type"], "dropped");
    assert!(
        s.rig.machine.links.paused.reason("p0").is_some(),
        "the pause action ran"
    );
}

#[tokio::test]
async fn a_new_asks_prompt_has_the_last_exchanges_and_a_replys_has_its_thread() {
    let mut s = setup().await;
    send(&s, "a1", None);
    until_reports(&mut s, 1).await;
    let prompt = |job: &AskJob| build_prompt(TEMPLATE, &job.input()).unwrap();
    let first = prompt(&s.host.jobs.lock().unwrap()[0]);
    assert!(
        first.contains("what is lane 1?") && first.contains("unrelated"),
        "{first}"
    );
    assert!(
        !first.contains("and lane 2?"),
        "the ask itself is the question, not history"
    );

    send(&s, "a2", Some("a0"));
    until_reports(&mut s, 2).await;
    let reply = prompt(&s.host.jobs.lock().unwrap()[1]);
    assert!(
        reply.contains("what is lane 1?") && reply.contains("An item's lane."),
        "{reply}"
    );
    assert!(
        !reply.contains("unrelated"),
        "an ask in between is not in the thread: {reply}"
    );
}

#[tokio::test]
async fn an_ask_runs_with_every_slot_busy_and_counts_as_a_running_session() {
    let mut s = setup().await;
    s.rig.machine.concurrency = 0;
    s.host.hold.store(true, Ordering::SeqCst);
    send(&s, "a1", None);
    for _ in 0..50 {
        s.rig.machine.pass().await;
        if !s.host.jobs.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(s.host.jobs.lock().unwrap().len(), 1, "no slot was needed to start");
    assert_eq!(s.rig.lp(0).answering(), 1, "it counts while it runs");
    assert!(reports(&s.asked).is_empty());
}

fn waiting_report(sent: &[Value]) -> bool {
    sent[0]["status"] == "waiting" && sent[0]["waiting"] == "Paused: I'll answer when someone resumes"
}

#[tokio::test]
async fn while_this_computer_is_paused_it_waits_and_is_answered_on_resume() {
    let mut s = setup().await;
    s.rig.pause("p0", true);
    send(&s, "a1", None);
    let sent = until_reports(&mut s, 1).await;
    assert!(waiting_report(&sent), "{sent:?}");
    for _ in 0..3 {
        s.rig.machine.pass().await;
    }
    assert_eq!(reports(&s.asked).len(), 1, "told once");
    assert!(s.host.jobs.lock().unwrap().is_empty());
    s.rig.pause("p0", false);
    let sent = until_reports(&mut s, 2).await;
    assert_eq!(sent[1]["answer"], "Step 5 waits for a person.");
}

#[tokio::test]
async fn while_the_project_is_paused_in_the_store_it_waits_and_is_answered_on_resume() {
    let mut s = setup().await;
    s.switches.project_paused.store(true, Ordering::SeqCst);
    send(&s, "a1", None);
    let sent = until_reports(&mut s, 1).await;
    assert!(waiting_report(&sent), "{sent:?}");
    assert!(s.host.jobs.lock().unwrap().is_empty());
    s.switches.project_paused.store(false, Ordering::SeqCst);
    assert_eq!(until_reports(&mut s, 2).await[1]["status"], "done");
}

#[tokio::test]
async fn over_the_daily_budget_it_waits_and_is_answered_once_under_it() {
    let mut s = setup().await;
    *s.host.spend.lock().unwrap() = Spend {
        today_usd: 10.0,
        daily_usd: 10.0,
    };
    send(&s, "a1", None);
    assert!(waiting_report(&until_reports(&mut s, 1).await));
    *s.host.spend.lock().unwrap() = Spend::default();
    assert_eq!(until_reports(&mut s, 2).await[1]["status"], "done");
}

#[tokio::test]
async fn at_the_usage_limit_it_waits_and_is_answered_after() {
    let mut s = setup().await;
    let until = std::time::Instant::now() + Duration::from_millis(300);
    s.rig.machine.engine_limits.hit(crate::engines::EngineId::Claude, until);
    send(&s, "a1", None);
    assert!(waiting_report(&until_reports(&mut s, 1).await));
    assert!(s.host.jobs.lock().unwrap().is_empty());
    assert_eq!(until_reports(&mut s, 2).await[1]["status"], "done");
}

#[tokio::test]
async fn a_store_error_leaves_the_ask_unanswered_and_it_is_answered_once_the_store_is_back() {
    let mut s = setup().await;
    s.switches.asks_fail.store(true, Ordering::SeqCst);
    send(&s, "a1", None);
    for _ in 0..3 {
        s.rig.machine.pass().await;
    }
    assert!(s.host.jobs.lock().unwrap().is_empty(), "nothing is answered locally");
    assert!(reports(&s.asked).is_empty());
    s.switches.asks_fail.store(false, Ordering::SeqCst);
    assert_eq!(until_reports(&mut s, 1).await[0]["status"], "done");
}

#[tokio::test]
async fn nothing_is_taken_while_the_ask_claude_flag_is_off() {
    let mut s = setup().await;
    s.rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&[]));
    send(&s, "a1", None);
    for _ in 0..3 {
        s.rig.machine.pass().await;
    }
    assert!(s.host.jobs.lock().unwrap().is_empty());
    assert!(!ops(&s.asked).contains(&"asks_list".to_string()));
}
