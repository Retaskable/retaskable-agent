//! The ask step (`driver/asks.rs`): a fake store that records every op and a fake host whose answer is scripted.
use super::*;
use crate::engines::EngineId;
use crate::orchestrator::driver::{AskHost, AskJob, AskReply, AskStore, WAITING};
use crate::view::AskCommand;

#[derive(Default)]
struct Host {
    /// `blocked` answers: the project's pause or the daily budget.
    blocked: AtomicBool,
    /// The store can't be read.
    broken: AtomicBool,
    hold: AtomicBool,
    /// The session fails: the host answers that it couldn't.
    crashed: AtomicBool,
    jobs: Mutex<Vec<AskCommand>>,
}

impl AskHost for Host {
    fn blocked(&self, _: &AskStore) -> BoxFuture<'static, Result<bool>> {
        let (blocked, broken) = (self.blocked.load(Ordering::SeqCst), self.broken.load(Ordering::SeqCst));
        async move {
            if broken {
                anyhow::bail!("the store is down");
            }
            Ok(blocked)
        }
        .boxed()
    }
    fn run(&self, job: AskJob) -> BoxFuture<'static, Result<AskReply>> {
        self.jobs.lock().unwrap().push(job.ask.clone());
        let (broken, hold) = (self.broken.load(Ordering::SeqCst), self.hold.load(Ordering::SeqCst));
        let crashed = self.crashed.load(Ordering::SeqCst);
        async move {
            if crashed {
                return Ok(AskReply {
                    answer: "Sorry, I couldn't answer that: claude crashed".into(),
                    actions: vec![],
                });
            }
            if hold {
                std::future::pending::<()>().await;
            }
            if broken {
                anyhow::bail!("the store is down");
            }
            Ok(AskReply {
                answer: format!("Answer to {}", job.ask.text),
                actions: vec![json!({ "type": "pause", "ok": true })],
            })
        }
        .boxed()
    }
}

