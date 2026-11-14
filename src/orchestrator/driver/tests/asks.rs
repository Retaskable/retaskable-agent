//! The ask step (`driver/asks.rs`): a fake store that lists code-question rows and records every op, and a fake host
//! whose outcome is scripted.
use super::*;
use crate::engines::EngineId;
use crate::intents::Outcome;
use crate::orchestrator::driver::{AskHost, AskJob, AskStore};

pub(super) const PICTURE: &str = "0a1b2c3d-0000-4000-8000-0000000000a1.png";

/// What the host saw of one job.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Job {
    pub(super) id: String,
    pub(super) question: String,
    pub(super) images_block: String,
    /// Whether every picture the block lists was on disk while the session ran.
    pub(super) pictures_there: bool,
}

#[derive(Default)]
pub(super) struct Host {
    /// `blocked` answers: the project's pause or the daily budget.
    blocked: AtomicBool,
    /// The store can't be read.
    broken: AtomicBool,
    hold: AtomicBool,
    /// The session fails: the host answers that it couldn't.
    crashed: AtomicBool,
    /// The session was cut by a restart.
    cut: AtomicBool,
    pub(super) jobs: Mutex<Vec<Job>>,
    engines: Mutex<Vec<Option<EngineId>>>,
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
    fn run(&self, job: AskJob) -> BoxFuture<'static, Outcome> {
        let pictures_there = job
            .images_block
            .lines()
            .filter_map(|l| l.strip_prefix("- "))
            .all(|p| std::path::Path::new(p).exists());
        self.jobs.lock().unwrap().push(Job {
            id: job.id.clone(),
            question: job.question.clone(),
            images_block: job.images_block.clone(),
            pictures_there,
        });
        self.engines.lock().unwrap().push(job.engine);
        let (hold, crashed, cut) = (
            self.hold.load(Ordering::SeqCst),
            self.crashed.load(Ordering::SeqCst),
            self.cut.load(Ordering::SeqCst),
        );
        async move {
            if hold {
                std::future::pending::<()>().await;
            }
            if cut {
                return Outcome::Abandoned;
            }
            if crashed {
                return Outcome::Failed("claude crashed".into());
            }
            Outcome::Done(json!({ "answer": format!("Answer to {}", job.question) }))
        }
        .boxed()
    }
}

/// The rows the fake store lists, and what it answers a claim.
#[derive(Default)]
pub(super) struct Rows {
    pub(super) rows: Mutex<Vec<Value>>,
    /// Claim answers in order; won once it is empty.
    pub(super) claims: Mutex<Vec<Value>>,
}

pub(super) type Listed = Arc<Rows>;

