use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

use axum::{Json, Router, extract::State, routing::post};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};

use super::*;
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
    })
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
    );
    let mut first = next(&mut rx).await;
    assert_eq!(first.uri, "/api/agents/connect?project=one");
    assert_eq!(first.protocols, "retaskable, a1");
    let hello = read(&mut first).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["machine"], "pc");
    assert_eq!(hello["agentVersion"], env!("CARGO_PKG_VERSION"));
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
async fn before_setup_it_connects_to_its_own_hub() {
    let (url, _, mut rx) = cloud().await;
    let (_dir, store) = linked(&url);
    let c = Connector::start(&url, store, reqwest::Client::new(), view(&[]), timing());
    let mut a = next(&mut rx).await;
    assert_eq!(a.uri, "/api/agents/connect");
    assert_eq!(read(&mut a).await["project"], Value::Null);
    assert_eq!(read(&mut a).await["state"]["orchestrator"]["status"], "setup");
    c.stop(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn a_command_is_refused_with_a_nack() {
    let (url, _, mut rx) = cloud().await;
    let (_dir, store) = linked(&url);
    let c = Connector::start(&url, store, reqwest::Client::new(), view(&["one"]), timing());
    let mut a = next(&mut rx).await;
    read(&mut a).await;
    read(&mut a).await;
    let command = json!({ "type": "command", "id": "c1", "name": "pause", "args": {}, "issuedBy": "jc" });
    a.ws.send(Message::text(json!({ "type": "ping" }).to_string()))
        .await
        .unwrap();
    a.ws.send(Message::text(command.to_string())).await.unwrap();
    let nack = read(&mut a).await;
    assert_eq!(nack["type"], "nack");
    assert_eq!(nack["id"], "c1");
    assert_eq!(nack["reason"], "Not a command this agent takes yet: pause");
    c.stop(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn it_reconnects_after_a_close_and_takes_a_fresh_token_when_its_one_expired() {
    let (url, refreshes, mut rx) = cloud().await;
    let (_dir, store) = linked(&url);
    let c = Connector::start(&url, store.clone(), reqwest::Client::new(), view(&["one"]), timing());
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
    let c = Connector::start(&url, store.clone(), reqwest::Client::new(), view(&["one"]), timing());
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
