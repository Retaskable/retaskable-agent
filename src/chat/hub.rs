//! The Chat Agent's socket to retaskable.com's hub, one per project (chat-agent-process.md section 3): a `hello` with
//! `role: "chat"`, then only `ack`, `nack`, `alive` and `chat_report` back. The hub sends `ask` commands; each is checked
//! and answered at once, then carried out in its own task so a slow answer never holds the socket.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Semaphore, mpsc};
use tokio_tungstenite::tungstenite::Message;

use super::answer::Project;
use super::ask::{self, Command, Deps, Intake, Seen, taken};
use super::board::{self, Board};
use super::classify;
use super::claude::Runner;
use super::nack;
use super::report::chat_report;
use crate::connector::{Socket, Timing, backoff, close, connect_url, open, random};
use crate::link::{DeviceTokens, TokenAnswer};
use crate::telemetry::events::now_ms;

/// Asks carried out at once on one project. More are refused rather than queued behind a five-minute wait.
pub const MAX_ASKS: usize = 4;

/// Everything one project's Chat Agent needs, shared by the asks it carries out.
pub struct Chat<B, R> {
    pub board: B,
    pub runner: R,
    pub project: Project,
    pub cwd: PathBuf,
    pub host: String,
    pub classify_timeout: Duration,
    pub code_wait: Duration,
    pub code_poll: Duration,
    pub room: Arc<Semaphore>,
}

impl<B, R> Chat<B, R> {
    pub fn new(board: B, runner: R, project: Project, cwd: PathBuf, host: String) -> Self {
        Chat {
            board,
            runner,
            project,
            cwd,
            host,
            classify_timeout: classify::TIMEOUT,
            code_wait: board::CODE_QUESTION_WAIT,
            code_poll: board::CODE_POLL,
            room: Arc::new(Semaphore::new(MAX_ASKS)),
        }
    }
}

type Log = Arc<dyn Fn(String) + Send + Sync>;

/// The ack or nack for a message from the hub, with a task started for an ask that was taken. `None`: nothing to say.
fn receive<B: Board + 'static, R: Runner + 'static>(
    text: &str,
    chat: &Arc<Chat<B, R>>,
    seen: &mut Seen,
    out: &mpsc::UnboundedSender<Value>,
) -> Option<Value> {
    let msg: Value = serde_json::from_str(text).ok()?;
    if msg.get("type").and_then(Value::as_str) != Some("command") {
        return None;
    }
    let cmd = Command::parse(&msg)?;
    let now = now_ms();
    let at = crate::orchestrator::live::now_iso8601();
    let ask = match ask::intake(&cmd, seen, &chat.runner, &chat.host, now, at) {
        Intake::Refuse(reply) => return Some(reply),
        Intake::Take(ask) => ask,
    };
    let Ok(permit) = chat.room.clone().try_acquire_owned() else {
        return Some(nack(
            &cmd.id,
            "The Chat Agent is busy with other messages. Send it again in a moment.",
        ));
    };
    let reply = taken(&ask);
    let (chat, out) = (chat.clone(), out.clone());
    tokio::spawn(async move {
        let _permit = permit;
        let _ = out.send(chat_report(&ask, None));
        let d = Deps {
            board: &chat.board,
            runner: &chat.runner,
            project: &chat.project,
            cwd: &chat.cwd,
            host: &chat.host,
            classify_timeout: chat.classify_timeout,
            code_wait: chat.code_wait,
            code_poll: chat.code_poll,
        };
        let finished = ask::process(&d, &ask).await;
        let _ = out.send(chat_report(&ask, Some(&finished)));
    });
    Some(reply)
}