async fn store() -> (String, Asked) {
    let asked: Asked = Default::default();
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                Json(json!({ "value": if op == "list" { json!([]) } else { Value::Null } }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

async fn rig_with_host() -> (Rig, Arc<Host>, Asked) {
    let (base, asked) = store().await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["ask-claude"]));
    let host = Arc::new(Host::default());
    rig.machine.asks = Some(host.clone());
    (rig, host, asked)
}

fn ask(id: &str, text: &str) -> AskCommand {
    AskCommand {
        project: "p0".into(),
        ask_id: id.into(),
        text: text.into(),
        reply_to: None,
        about: None,
        images_block: String::new(),
    }
}

fn push(rig: &Rig, a: AskCommand) {
    rig.machine.links.item_commands.push_ask(a);
}

fn reports(asked: &Asked) -> Vec<Value> {
    (asked.lock().unwrap().iter())
        .filter(|(op, _)| op == "ask_report")
        .map(|(_, a)| a.clone())
        .collect()
}

/// Passes until the store has `n` ask reports.
async fn until(rig: &mut Rig, asked: &Asked, n: usize) -> Vec<Value> {
    for _ in 0..200 {
        rig.machine.pass().await;
        let sent = reports(asked);
        if sent.len() >= n {
            return sent;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the store never got {n} ask reports: {:?}", reports(asked));
}

async fn quiet_passes(rig: &mut Rig) {
    for _ in 0..8 {
        rig.machine.pass().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn an_ask_is_answered_with_the_answer_and_each_actions_result() {
    let (mut rig, host, asked) = rig_with_host().await;
    push(&rig, ask("k1", "why is step 5 stuck?"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["id"], "k1");
    assert_eq!(sent[0]["answer"], "Answer to why is step 5 stuck?");
    assert_eq!(sent[0]["actions"], json!([{ "type": "pause", "ok": true }]));
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_reply_reaches_the_host_with_its_thread_and_page() {
    let (mut rig, host, asked) = rig_with_host().await;
    let mut a = ask("k2", "and then?");
    (a.reply_to, a.about, a.images_block) = (
        Some("k1".into()),
        Some("item-a".into()),
        "Attached images: a.png".into(),
    );
    push(&rig, a.clone());
    until(&mut rig, &asked, 1).await;
    assert_eq!(host.jobs.lock().unwrap()[0], a);
}

#[tokio::test]
async fn an_ask_runs_with_every_slot_busy_and_counts_as_a_running_session() {
    let (mut rig, host, asked) = rig_with_host().await;
    rig.machine.concurrency = 0;
    host.hold.store(true, Ordering::SeqCst);
    push(&rig, ask("k1", "hello"));
    for _ in 0..50 {
        rig.machine.pass().await;
        if !host.jobs.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
    assert_eq!(rig.lp(0).asking(), 1);
    assert!(reports(&asked).is_empty());
}

#[tokio::test]
async fn a_second_ask_is_not_held_up_by_the_first() {
    let (mut rig, host, asked) = rig_with_host().await;
    host.hold.store(true, Ordering::SeqCst);
    push(&rig, ask("k1", "one"));
    quiet_passes(&mut rig).await;
    host.hold.store(false, Ordering::SeqCst);
    push(&rig, ask("k2", "two"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["id"], "k2");
    assert_eq!(reports(&asked).len(), 1, "the first is still thinking");
}

/// The gate holds the ask, which is reported waiting once, and lifting it gets the answer.
async fn waits_then_answers(rig: &mut Rig, asked: &Asked, lift: impl FnOnce(&mut Rig)) {
    push(rig, ask("k1", "hello"));
    let sent = until(rig, asked, 1).await;
    assert_eq!(sent[0]["id"], "k1");
    assert_eq!(sent[0]["status"], "waiting");
    assert_eq!(sent[0]["waiting"], WAITING);
    assert!(sent[0].get("answer").is_none());
    quiet_passes(rig).await;
    assert_eq!(reports(asked).len(), 1, "the waiting message goes once");
    lift(rig);
    let sent = until(rig, asked, 2).await;
    assert_eq!(sent[1]["answer"], "Answer to hello");
    quiet_passes(rig).await;
    assert_eq!(reports(asked).len(), 2, "answered once");
}

#[tokio::test]
async fn while_this_computer_is_paused_the_ask_waits_and_is_answered_on_resume() {
    let (mut rig, host, asked) = rig_with_host().await;
    rig.pause("p0", true);
    waits_then_answers(&mut rig, &asked, |rig| rig.pause("p0", false)).await;
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn while_the_project_is_paused_or_over_budget_the_ask_waits_and_is_answered_after() {
    let (mut rig, host, asked) = rig_with_host().await;
    host.blocked.store(true, Ordering::SeqCst);
    let lift = host.clone();
    waits_then_answers(&mut rig, &asked, move |_| lift.blocked.store(false, Ordering::SeqCst)).await;
}

#[tokio::test]
async fn at_the_usage_limit_the_ask_waits_and_is_answered_when_it_passes() {
    let (mut rig, host, asked) = rig_with_host().await;
    rig.machine
        .engine_limits
        .hit(EngineId::Claude, std::time::Instant::now() + Duration::from_secs(3600));
    waits_then_answers(&mut rig, &asked, |rig| {
        rig.machine.engine_limits = Default::default();
    })
    .await;
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_store_error_leaves_the_ask_unanswered_until_the_store_is_back() {
    let (mut rig, host, asked) = rig_with_host().await;
    host.broken.store(true, Ordering::SeqCst);
    push(&rig, ask("k1", "hello"));
    quiet_passes(&mut rig).await;
    assert!(reports(&asked).is_empty());
    host.broken.store(false, Ordering::SeqCst);
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["answer"], "Answer to hello");
}

#[tokio::test]
async fn with_the_flag_off_an_ask_is_refused_and_nothing_runs() {
    let (mut rig, host, asked) = rig_with_host().await;
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&[]));
    push(&rig, ask("k1", "hello"));
    quiet_passes(&mut rig).await;
    assert!(host.jobs.lock().unwrap().is_empty());
    assert!(reports(&asked).is_empty());
    assert!(!rig.machine.links.item_commands.ask_on("p0"));
}

#[tokio::test]
async fn a_session_that_fails_is_answered_once_and_not_run_again() {
    let (mut rig, host, asked) = rig_with_host().await;
    host.crashed.store(true, Ordering::SeqCst);
    push(&rig, ask("k1", "hello"));
    let sent = until(&mut rig, &asked, 1).await;
    assert!(sent[0]["answer"].as_str().unwrap().starts_with("Sorry"));
    quiet_passes(&mut rig).await;
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
    assert_eq!(reports(&asked).len(), 1);
}
