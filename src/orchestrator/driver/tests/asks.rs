//! The ask step (`driver/asks.rs`): a fake store that records every op and a fake host whose answer is scripted.
use super::*;
use crate::engines::{EngineId, EventKind, SessionEvent};
use crate::orchestrator::driver::{AskHost, AskJob, AskReply, AskStore, WAITING};
use crate::view::AskCommand;

#[derive(Default)]
pub(super) struct Host {
    /// `blocked` answers: the project's pause or the daily budget.
    blocked: AtomicBool,
    /// The store can't be read.
    broken: AtomicBool,
    hold: AtomicBool,
    /// The session fails: the host answers that it couldn't.
    crashed: AtomicBool,
    pub(super) jobs: Mutex<Vec<AskCommand>>,
    engines: Mutex<Vec<Option<EngineId>>>,
    /// Tool events the session emits, a pause apart, before it answers.
    reads: Mutex<Vec<&'static str>>,
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
        self.engines.lock().unwrap().push(job.engine);
        let (broken, hold) = (self.broken.load(Ordering::SeqCst), self.hold.load(Ordering::SeqCst));
        let crashed = self.crashed.load(Ordering::SeqCst);
        let reads = std::mem::take(&mut *self.reads.lock().unwrap());
        async move {
            if crashed {
                return Ok(AskReply {
                    answer: "Sorry, I couldn't answer that: claude crashed".into(),
                    actions: vec![],
                });
            }
            for path in reads {
                for _ in 0..3 {
                    let tool = SessionEvent {
                        at: 0,
                        kind: EventKind::Tool,
                        text: format!("Read: {path}"),
                    };
                    let _ = job.events.send(tool);
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
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

/// A store that records every op, refusing the op `fail` with a 500.
async fn store_failing(fail: &'static str) -> (String, Asked) {
    let asked: Asked = Default::default();
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                if op == fail {
                    return (
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": "boom" })),
                    );
                }
                let value = if op == "list" { json!([]) } else { Value::Null };
                (axum::http::StatusCode::OK, Json(json!({ "value": value })))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

async fn rig_with_host() -> (Rig, Arc<Host>, Asked) {
    rig_failing("").await
}

async fn rig_failing(fail: &'static str) -> (Rig, Arc<Host>, Asked) {
    let (base, asked) = store_failing(fail).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["ask-claude", "watchdog-anomaly-rules"]));
    let host = Arc::new(Host::default());
    rig.machine.asks = Some(host.clone());
    (rig, host, asked)
}

pub(super) fn ask(id: &str, text: &str) -> AskCommand {
    AskCommand {
        project: "p0".into(),
        ask_id: id.into(),
        text: text.into(),
        reply_to: None,
        about: None,
        images_block: String::new(),
    }
}

pub(super) fn push(rig: &Rig, a: AskCommand) {
    rig.machine.links.item_commands.push_ask(a);
}

pub(super) fn reports(asked: &Asked) -> Vec<Value> {
    (asked.lock().unwrap().iter())
        .filter(|(op, _)| op == "ask_report")
        .map(|(_, a)| a.clone())
        .collect()
}

/// Passes until the store has `n` ask reports.
pub(super) async fn until(rig: &mut Rig, asked: &Asked, n: usize) -> Vec<Value> {
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

pub(super) async fn quiet_passes(rig: &mut Rig) {
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

#[tokio::test]
async fn progress_reports_rising_time_and_the_file_read_then_the_answer() {
    let (mut rig, host, asked) = rig_with_host().await;
    rig.lp(0).asks.progress_every = Duration::from_millis(100);
    *host.reads.lock().unwrap() = vec!["src/a.rs", "src/b.rs", "src/c.rs"];
    push(&rig, ask("k1", "why?"));
    let sent = loop {
        let sent = until(&mut rig, &asked, 1).await;
        if sent.iter().any(|r| r.get("answer").is_some()) {
            break sent;
        }
    };
    let last = sent.len() - 1;
    assert!(
        sent[last]["answer"].is_string(),
        "the answer follows the last progress report: {sent:?}"
    );
    let progress: Vec<&str> = sent[..last].iter().map(|r| r["progress"].as_str().unwrap()).collect();
    // 9 events came in about 750ms: debounced to a report per tick, not one per event.
    assert!(progress.len() >= 2 && progress.len() < 9, "{progress:?}");
    assert!(progress.iter().any(|p| p.ends_with("reading src/a.rs")), "{progress:?}");
    assert!(progress.last().unwrap().ends_with("reading src/c.rs"), "{progress:?}");
    assert!(progress[0].starts_with("Thinking for 0s"), "{progress:?}");
}

fn out_for(secs: u64) -> std::time::Instant {
    std::time::Instant::now() + Duration::from_secs(secs)
}

/// `engine-start-rules` On, the engines chosen as `choice`; with `claude_out` Claude is signed out, with `cursor_out`
/// Cursor is at its limit.
fn start_rules(rig: &mut Rig, flags: &[&str], choice: Value, claude_out: bool, cursor_out: bool) {
    let mut on = vec!["engine-start-rules", "engine-placement"];
    on.extend(flags);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&on));
    rig.machine.links.engine_choice = serde_json::from_value(choice).unwrap();
    rig.machine.claude_signed_out_until = claude_out.then(|| out_for(600));
    if cursor_out {
        rig.machine
            .engine_limits
            .hit(crate::engines::EngineId::Cursor, out_for(600));
    }
}

#[tokio::test]
async fn with_claude_signed_out_an_ask_on_cursor_is_answered() {
    let (mut rig, host, asked) = rig_with_host().await;
    start_rules(
        &mut rig,
        &["ask-claude"],
        json!({"roles": {"chat": "cursor"}}),
        true,
        false,
    );
    push(&rig, ask("k1", "hello"));
    until(&mut rig, &asked, 1).await;
    assert_eq!(*host.engines.lock().unwrap(), vec![None]);
}

#[tokio::test]
async fn an_ask_on_claude_with_claude_out_is_answered_on_cursor_used_here() {
    let (mut rig, host, asked) = rig_with_host().await;
    start_rules(
        &mut rig,
        &["ask-claude"],
        json!({"roles": {"planner": "cursor"}}),
        true,
        false,
    );
    push(&rig, ask("k1", "hello"));
    until(&mut rig, &asked, 1).await;
    assert_eq!(*host.engines.lock().unwrap(), vec![Some(EngineId::Cursor)]);
}

#[tokio::test]
async fn an_ask_waits_when_no_engine_may_start() {
    let (mut rig, host, asked) = rig_with_host().await;
    start_rules(
        &mut rig,
        &["ask-claude"],
        json!({"roles": {"planner": "cursor"}}),
        true,
        true,
    );
    push(&rig, ask("k1", "hello"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "waiting");
    assert!(host.jobs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_ask_report_the_store_refuses_is_logged_as_a_store_error_for_the_ask() {
    let (mut rig, _host, _asked) = rig_failing("ask_report").await;
    push(&rig, ask("k1", "hello"));
    for _ in 0..40 {
        rig.machine.pass().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let logged: Vec<Value> = (rig.lp(0).events().recent().iter())
        .map(|e| e.to_value())
        .filter(|v| v["kind"] == "store_error" && v["op"] == "ask_report")
        .collect();
    assert!(!logged.is_empty(), "{:?}", rig.lp(0).events().recent());
    assert_eq!(logged[0]["item"], "k1");
}

/// `local-limit-fallback`: Claude at its usage limit holds an ask only when the flag is Off, the model is down or
/// Claude is signed out; else the ask starts (on the local model), with the start rules On or Off.
#[tokio::test]
async fn at_the_limit_an_ask_starts_with_the_fallback_and_waits_without_it() {
    use crate::local_model::LocalModel;
    for rules in [false, true] {
        for (fallback, model, signed_out, starts) in [
            (true, LocalModel::up(), false, true),
            (false, LocalModel::up(), false, false),
            (true, LocalModel::off(), false, false),
            (true, LocalModel::up(), true, false),
        ] {
            crate::tiers::set_local_ladder_for_test(true);
            crate::tiers::set_local_limit_fallback_for_test(fallback);
            let (mut rig, host, _asked) = rig_with_host().await;
            let mut on = vec!["ask-claude", "watchdog-anomaly-rules"];
            on.extend(rules.then_some("engine-start-rules"));
            rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&on));
            rig.machine.local = model;
            rig.machine.claude_signed_out_until =
                signed_out.then(|| std::time::Instant::now() + Duration::from_secs(60));
            rig.machine
                .engine_limits
                .hit(EngineId::Claude, std::time::Instant::now() + Duration::from_secs(3600));
            push(&rig, ask("k1", "hello"));
            quiet_passes(&mut rig).await;
            let case = (rules, fallback, signed_out);
            assert_eq!(host.jobs.lock().unwrap().len(), usize::from(starts), "{case:?}");
        }
    }
}