/// One open connection: hello, then the hub's commands, and `alive` in a quiet minute. The close code and reason.
async fn session<B: Board + 'static, R: Runner + 'static>(
    ws: Socket,
    chat: &Arc<Chat<B, R>>,
    hello: &Value,
    idle: Duration,
    seen: &mut Seen,
) -> (u16, String) {
    let (mut tx, mut rx) = ws.split();
    if let Err(e) = tx.send(Message::text(hello.to_string())).await {
        return (1006, e.to_string());
    }
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
    let mut alive = tokio::time::interval_at(tokio::time::Instant::now() + idle, idle);
    loop {
        let reply = tokio::select! {
            msg = rx.next() => match msg {
                Some(Ok(Message::Text(text))) => receive(&text, chat, seen, &out_tx),
                Some(Ok(Message::Close(frame))) => {
                    return frame.map_or((1005, String::new()), |f| (u16::from(f.code), f.reason.to_string()));
                }
                Some(Ok(_)) => None,
                Some(Err(e)) => return (1006, e.to_string()),
                None => return (1006, String::new()),
            },
            Some(msg) = out_rx.recv() => Some(msg),
            _ = alive.tick() => Some(json!({ "type": "alive" })),
        };
        if let Some(reply) = reply
            && let Err(e) = tx.send(Message::text(reply.to_string())).await
        {
            return (1006, e.to_string());
        }
    }
}

