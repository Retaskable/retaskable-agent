use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::wake::wake;
use super::*;
use crate::work_signal::{Wake, WorkSignals};

fn texts(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|s| s.to_string()).collect()
}

#[test]
fn every_push_the_store_sends_is_read() {
    let cases = [
        (r#"{"type":"work_available"}"#, Push::WorkAvailable),
        (
            r#"{"type":"changed","items":["a","b"]}"#,
            Push::Changed {
                items: texts(&["a", "b"]),
            },
        ),
        (
            r#"{"type":"drafts_changed","drafts":["d"]}"#,
            Push::DraftsChanged { drafts: texts(&["d"]) },
        ),
        (
            r#"{"type":"settings_changed","version":4}"#,
            Push::SettingsChanged { version: 4.0 },
        ),
        (r#"{"type":"paused_changed"}"#, Push::PausedChanged),
        (r#"{"type":"tryouts_changed"}"#, Push::TryoutsChanged),
        (
            r#"{"type":"tryout_problem","key":"k","notes":"it broke"}"#,
            Push::TryoutProblem {
                key: "k".into(),
                notes: "it broke".into(),
            },
        ),
        (
            r#"{"type":"draft_answer","id":"d1"}"#,
            Push::DraftAnswer { id: "d1".into() },
        ),
        (
            r#"{"type":"facts_changed","kinds":["expert_queue","smoke"]}"#,
            Push::FactsChanged {
                kinds: texts(&["expert_queue", "smoke"]),
            },
        ),
        (r#"{"type":"ideas_changed"}"#, Push::IdeasChanged),
    ];
    for (text, push) in cases {
        assert_eq!(parse(text), Some(push), "{text}");
    }
}

#[test]
fn a_push_missing_what_it_needs_is_ignored_and_the_rest_is_filled_in() {
    // The same leniency as `receive` in backlogSocket.ts.
    for text in [
        "pong",
        "not json",
        "[]",
        r#"{"type":"unknown"}"#,
        r#"{"type":7}"#,
        r#"{"items":["a"]}"#,
        r#"{"type":"changed"}"#,
        r#"{"type":"changed","items":"a"}"#,
        r#"{"type":"drafts_changed"}"#,
        r#"{"type":"settings_changed","version":"4"}"#,
        r#"{"type":"tryout_problem","notes":"x"}"#,
        r#"{"type":"draft_answer","id":3}"#,
    ] {
        assert_eq!(parse(text), None, "{text}");
    }
    assert_eq!(
        parse(r#"{"type":"tryout_problem","key":"k"}"#),
        Some(Push::TryoutProblem {
            key: "k".into(),
            notes: String::new()
        })
    );
    assert_eq!(
        parse(r#"{"type":"facts_changed"}"#),
        Some(Push::FactsChanged { kinds: vec![] })
    );
    // Only the strings of a list are kept.
    assert_eq!(
        parse(r#"{"type":"changed","items":["a",1,null,"b"]}"#),
        Some(Push::Changed {
            items: texts(&["a", "b"])
        })
    );
}

#[test]
fn a_push_wakes_only_what_waits_on_it() {
    let signals = WorkSignals::default();
    let taken = |s: &WorkSignals| [Wake::Work, Wake::ExpertQueue, Wake::DeployRequests].map(|k| s.take("p", k));
    wake(&signals, "p", &Push::WorkAvailable);
    assert_eq!(taken(&signals), [true, false, false]);
    wake(
        &signals,
        "p",
        &Push::FactsChanged {
            kinds: texts(&["expert_queue"]),
        },
    );
    assert_eq!(taken(&signals), [false, true, false]);
    wake(
        &signals,
        "p",
        &Push::FactsChanged {
            kinds: texts(&["deploy_requests", "smoke"]),
        },
    );
    assert_eq!(taken(&signals), [false, false, true]);
    // Nothing named: any may have changed.
    wake(&signals, "p", &Push::FactsChanged { kinds: vec![] });
    assert_eq!(taken(&signals), [false, true, true]);
    // Facts about something else wake nothing.
    wake(
        &signals,
        "p",
        &Push::FactsChanged {
            kinds: texts(&["smoke", "lighthouse"]),
        },
    );
    assert_eq!(taken(&signals), [false, false, false]);
    // A socket that opened again may have missed any of them.
    wake(&signals, "p", &Push::Reconnected);
    assert_eq!(taken(&signals), [true, true, true]);
    // The rest are heard and dropped, and a wake is for its own project.
    for push in [Push::Changed { items: vec![] }, Push::PausedChanged, Push::IdeasChanged] {
        wake(&signals, "p", &push);
    }
    wake(&signals, "other", &Push::WorkAvailable);
    assert_eq!(taken(&signals), [false, false, false]);
}

/// What the fake transport does with the next open.
enum Script {
    Open(Conn),
    Fail(&'static str),
    Hang,
}

#[derive(Default)]
struct FakeTransport {
    script: Mutex<VecDeque<Script>>,
    opened: Mutex<Option<mpsc::UnboundedSender<(String, String, Instant)>>>,
}

impl Transport for FakeTransport {
    fn open(&self, url: &str, access_token: &str) -> BoxFuture<'static, Result<Conn, String>> {
        let at = (url.to_string(), access_token.to_string(), Instant::now());
        self.opened.lock().unwrap().as_ref().unwrap().send(at).unwrap();
        let next = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .expect("an open nobody scripted");
        Box::pin(async move {
            match next {
                Script::Open(conn) => Ok(conn),
                Script::Fail(why) => Err(why.to_string()),
                Script::Hang => std::future::pending().await,
            }
        })
    }
}

/// Answers each token ask from a script and notes what it was asked.
struct FakeTokens {
    answers: Mutex<VecDeque<TokenAnswer>>,
    asked: Mutex<Vec<Option<String>>>,
}

impl Tokens for FakeTokens {
    fn access_token(&self, stale: Option<String>) -> BoxFuture<'static, TokenAnswer> {
        self.asked.lock().unwrap().push(stale);
        let next = self.answers.lock().unwrap().pop_front();
        Box::pin(async move { next.expect("a token ask nobody scripted") })
    }
}

fn token(t: &str) -> TokenAnswer {
    TokenAnswer::Token {
        url: "https://store.test".into(),
        access_token: t.into(),
    }
}

struct Rig {
    handle: Handle,
    tokens: Arc<FakeTokens>,
    opened: mpsc::UnboundedReceiver<(String, String, Instant)>,
    pushes: mpsc::UnboundedReceiver<Push>,
    started: Instant,
}

/// Starts a socket on virtual time, with no randomness in its waits.
fn rig(tokens: Vec<TokenAnswer>, script: Vec<Script>, repo: Option<u64>) -> Rig {
    let transport = Arc::new(FakeTransport {
        script: Mutex::new(script.into()),
        ..Default::default()
    });
    let (otx, opened) = mpsc::unbounded_channel();
    *transport.opened.lock().unwrap() = Some(otx);
    let tokens = Arc::new(FakeTokens {
        answers: Mutex::new(tokens.into()),
        asked: Mutex::default(),
    });
    let (ptx, pushes) = mpsc::unbounded_channel();
    let deps = Deps {
        project: "p".into(),
        tokens: tokens.clone(),
        transport: transport.clone(),
        repo_id: Arc::new(move || Box::pin(async move { repo })),
        pushed: Arc::new(move |p| {
            let _ = ptx.send(p);
        }),
        timing: Timing::default(),
        random: || 0.0,
    };
    let started = Instant::now();
    Rig {
        handle: start(deps),
        tokens,
        opened,
        pushes,
        started,
    }
}

impl Rig {
    async fn opens(&mut self) -> (String, String, Duration) {
        let (url, token, at) = self.opened.recv().await.unwrap();
        (url, token, at - self.started)
    }
}

#[tokio::test(start_paused = true)]
async fn it_connects_with_the_token_and_hears_what_the_store_pushes() {
    let (conn, from_store, _to_store) = pair();
    let mut r = rig(vec![token("T")], vec![Script::Open(conn)], Some(42));
    let (url, bearer, _) = r.opens().await;
    assert_eq!(
        (url.as_str(), bearer.as_str()),
        ("wss://store.test/v1/repos/42/socket", "T")
    );
    // Each opening is a `reconnected`, so what waits on pushes looks again.
    assert_eq!(r.pushes.recv().await, Some(Push::Reconnected));
    for text in [
        "pong",
        "garbage",
        r#"{"type":"work_available"}"#,
        r#"{"type":"facts_changed","kinds":["expert_queue"]}"#,
    ] {
        from_store.send(Incoming::Text(text.into())).await.unwrap();
    }
    assert_eq!(r.pushes.recv().await, Some(Push::WorkAvailable));
    assert_eq!(
        r.pushes.recv().await,
        Some(Push::FactsChanged {
            kinds: texts(&["expert_queue"])
        })
    );
    r.handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn it_says_ping_every_keepalive_and_closes_cleanly_when_stopped() {
    let (conn, _from_store, mut to_store) = pair();
    let mut r = rig(vec![token("T")], vec![Script::Open(conn)], Some(1));
    r.opens().await;
    let began = Instant::now();
    for n in 1..=3u32 {
        assert_eq!(to_store.recv().await, Some(Outgoing::Text("ping".into())));
        assert_eq!(Instant::now() - began, Timing::default().keepalive * n);
    }
    r.handle.stop().await;
    assert_eq!(
        to_store.recv().await,
        Some(Outgoing::Close(1000, "agent stopping".into()))
    );
}

#[tokio::test(start_paused = true)]
async fn an_agent_that_is_not_linked_looks_again_every_minute_and_asks_nothing_else() {
    let mut r = rig(
        vec![TokenAnswer::Idle, TokenAnswer::Idle, TokenAnswer::Idle],
        vec![],
        Some(1),
    );
    // Never opens: `Idle` is answered three times, a minute apart (the third ask is where it stops being scripted).
    tokio::time::sleep(Duration::from_secs(125)).await;
    assert_eq!(r.tokens.asked.lock().unwrap().len(), 3);
    assert!(r.opened.try_recv().is_err());
    // (The fourth ask would panic the task: stop before it.)
    r.handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_failed_connect_backs_off_and_the_next_try_takes_a_fresh_token() {
    let (conn, _from_store, _to_store) = pair();
    let script = vec![Script::Fail("refused"), Script::Fail("refused"), Script::Open(conn)];
    let mut r = rig(vec![token("A"), token("B"), token("C")], script, Some(1));
    let waits = [r.opens().await.2, r.opens().await.2, r.opens().await.2];
    // 1 s then 2 s: the waits double from the first (no randomness here).
    assert_eq!(waits, [Duration::ZERO, Duration::from_secs(1), Duration::from_secs(3)]);
    // The token a connect never opened on is the one the next ask calls stale.
    let asked = r.tokens.asked.lock().unwrap().clone();
    assert_eq!(asked, vec![None, Some("A".to_string()), Some("B".to_string())]);
    r.handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_connect_that_never_answers_is_given_up() {
    let (conn, _from_store, _to_store) = pair();
    let mut r = rig(
        vec![token("A"), token("B")],
        vec![Script::Hang, Script::Open(conn)],
        Some(1),
    );
    assert_eq!(r.opens().await.2, Duration::ZERO);
    let second = r.opens().await.2;
    assert_eq!(second, Timing::default().connect + Timing::default().backoff);
    r.handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_good_connection_starts_the_backoff_over_and_a_short_one_does_not() {
    let (a, a_in, _a_out) = pair();
    let (b, b_in, _b_out) = pair();
    let (c, c_in, _c_out) = pair();
    let tokens = vec![token("1"), token("2"), token("3"), token("4")];
    let script = vec![Script::Open(a), Script::Open(b), Script::Open(c), Script::Fail("down")];
    let mut r = rig(tokens, script, Some(1));
    r.opens().await;
    // Dropped at once: the first wait, 1 s.
    a_in.send(Incoming::Closed(1006, String::new())).await.unwrap();
    let t0 = r.opens().await.2;
    b_in.send(Incoming::Closed(1006, String::new())).await.unwrap();
    // Dropped again at once: the second wait, 2 s, not 1 s.
    let t1 = r.opens().await.2;
    assert_eq!(t1 - t0, Duration::from_secs(2));
    // This one lasts past `stable`, so the wait after it is 1 s again.
    tokio::time::sleep(Timing::default().stable).await;
    c_in.send(Incoming::Closed(1001, "going away".into())).await.unwrap();
    let t2 = r.opens().await.2;
    assert_eq!(t2 - t1, Timing::default().stable + Duration::from_secs(1));
    r.handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn unlinked_on_its_agents_page_it_asks_for_a_token_at_once_and_forgets_the_link() {
    let (conn, from_store, _to_store) = pair();
    let (again, _again_in, _again_out) = pair();
    let mut r = rig(
        vec![token("A"), TokenAnswer::Idle, token("C")],
        vec![Script::Open(conn), Script::Open(again)],
        Some(1),
    );
    r.opens().await;
    from_store
        .send(Incoming::Closed(CLOSE_UNLINKED, "Unlinked from the backlog".into()))
        .await
        .unwrap();
    // No wait, and the token that was refused goes with the ask (the refresh then fails and forgets the link);
    // answered `Idle` it looks again in a minute.
    let (_, bearer, at) = r.opens().await;
    assert_eq!(bearer, "C");
    assert_eq!(at, Duration::from_secs(60));
    let asked = r.tokens.asked.lock().unwrap().clone();
    assert_eq!(asked, vec![None, Some("A".to_string()), None]);
    r.handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn without_the_repos_id_it_tries_again_later_and_opens_nothing() {
    let mut r = rig(vec![token("A"), token("B")], vec![], None);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(r.tokens.asked.lock().unwrap().len(), 2);
    assert!(r.opened.try_recv().is_err());
    r.handle.stop().await;
}

#[allow(clippy::result_large_err)] // tungstenite's handshake callback's own signature
#[tokio::test(start_paused = true)]
async fn the_real_transport_signs_in_with_a_bearer_header_and_carries_text_both_ways() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let seen = Arc::new(Mutex::new(String::new()));
        let note = seen.clone();
        let ws = tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, res: Response| {
            let line = format!(
                "{} {}",
                req.uri().path(),
                req.headers()["authorization"].to_str().unwrap()
            );
            *note.lock().unwrap() = line;
            Ok(res)
        })
        .await
        .unwrap();
        let (mut tx, mut rx) = ws.split();
        tx.send(Message::text(r#"{"type":"work_available"}"#)).await.unwrap();
        let got = rx.next().await.unwrap().unwrap();
        tx.send(Message::Close(Some(
            tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: 4401.into(),
                reason: "Unlinked".into(),
            },
        )))
        .await
        .unwrap();
        let line = seen.lock().unwrap().clone();
        (line, got)
    });
    let mut conn = Wire
        .open(&format!("ws://127.0.0.1:{port}/v1/repos/9/socket"), "secret")
        .await
        .unwrap();
    assert_eq!(
        conn.incoming.recv().await,
        Some(Incoming::Text(r#"{"type":"work_available"}"#.into()))
    );
    conn.outgoing.send(Outgoing::Text("ping".into())).await.unwrap();
    assert_eq!(
        conn.incoming.recv().await,
        Some(Incoming::Closed(4401, "Unlinked".into()))
    );
    let (line, got) = server.await.unwrap();
    assert_eq!(line, "/v1/repos/9/socket Bearer secret");
    assert_eq!(got, Message::text("ping"));
}