/// A store that lists the rows, records every op and refuses the op `fail` with a 500. A row ended with
/// `intent_finish` is no longer listed open.
async fn store_failing(fail: &'static str) -> (String, Asked, Listed) {
    let asked: Asked = Default::default();
    let listed: Listed = Default::default();
    let (seen, rows) = (asked.clone(), listed.clone());
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(move |Json(body): Json<Value>| {
                let (seen, rows) = (seen.clone(), rows.clone());
                async move {
                    let op = body["op"].as_str().unwrap_or("").to_string();
                    seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                    if op == fail {
                        return (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({ "error": "boom" })),
                        );
                    }
                    let value = match op.as_str() {
                        "list" | "drafts" | "draft_list" => json!([]),
                        "facts_get" => json!({ "paused": false }),
                        "intent_list" => Value::Array(rows.rows.lock().unwrap().clone()),
                        "intent_claim" => {
                            let mut claims = rows.claims.lock().unwrap();
                            if claims.is_empty() {
                                json!({ "won": true })
                            } else {
                                claims.remove(0)
                            }
                        }
                        "intent_beat" => json!({ "ok": true }),
                        "intent_finish" => {
                            let id = body["args"]["id"].clone();
                            for row in rows.rows.lock().unwrap().iter_mut().filter(|r| r["id"] == id) {
                                row["status"] = body["args"]["status"].clone();
                            }
                            json!({ "ok": true })
                        }
                        _ => Value::Null,
                    };
                    (axum::http::StatusCode::OK, Json(json!({ "value": value })))
                }
            }),
        )
        .route(
            "/v1/repos/{id}/pictures/{picture}",
            axum::routing::get(
                |axum::extract::Path((_, picture)): axum::extract::Path<(u64, String)>| async move {
                    if picture == PICTURE {
                        (axum::http::StatusCode::OK, picture.into_bytes())
                    } else {
                        (axum::http::StatusCode::NOT_FOUND, Vec::new())
                    }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked, listed)
}

async fn rig_with_host() -> (Rig, Arc<Host>, Asked, Listed) {
    rig_failing("").await
}

async fn rig_failing(fail: &'static str) -> (Rig, Arc<Host>, Asked, Listed) {
    let (base, asked, listed) = store_failing(fail).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.lp(0).asks.poll_every = Duration::ZERO;
    let host = Arc::new(Host::default());
    rig.machine.asks = Some(host.clone());
    (rig, host, asked, listed)
}

pub(super) fn code_question(id: &str, question: &str) -> Value {
    json!({ "id": id, "kind": "ask", "status": "requested", "issuedBy": "ana",
        "args": { "question": question, "codeQuestion": true } })
}

pub(super) fn push(listed: &Listed, row: Value) {
    listed.rows.lock().unwrap().push(row);
}

pub(super) fn ops_named(asked: &Asked, name: &str) -> Vec<Value> {
    (asked.lock().unwrap().iter())
        .filter(|(op, _)| op == name)
        .map(|(_, a)| a.clone())
        .collect()
}

/// The `intent_finish` calls the store has had.
pub(super) fn finishes(asked: &Asked) -> Vec<Value> {
    ops_named(asked, "intent_finish")
}

/// Passes until the store has `n` finishes.
pub(super) async fn until(rig: &mut Rig, asked: &Asked, n: usize) -> Vec<Value> {
    for _ in 0..200 {
        rig.machine.pass().await;
        let sent = finishes(asked);
        if sent.len() >= n {
            return sent;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the store never got {n} finishes: {:?}", finishes(asked));
}

pub(super) async fn quiet_passes(rig: &mut Rig) {
    for _ in 0..8 {
        rig.machine.pass().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_code_question_is_claimed_beaten_answered_and_finished_with_its_answer() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    push(&listed, code_question("in-1", "why is step 5 stuck?"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        sent[0],
        json!({ "id": "in-1", "status": "done", "result": { "answer": "Answer to why is step 5 stuck?" } })
    );
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
    assert_eq!(host.jobs.lock().unwrap()[0].id, "in-1");
    let claims = ops_named(&asked, "intent_claim");
    assert_eq!(claims[0]["id"], "in-1");
    assert_eq!(claims[0]["can"], true);
    assert!(
        !ops_named(&asked, "intent_beat").is_empty(),
        "the first beat goes at once: {:?}",
        asked.lock().unwrap()
    );
    let order: Vec<String> = asked.lock().unwrap().iter().map(|(op, _)| op.clone()).collect();
    let at = |name: &str| order.iter().position(|o| o == name).unwrap();
    assert!(at("intent_list") < at("intent_claim") && at("intent_claim") < at("intent_beat"));
    assert!(at("intent_beat") < at("intent_finish"));
    assert_eq!(ops_named(&asked, "intent_list")[0], json!({ "kinds": ["ask"] }));
}

#[tokio::test]
async fn a_question_is_answered_once_though_the_store_keeps_listing_the_row() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    push(&listed, code_question("in-1", "hello"));
    until(&mut rig, &asked, 1).await;
    // Even a store that hasn't recorded the finish lists the row open again.
    listed.rows.lock().unwrap()[0]["status"] = json!("running");
    quiet_passes(&mut rig).await;
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
    assert_eq!(finishes(&asked).len(), 1);
}

#[tokio::test]
async fn an_ask_that_is_not_a_code_question_is_left_alone() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    push(
        &listed,
        json!({ "id": "in-1", "kind": "ask", "status": "requested", "args": { "question": "hi" } }),
    );
    quiet_passes(&mut rig).await;
    assert!(host.jobs.lock().unwrap().is_empty());
    assert!(ops_named(&asked, "intent_claim").is_empty());
}

#[tokio::test]
async fn the_rows_pictures_are_fetched_shown_to_the_session_and_removed_after() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    let mut row = code_question("in-1", "what is in this picture?");
    row["args"]["pictureIds"] = json!([PICTURE, "../escape.png"]);
    push(&listed, row);
    until(&mut rig, &asked, 1).await;
    let job = host.jobs.lock().unwrap()[0].clone();
    assert!(job.images_block.starts_with("Attached images"), "{job:?}");
    assert!(
        job.images_block.contains(PICTURE) && !job.images_block.contains("escape"),
        "{job:?}"
    );
    assert!(job.pictures_there, "the picture was on disk while the session ran");
    let path = job
        .images_block
        .lines()
        .last()
        .unwrap()
        .trim_start_matches("- ")
        .to_string();
    for _ in 0..100 {
        if !std::path::Path::new(&path).exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!std::path::Path::new(&path).exists(), "removed after the run");
}

#[tokio::test]
async fn a_picture_the_store_cannot_give_fails_the_row_without_running_a_session() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    let mut row = code_question("in-1", "look");
    row["args"]["pictureIds"] = json!(["0a1b2c3d-0000-4000-8000-0000000000ff.png"]);
    push(&listed, row);
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "failed");
    assert_eq!(sent[0]["why"], "Couldn't read the attached pictures.");
    assert!(host.jobs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_question_runs_with_every_slot_busy_and_counts_as_a_running_session() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    rig.machine.concurrency = 0;
    host.hold.store(true, Ordering::SeqCst);
    push(&listed, code_question("in-1", "hello"));
    for _ in 0..50 {
        rig.machine.pass().await;
        if !host.jobs.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
    assert_eq!(rig.lp(0).asking(), 1);
    assert!(finishes(&asked).is_empty());
}

#[tokio::test]
async fn a_second_question_is_not_held_up_by_the_first() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    host.hold.store(true, Ordering::SeqCst);
    push(&listed, code_question("in-1", "one"));
    quiet_passes(&mut rig).await;
    host.hold.store(false, Ordering::SeqCst);
    push(&listed, code_question("in-2", "two"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["id"], "in-2");
    assert_eq!(finishes(&asked).len(), 1, "the first is still thinking");
}

/// The gate holds the question: the store isn't even asked for rows. Lifting it gets the answer.
async fn waits_then_answers(rig: &mut Rig, asked: &Asked, listed: &Listed, lift: impl FnOnce(&mut Rig)) {
    push(listed, code_question("in-1", "hello"));
    quiet_passes(rig).await;
    assert!(ops_named(asked, "intent_list").is_empty(), "held: no look at the store");
    assert!(ops_named(asked, "intent_claim").is_empty());
    lift(rig);
    let sent = until(rig, asked, 1).await;
    assert_eq!(sent[0]["result"]["answer"], "Answer to hello");
    quiet_passes(rig).await;
    assert_eq!(finishes(asked).len(), 1, "answered once");
}

#[tokio::test]
async fn while_this_computer_is_paused_the_question_waits_and_is_answered_on_resume() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    rig.pause("p0", true);
    waits_then_answers(&mut rig, &asked, &listed, |rig| rig.pause("p0", false)).await;
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn while_the_project_is_paused_or_over_budget_the_question_waits_and_is_answered_after() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    host.blocked.store(true, Ordering::SeqCst);
    let lift = host.clone();
    waits_then_answers(&mut rig, &asked, &listed, move |_| {
        lift.blocked.store(false, Ordering::SeqCst)
    })
    .await;
}

#[tokio::test]
async fn at_the_usage_limit_the_question_waits_and_is_answered_when_it_passes() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    rig.machine
        .engine_limits
        .hit(EngineId::Claude, std::time::Instant::now() + Duration::from_secs(3600));
    waits_then_answers(&mut rig, &asked, &listed, |rig| {
        rig.machine.engine_limits = Default::default();
    })
    .await;
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_store_error_takes_nothing_until_the_store_is_back() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    host.broken.store(true, Ordering::SeqCst);
    push(&listed, code_question("in-1", "hello"));
    quiet_passes(&mut rig).await;
    assert!(finishes(&asked).is_empty() && host.jobs.lock().unwrap().is_empty());
    host.broken.store(false, Ordering::SeqCst);
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["result"]["answer"], "Answer to hello");
}