/// Keeps one project's socket open for as long as the process runs: reconnects with backoff, takes a fresh token when
/// the hub says the old one ran out, and waits to be linked or given the right role when it says so.
pub async fn run_project<B: Board + 'static, R: Runner + 'static>(
    chat: Arc<Chat<B, R>>,
    cloud: Arc<DeviceTokens>,
    cloud_url: String,
    project_id: String,
    hello: Value,
    timing: Timing,
    log: Log,
) {
    let mut seen = Seen::default();
    let mut attempt = 0u32;
    let mut stale: Option<String> = None;
    loop {
        let token = match cloud.access_token(stale.take().as_deref()).await {
            TokenAnswer::Token { access_token, .. } => access_token,
            TokenAnswer::Idle => {
                log("Chat Agent isn't linked to retaskable.com. Run `retaskable-chat-agent link`.".into());
                tokio::time::sleep(timing.idle).await;
                continue;
            }
            TokenAnswer::Unreachable => {
                log("Chat Agent couldn't reach retaskable.com to sign in.".into());
                tokio::time::sleep(backoff(attempt, timing.backoff, random())).await;
                attempt = attempt.saturating_add(1);
                continue;
            }
        };
        let url = connect_url(&cloud_url, &project_id);
        let opened = tokio::time::timeout(timing.connect, open(&url, &token)).await;
        let started = Instant::now();
        match opened {
            Ok(Ok(ws)) => {
                log(format!("Chat Agent connected for project {project_id}."));
                let (code, reason) = session(ws, &chat, &hello, timing.idle, &mut seen).await;
                log(format!(
                    "Chat Agent's connection closed ({code}{}).",
                    if reason.is_empty() {
                        String::new()
                    } else {
                        format!(": {reason}")
                    }
                ));
                match code {
                    close::TOKEN_EXPIRED => stale = Some(token),
                    close::UNLINKED => {
                        let _ = cloud.forget();
                        tokio::time::sleep(timing.idle).await;
                        continue;
                    }
                    close::WRONG_ROLE => {
                        log("retaskable.com says this link isn't a Chat Agent's. Run `retaskable-chat-agent link` again.".into());
                        let _ = cloud.forget();
                        tokio::time::sleep(timing.idle).await;
                        continue;
                    }
                    _ => {}
                }
            }
            Ok(Err(e)) => log(format!("Chat Agent couldn't connect: {e}")),
            Err(_) => log("Chat Agent's connect took too long.".into()),
        }
        attempt = if started.elapsed() >= timing.stable {
            0
        } else {
            attempt.saturating_add(1)
        };
        tokio::time::sleep(backoff(attempt, timing.backoff, random())).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::board::fake::Fake as FakeBoard;
    use crate::chat::claude::fake::Fake;
    use crate::connector::SUBPROTOCOL;
    use crate::engines::engine::test_result;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
    use tokio_tungstenite::tungstenite::http::HeaderValue;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    #[allow(clippy::result_large_err)] // the handshake callback's own error type
    async fn serve() -> (String, tokio::task::JoinHandle<Vec<Value>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let callback = |_: &Request, mut res: Response| {
                res.headers_mut()
                    .insert("Sec-WebSocket-Protocol", HeaderValue::from_static(SUBPROTOCOL));
                Ok(res)
            };
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, callback).await.unwrap();
            let now = now_ms();
            let command = |id: &str, name: &str, args: Value| {
                json!({ "type": "command", "id": id, "name": name, "args": args,
                        "issuedBy": "ada", "issuedAt": now, "expiresAt": now + 60_000 })
            };
            let mut got = Vec::new();
            got.push(serde_json::from_str::<Value>(&ws.next().await.unwrap().unwrap().into_text().unwrap()).unwrap());
            for c in [
                command("a1", "pause", json!({})),
                command("a2", "ask", json!({ "question": "what is building?" })),
            ] {
                ws.send(Message::text(c.to_string())).await.unwrap();
            }
            // nack for the pause, ack and thinking and the finished report for the ask.
            while got.len() < 5 {
                let m = ws.next().await.unwrap().unwrap().into_text().unwrap();
                got.push(serde_json::from_str(&m).unwrap());
            }
            let frame = CloseFrame {
                code: CloseCode::from(4004),
                reason: "wrong role".into(),
            };
            ws.send(Message::Close(Some(frame))).await.unwrap();
            got
        });
        (url, server)
    }

    #[tokio::test]
    async fn a_session_says_hello_nacks_other_commands_and_carries_an_ask_to_its_report() {
        let (url, server) = serve().await;
        let ws = open(&url, "token").await.unwrap();
        let runner = Fake::new(vec![
            Ok(test_result(
                true,
                "success",
                Some(json!({ "kind": "question", "confidence": 0.9 })),
                None,
            )),
            Ok(test_result(
                true,
                "success",
                Some(json!({ "answer": "Dark mode.", "actions": [] })),
                None,
            )),
        ]);
        let board = FakeBoard::new(|_, _, _| Ok(json!([])));
        let chat = Arc::new(Chat::new(
            board,
            runner,
            Project::default(),
            PathBuf::from("."),
            "box".into(),
        ));
        let hello = json!({ "type": "hello", "role": "chat" });
        let (code, reason) = session(ws, &chat, &hello, Duration::from_secs(60), &mut Seen::default()).await;
        assert_eq!((code, reason.as_str()), (4004, "wrong role"));
        let got = server.await.unwrap();
        assert_eq!(got[0]["role"], "chat");
        let by_type = |t: &str| got.iter().filter(|m| m["type"] == t).cloned().collect::<Vec<_>>();
        let nacks = by_type("nack");
        assert_eq!((nacks.len(), nacks[0]["id"].clone()), (1, json!("a1")));
        assert_eq!(by_type("ack")[0]["id"], "a2");
        let reports = by_type("chat_report");
        let states: Vec<_> = reports.iter().map(|r| r["ask"]["state"].clone()).collect();
        assert_eq!(states, [json!("thinking"), json!("done")]);
        assert_eq!(reports[1]["ask"]["answer"], "Dark mode.");
    }

    #[tokio::test]
    async fn asks_past_the_room_are_nacked_busy() {
        let runner = Fake::new(vec![]);
        let board = FakeBoard::new(|_, _, _| Ok(json!([])));
        let chat = Arc::new(Chat::new(
            board,
            runner,
            Project::default(),
            PathBuf::from("."),
            "box".into(),
        ));
        chat.room.forget_permits(MAX_ASKS);
        let (out, _rx) = mpsc::unbounded_channel();
        let now = now_ms();
        let text = json!({ "type": "command", "id": "x", "name": "ask", "args": { "question": "hi" },
                           "issuedBy": "ada", "issuedAt": now, "expiresAt": now + 60_000 })
        .to_string();
        let reply = receive(&text, &chat, &mut Seen::default(), &out).unwrap();
        assert_eq!(reply["type"], "nack");
        assert!(reply["reason"].as_str().unwrap().contains("busy"));
    }

    #[tokio::test]
    async fn anything_but_a_command_is_ignored() {
        let chat = Arc::new(Chat::new(
            FakeBoard::new(|_, _, _| Ok(json!([]))),
            Fake::new(vec![]),
            Project::default(),
            PathBuf::from("."),
            "box".into(),
        ));
        let (out, _rx) = mpsc::unbounded_channel();
        for text in [
            "not json",
            r#"{"type":"welcome"}"#,
            r#"{"type":"command","name":"ask"}"#,
        ] {
            assert!(receive(text, &chat, &mut Seen::default(), &out).is_none());
        }
    }
}
