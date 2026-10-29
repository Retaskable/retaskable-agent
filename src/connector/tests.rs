use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

use axum::{Json, Router, extract::State, routing::post};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};

use super::*;
use crate::commands;
use crate::link::FileStore;

/// A connection the fake hub took: the path and query it was asked for, the protocols offered, and the socket.
struct Accepted {
    uri: String,
    protocols: String,
    ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
}

/// A fake cloud: `/api/device/token` hands out access tokens a1, a2 … (and rotates the refresh token), and every
/// WebSocket it accepts comes out of the receiver.
#[allow(clippy::result_large_err)] // tungstenite's handshake callback returns its own error response
async fn cloud() -> (
    String,
    Arc<StdMutex<VecDeque<Value>>>,
    mpsc::UnboundedReceiver<Accepted>,
) {
    let refreshes = Arc::new(StdMutex::new(VecDeque::new()));
    async fn token(State(seen): State<Arc<StdMutex<VecDeque<Value>>>>, Json(body): Json<Value>) -> Json<Value> {
        let mut seen = seen.lock().unwrap();
        seen.push_back(body);
        let n = seen.len();
        Json(json!({ "access_token": format!("a{n}"), "refresh_token": format!("r{}", n + 1), "expires_in": 3600 }))
    }
    let app = Router::new()
        .route("/api/device/token", post(token))
        .with_state(refreshes.clone());
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = http.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(http, app).await.unwrap() });

    // The cloud serves both on one port: a POST goes on to the token endpoint, anything else is a socket.
    let (tx, rx) = mpsc::unbounded_channel();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut head = [0u8; 4];
            if stream.peek(&mut head).await.unwrap_or(0) == 4 && &head == b"POST" {
                // The token's POST: hand the bytes to the HTTP server.
                tokio::spawn(async move {
                    let mut upstream = tokio::net::TcpStream::connect(http_addr).await.unwrap();
                    let mut stream = stream;
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
                });
                continue;
            }
            let tx = tx.clone();
            tokio::spawn(async move {
                let seen = Arc::new(StdMutex::new((String::new(), String::new())));
                let kept = seen.clone();
                let callback = move |req: &Request, mut res: Response| {
                    let protocols = req
                        .headers()
                        .get("Sec-WebSocket-Protocol")
                        .and_then(|v| v.to_str().ok());
                    *kept.lock().unwrap() = (req.uri().to_string(), protocols.unwrap_or("").to_string());
                    res.headers_mut()
                        .insert("Sec-WebSocket-Protocol", HeaderValue::from_static(SUBPROTOCOL));
                    Ok(res)
                };
                if let Ok(ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await {
                    let (uri, protocols) = seen.lock().unwrap().clone();
                    let _ = tx.send(Accepted { uri, protocols, ws });
                }
            });
        }
    });
    (format!("http://{addr}"), refreshes, rx)
}

fn timing() -> Timing {
    Timing {
        idle: Duration::from_millis(200),
        connect: Duration::from_secs(5),
        backoff: Duration::from_millis(10),
        stable: Duration::from_secs(30),
    }
}

fn view(projects: &[&str]) -> Arc<AgentView> {
    Arc::new(AgentView {
        machine: "pc".into(),
        tags: vec!["windows".into()],
        projects: projects
            .iter()
            .map(|id| ProjectView {
                id: id.to_string(),
                github_repo: format!("o/{id}"),
                backlog_path: "backlog".into(),
            })
            .collect(),
        version: "9.9.9".into(),
        lanes: 2,
        chore_lane: Some(2),
        lane_sizes: LaneSizes::default(),
        paused: PauseState::default(),
    })
}

/// An `exit` that does nothing, for tests that don't send Restart.
fn noop_exit() -> Arc<Exit> {
    Arc::new(|_: i32| {})
}

fn linked(url: &str) -> (tempfile::TempDir, Arc<dyn TokenStore>) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TokenStore> = Arc::new(FileStore::new(dir.path().join("credentials.json"), "retaskable"));
    store.set(url, "r1").unwrap();
    (dir, store)
}

async fn next(rx: &mut mpsc::UnboundedReceiver<Accepted>) -> Accepted {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("a connection")
        .unwrap()
}

