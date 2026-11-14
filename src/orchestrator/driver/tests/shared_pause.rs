//! A project is held while this computer's pause or the store's is on, or the
//! store's is unknown; work already running carries on.
use super::asks::{self, Host as AskHost};
use super::drafts::Host as DraftHost;
use super::*;
use axum::response::IntoResponse;

const OFF: u8 = 0;
const ON: u8 = 1;
const DOWN: u8 = 2;

/// A store with an item to claim and a draft to take (each once), whose `facts_get` answers by `mode`.
async fn store(mode: Arc<AtomicU32>, rows: asks::Listed) -> (String, Asked) {
    let asked: Asked = Default::default();
    let (seen, claimed, drafted) = (
        asked.clone(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, mode, claimed, drafted, rows) = (
                seen.clone(),
                mode.clone(),
                claimed.clone(),
                drafted.clone(),
                rows.clone(),
            );
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                let value = match op.as_str() {
                    "facts_get" => match mode.load(Ordering::SeqCst) as u8 {
                        DOWN => return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                        ON => json!({ "paused": true, "pausedReason": "Paused by @james for everyone" }),
                        _ => json!({ "paused": false }),
                    },
                    "list" => json!([item("a", "claimed")]),
                    "intent_list" => Value::Array(rows.rows.lock().unwrap().clone()),
                    "intent_claim" => json!({ "won": true }),
                    "intent_beat" | "intent_finish" => json!({ "ok": true }),
                    "claim_next" if !claimed.swap(true, Ordering::SeqCst) => {
                        json!({ "item": item("a", "claimed"), "token": "lease-1" })
                    }
                    "draft_claim_next" if !drafted.swap(true, Ordering::SeqCst) => json!({
                        "draft": { "id": "d1", "request": "Add charts", "titles": ["Charts"] }, "token": "lease-d" }),
                    _ => Value::Null,
                };
                Json(json!({ "value": value })).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

struct T {
    rig: Rig,
    fake: Arc<Fake>,
    asks: Arc<AskHost>,
    drafts: Arc<DraftHost>,
    asked: Asked,
    listed: asks::Listed,
    mode: Arc<AtomicU32>,
}

async fn setup(mode: u8, own: bool) -> T {
    let mode = Arc::new(AtomicU32::new(mode as u32));
    let listed: asks::Listed = Default::default();
    let (base, asked) = store(mode.clone(), listed.clone()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    let (asks, drafts) = (Arc::new(AskHost::default()), Arc::new(DraftHost::default()));
    rig.machine.asks = Some(asks.clone());
    rig.lp(0).asks.poll_every = Duration::ZERO;
    rig.machine.drafts = Some(drafts.clone());
    rig.pause("p0", own);
    T {
        rig,
        fake,
        asks,
        drafts,
        asked,
        listed,
        mode,
    }
}

fn count(t: &T, op: &str) -> usize {
    ops(&t.asked).iter().filter(|o| *o == op).count()
}

/// Nothing new started in a few passes, while the fetch-side reads carried on.
async fn held(t: &mut T) {
    asks::push(&t.listed, asks::code_question("in-1", "hello"));
    for _ in 0..5 {
        t.rig.machine.pass().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(count(t, "claim_next"), 0);
    assert_eq!(count(t, "draft_claim_next"), 0);
    assert!(t.fake.started.lock().unwrap().is_empty());
    assert!(t.asks.jobs.lock().unwrap().is_empty());
    assert!(t.drafts.jobs.lock().unwrap().is_empty());
    assert!(count(t, "list") > 0, "the read still runs");
}

#[tokio::test]
async fn the_stores_pause_holds_everything_new() {
    let mut t = setup(ON, false).await;
    held(&mut t).await;
}

#[tokio::test]
async fn this_computers_pause_alone_holds_everything_new() {
    let mut t = setup(OFF, true).await;
    held(&mut t).await;
}

#[tokio::test]
async fn both_pauses_hold_everything_new() {
    let mut t = setup(ON, true).await;
    held(&mut t).await;
}

#[tokio::test]
async fn an_unreachable_store_holds_everything_new() {
    let mut t = setup(DOWN, false).await;
    held(&mut t).await;
}

#[tokio::test]
async fn a_computer_resume_while_the_store_is_paused_keeps_everything_held() {
    let mut t = setup(ON, true).await;
    t.rig.pause("p0", false);
    held(&mut t).await;
}

#[tokio::test]
async fn with_no_pause_the_claim_and_the_draft_start() {
    let mut t = setup(OFF, false).await;
    for _ in 0..5 {
        t.rig.machine.pass().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(count(&t, "claim_next") >= 1);
    assert!(count(&t, "draft_claim_next") >= 1);
}

#[tokio::test]
async fn a_session_already_running_finishes_while_the_store_is_paused() {
    let mut t = setup(OFF, false).await;
    t.fake
        .script
        .lock()
        .unwrap()
        .push(SessionResult::Implementer(ImplementerOutcome::Finished));
    t.rig.machine.pass().await;
    assert_eq!(t.fake.started.lock().unwrap().len(), 1);
    t.mode.store(ON as u32, Ordering::SeqCst);
    t.rig.machine.pass().await;
    assert_eq!(
        t.rig.lp(0).runs["a"].state,
        ItemState::Checking,
        "the ended session was taken in"
    );
}

#[tokio::test]
async fn a_question_waits_across_the_stores_pause_and_is_answered_after_it_lifts() {
    let mut t = setup(ON, false).await;
    asks::push(&t.listed, asks::code_question("in-1", "hello"));
    asks::quiet_passes(&mut t.rig).await;
    assert_eq!(count(&t, "intent_claim"), 0, "held while the store is paused");
    t.mode.store(OFF as u32, Ordering::SeqCst);
    let sent = asks::until(&mut t.rig, &t.asked, 1).await;
    assert_eq!(sent[0]["result"]["answer"], "Answer to hello");
}