#[tokio::test]
async fn a_claim_held_back_by_the_board_or_lost_runs_nothing_until_it_is_won() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    *listed.claims.lock().unwrap() = vec![json!({ "won": false, "blocked": true }), json!({ "won": false })];
    push(&listed, code_question("in-1", "hello"));
    quiet_passes(&mut rig).await;
    assert!(ops_named(&asked, "intent_claim").len() >= 3, "it asks again each pass");
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["result"]["answer"], "Answer to hello");
    assert_eq!(
        host.jobs.lock().unwrap().len(),
        1,
        "blocked and lost claims ran nothing"
    );
}

#[tokio::test]
async fn a_session_that_fails_finishes_the_row_failed_once_and_is_not_run_again() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    host.crashed.store(true, Ordering::SeqCst);
    push(&listed, code_question("in-1", "hello"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        sent[0],
        json!({ "id": "in-1", "status": "failed", "why": "claude crashed" })
    );
    quiet_passes(&mut rig).await;
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
    assert_eq!(finishes(&asked).len(), 1);
}

#[tokio::test]
async fn a_session_cut_by_a_restart_says_nothing_so_the_row_opens_again() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    host.cut.store(true, Ordering::SeqCst);
    push(&listed, code_question("in-1", "hello"));
    quiet_passes(&mut rig).await;
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
    assert!(finishes(&asked).is_empty());
}