async fn read(a: &mut Accepted) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), a.ws.next())
            .await
            .expect("a message");
        if let Message::Text(text) = msg.unwrap().unwrap() {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

async fn close(a: &mut Accepted, code: u16) {
    let frame = CloseFrame {
        code: CloseCode::from(code),
        reason: "bye".into(),
    };
    a.ws.close(Some(frame)).await.unwrap();
}

#[tokio::test]
async fn each_project_connects_with_its_token_says_hello_and_sends_a_snapshot() {
    let (url, refreshes, mut rx) = cloud().await;
    let (_dir, store) = linked(&url);
    let c = Connector::start(
        &url,
        store.clone(),
        reqwest::Client::new(),
        view(&["one", "two"]),
        timing(),
        noop_exit(),
    );
    let mut first = next(&mut rx).await;
    assert_eq!(first.uri, "/api/agents/connect?project=one");
    assert_eq!(first.protocols, "retaskable, a1");
    let hello = read(&mut first).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["machine"], "pc");
    assert_eq!(hello["agentVersion"], "9.9.9");
    assert_eq!(
        hello["project"],
        json!({ "githubRepo": "o/one", "backlogPath": "backlog" })
    );
    assert_eq!(hello["tags"], json!(["windows"]));
    let snapshot = read(&mut first).await;
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["state"]["orchestrator"]["status"], "paused");
    assert_eq!(snapshot["state"]["items"], json!([]));

    // The second project waits for the first, then shares its token: one refresh for both.
    let second = next(&mut rx).await;
    assert_eq!(second.uri, "/api/agents/connect?project=two");
    assert_eq!(second.protocols, "retaskable, a1");
    assert_eq!(refreshes.lock().unwrap().len(), 1);
    assert_eq!(store.get(&url).unwrap().as_deref(), Some("r2"));

    // A quiet spell: it says it's alive.
    assert_eq!(read(&mut first).await, json!({ "type": "alive" }));
    c.stop(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn the_snapshot_lists_the_lanes_and_is_sent_again_when_their_disk_use_changes() {
    let (url, _, mut rx) = cloud().await;
    let (_dir, store) = linked(&url);
    let view = view(&["one"]);
    let c = Connector::start(&url, store, reqwest::Client::new(), view.clone(), timing(), noop_exit());
    let mut first = next(&mut rx).await;
    read(&mut first).await; // hello
    let lanes = read(&mut first).await["state"]["lanes"].clone();
    assert_eq!(
        lanes,
        json!([
            { "k": 1, "diskGb": 0.0, "playing": false },
            { "k": 2, "diskGb": 0.0, "playing": false, "note": CHORE_LANE_NOTE },
        ])
    );
    view.lane_sizes.set("one", std::collections::BTreeMap::from([(1, 3.3)]));
    let mut again = read(&mut first).await;
    while again["type"] == "alive" {
        again = read(&mut first).await;
    }
    assert_eq!(again["type"], "snapshot");
    assert_eq!(again["state"]["lanes"][0]["diskGb"], 3.3);
    c.stop(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn before_setup_it_connects_to_its_own_hub() {
    let (url, _, mut rx) = cloud().await;
    let (_dir, store) = linked(&url);
    let c = Connector::start(&url, store, reqwest::Client::new(), view(&[]), timing(), noop_exit());
    let mut a = next(&mut rx).await;
    assert_eq!(a.uri, "/api/agents/connect");
    assert_eq!(read(&mut a).await["project"], Value::Null);
    assert_eq!(read(&mut a).await["state"]["orchestrator"]["status"], "setup");
    c.stop(Duration::from_secs(1)).await;
}

/// A fresh, still-valid command: callers override `name`/`args` as needed.
fn command(id: &str, name: &str, args: Value) -> Value {
    let now = commands::now_ms();
    json!({
        "type": "command",
        "id": id,
        "name": name,
        "args": args,
        "issuedBy": "jc",
        "issuedAt": now,
        "expiresAt": now + 60_000,
    })
}

/// Starts a connector for one project ("one") with `exit`, and returns it once its hello and
/// snapshot have been read, ready for a test to send it a command. The returned `TempDir` must
/// be kept (as `_dir`): dropping it removes the credentials file the connector's still using.
async fn connected_project(exit: Arc<Exit>) -> (Connector, Accepted, tempfile::TempDir) {
    let (url, _, mut rx) = cloud().await;
    let (dir, store) = linked(&url);
    let c = Connector::start(&url, store, reqwest::Client::new(), view(&["one"]), timing(), exit);
    let mut a = next(&mut rx).await;
    read(&mut a).await; // hello
    read(&mut a).await; // snapshot
    (c, a, dir)
}

#[tokio::test]
async fn an_unported_command_is_refused_with_a_nack() {
    let (c, mut a, _dir) = connected_project(noop_exit()).await;
    a.ws.send(Message::text(json!({ "type": "ping" }).to_string()))
        .await
        .unwrap();
    a.ws.send(Message::text(command("c1", "shoot", json!({})).to_string()))
        .await
        .unwrap();
    let nack = read(&mut a).await;
    assert_eq!(nack["type"], "nack");
    assert_eq!(nack["id"], "c1");
    assert_eq!(nack["reason"], "Not a command this agent takes yet: shoot");
    c.stop(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn pause_and_resume_need_computer_true_or_the_site_answered_it_itself() {
    let (c, mut a, _dir) = connected_project(noop_exit()).await;

    a.ws.send(Message::text(command("c1", "pause", json!({})).to_string()))
        .await
        .unwrap();
    let nack = read(&mut a).await;
    assert_eq!(
        nack["reason"],
        "retaskable.com answers this one itself; it doesn't take a computer."
    );

    a.ws.send(Message::text(
        command("c2", "pause", json!({ "computer": true })).to_string(),
    ))
    .await
    .unwrap();
    assert_eq!(read(&mut a).await, json!({ "type": "ack", "id": "c2" }));
    let snapshot = read(&mut a).await;
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(
        snapshot["state"]["orchestrator"]["pausedReason"],
        "Paused from retaskable.com by jc"
    );

    a.ws.send(Message::text(
        command("c3", "resume", json!({ "computer": true })).to_string(),
    ))
    .await
    .unwrap();
    assert_eq!(read(&mut a).await, json!({ "type": "ack", "id": "c3" }));
    let snapshot = read(&mut a).await;
    assert_eq!(snapshot["state"]["orchestrator"]["pausedReason"], NO_WORK_YET);
    c.stop(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn restart_acks_and_calls_exit_with_75() {
    let calls: Arc<StdMutex<Vec<i32>>> = Arc::new(StdMutex::new(Vec::new()));
    let kept = calls.clone();
    let exit: Arc<Exit> = Arc::new(move |code: i32| kept.lock().unwrap().push(code));
    let (c, mut a, _dir) = connected_project(exit).await;
    a.ws.send(Message::text(command("c1", "restart", json!({})).to_string()))
        .await
        .unwrap();
    assert_eq!(read(&mut a).await, json!({ "type": "ack", "id": "c1" }));
    assert_eq!(*calls.lock().unwrap(), vec![75]);
    c.stop(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn a_replayed_command_id_is_refused_the_second_time() {
    let (c, mut a, _dir) = connected_project(noop_exit()).await;
    let cmd = command("c1", "restart", json!({}));
    a.ws.send(Message::text(cmd.to_string())).await.unwrap();
    assert_eq!(read(&mut a).await, json!({ "type": "ack", "id": "c1" }));
    a.ws.send(Message::text(cmd.to_string())).await.unwrap();
    let nack = read(&mut a).await;
    assert_eq!(nack["reason"], "Already received: a command runs once.");
    c.stop(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn an_expired_command_is_refused() {
    let (c, mut a, _dir) = connected_project(noop_exit()).await;
    let issued_at = commands::now_ms() - 61_000;
    let old = json!({
        "type": "command",
        "id": "c1",
        "name": "restart",
        "args": {},
        "issuedBy": "jc",
        "issuedAt": issued_at,
        "expiresAt": issued_at + 60_000,
    });
    a.ws.send(Message::text(old.to_string())).await.unwrap();
    let nack = read(&mut a).await;
    assert!(nack["reason"].as_str().unwrap().starts_with("Expired"));
    c.stop(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn it_reconnects_after_a_close_and_takes_a_fresh_token_when_its_one_expired() {
    let (url, refreshes, mut rx) = cloud().await;
    let (_dir, store) = linked(&url);
    let c = Connector::start(
        &url,
        store.clone(),
        reqwest::Client::new(),
        view(&["one"]),
        timing(),
        noop_exit(),
    );
    let mut a = next(&mut rx).await;
    close(&mut a, close::REPLACED).await;
    // Same token, after a backoff.
    let mut b = next(&mut rx).await;
    assert_eq!(b.protocols, "retaskable, a1");
    close(&mut b, close::TOKEN_EXPIRED).await;
    let c2 = next(&mut rx).await;
    assert_eq!(c2.protocols, "retaskable, a2");
    assert_eq!(refreshes.lock().unwrap()[1]["refresh_token"], "r2");
    assert_eq!(store.get(&url).unwrap().as_deref(), Some("r3"));
    c.stop(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn an_unlinked_agent_waits_and_connects_once_linked() {
    let (url, _, mut rx) = cloud().await;
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TokenStore> = Arc::new(FileStore::new(dir.path().join("credentials.json"), "retaskable"));
    let c = Connector::start(
        &url,
        store.clone(),
        reqwest::Client::new(),
        view(&["one"]),
        timing(),
        noop_exit(),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .is_err()
    );
    // `retaskable link` in a terminal: the next idle look finds the token.
    store.set(&url, "r1").unwrap();
    assert_eq!(next(&mut rx).await.protocols, "retaskable, a1");
    c.stop(Duration::from_secs(1)).await;
}

#[test]
fn backoff_doubles_to_sixty_times_its_unit_less_up_to_half() {
    let s = Duration::from_secs(1);
    assert_eq!(backoff(0, s, 0.0), s);
    assert_eq!(backoff(3, s, 0.0), Duration::from_secs(8));
    assert_eq!(backoff(10, s, 0.0), Duration::from_secs(60));
    assert_eq!(backoff(40, s, 0.0), Duration::from_secs(60));
    assert_eq!(backoff(1, s, 1.0), s);
    let r = random();
    assert!((0.0..1.0).contains(&r));
}

#[test]
fn the_socket_url_swaps_the_scheme_and_names_the_project() {
    assert_eq!(
        connect_url("https://retaskable.com", ""),
        "wss://retaskable.com/api/agents/connect"
    );
    assert_eq!(
        connect_url("http://localhost:8787", "a b"),
        "ws://localhost:8787/api/agents/connect?project=a+b"
    );
    assert_eq!(why(close::UNLINKED, ""), "the agent was unlinked");
    assert_eq!(why(1011, "oops"), "closed (1011: oops)");
}