fn out_for(secs: u64) -> std::time::Instant {
    std::time::Instant::now() + Duration::from_secs(secs)
}

/// The engines chosen as `choice`; with `claude_out` Claude is signed out, with `cursor_out`
/// Cursor is at its limit.
fn start_rules(rig: &mut Rig, choice: Value, claude_out: bool, cursor_out: bool) {
    rig.machine.links.engine_choice = serde_json::from_value(choice).unwrap();
    rig.machine.claude_signed_out_until = claude_out.then(|| out_for(600));
    if cursor_out {
        rig.machine
            .engine_limits
            .hit(crate::engines::EngineId::Cursor, out_for(600));
    }
}

#[tokio::test]
async fn with_claude_signed_out_a_question_on_cursor_is_answered() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    start_rules(&mut rig, json!({"roles": {"chat": "cursor"}}), true, false);
    push(&listed, code_question("in-1", "hello"));
    until(&mut rig, &asked, 1).await;
    assert_eq!(*host.engines.lock().unwrap(), vec![None]);
}

#[tokio::test]
async fn a_question_on_claude_with_claude_out_is_answered_on_cursor_used_here() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    start_rules(&mut rig, json!({"roles": {"planner": "cursor"}}), true, false);
    push(&listed, code_question("in-1", "hello"));
    until(&mut rig, &asked, 1).await;
    assert_eq!(*host.engines.lock().unwrap(), vec![Some(EngineId::Cursor)]);
}

#[tokio::test]
async fn a_question_waits_when_no_engine_may_start() {
    let (mut rig, host, asked, listed) = rig_with_host().await;
    start_rules(&mut rig, json!({"roles": {"planner": "cursor"}}), true, true);
    push(&listed, code_question("in-1", "hello"));
    quiet_passes(&mut rig).await;
    assert!(host.jobs.lock().unwrap().is_empty());
    assert!(ops_named(&asked, "intent_claim").is_empty());
}

#[tokio::test]
async fn a_finish_the_store_refuses_is_logged_as_a_store_error_for_the_row() {
    let (mut rig, _host, _asked, listed) = rig_failing("intent_finish").await;
    push(&listed, code_question("in-1", "hello"));
    for _ in 0..40 {
        rig.machine.pass().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let logged: Vec<Value> = (rig.lp(0).events().recent().iter())
        .map(|e| e.to_value())
        .filter(|v| v["kind"] == "store_error" && v["op"] == "intent_finish")
        .collect();
    assert!(!logged.is_empty(), "{:?}", rig.lp(0).events().recent());
    assert_eq!(logged[0]["item"], "in-1");
}

/// Claude at its usage limit holds a question only when the model is down or Claude is signed out; else it starts
/// (on the local model).
#[tokio::test]
async fn at_the_limit_a_question_starts_with_the_fallback_and_waits_without_it() {
    use crate::local_model::LocalModel;
    for (model, signed_out, starts) in [
        (LocalModel::up(), false, true),
        (LocalModel::off(), false, false),
        (LocalModel::up(), true, false),
    ] {
        let (mut rig, host, _asked, listed) = rig_with_host().await;
        rig.machine.local = model;
        rig.machine.claude_signed_out_until = signed_out.then(|| std::time::Instant::now() + Duration::from_secs(60));
        rig.machine
            .engine_limits
            .hit(EngineId::Claude, std::time::Instant::now() + Duration::from_secs(3600));
        push(&listed, code_question("in-1", "hello"));
        quiet_passes(&mut rig).await;
        assert_eq!(host.jobs.lock().unwrap().len(), usize::from(starts), "{signed_out}");
    }
}
