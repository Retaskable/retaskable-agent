//! How the socket reaches the store: a [`Transport`] opens one connection and hands back two channels, so the
//! loop never touches a WebSocket and a test runs it against channels it holds. [`Wire`] is the real one,
//! tokio-tungstenite with the store's access token in the `Authorization` header (Retaskable.com's
//! `agent/src/backlogSocket.ts`, `open`).
use futures_util::future::BoxFuture;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, http::HeaderValue};

use crate::link::cause;

/// What comes in on an open socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Text(String),
    /// The socket ended, with the close code and reason (1006 when it just dropped).
    Closed(u16, String),
}

/// What goes out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    Text(String),
    Close(u16, String),
}

/// One open socket: its incoming frames and a way to send.
pub struct Conn {
    pub incoming: mpsc::Receiver<Incoming>,
    pub outgoing: mpsc::Sender<Outgoing>,
}

/// Opens sockets. `Err` says why one couldn't be opened.
pub trait Transport: Send + Sync + 'static {
    fn open(&self, url: &str, access_token: &str) -> BoxFuture<'static, Result<Conn, String>>;
}

/// The real transport.
pub struct Wire;

impl Transport for Wire {
    fn open(&self, url: &str, access_token: &str) -> BoxFuture<'static, Result<Conn, String>> {
        let (url, token) = (url.to_string(), access_token.to_string());
        Box::pin(async move {
            let mut request = url.as_str().into_client_request().map_err(|e| cause(&e))?;
            let bearer = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| cause(&e))?;
            request.headers_mut().insert("Authorization", bearer);
            let (ws, _) = tokio_tungstenite::connect_async(request).await.map_err(|e| cause(&e))?;
            let (mut sink, mut stream) = ws.split();
            let (in_tx, incoming) = mpsc::channel(64);
            let (outgoing, mut out_rx) = mpsc::channel::<Outgoing>(16);
            tokio::spawn(async move {
                let closed = loop {
                    tokio::select! {
                        frame = stream.next() => match frame {
                            Some(Ok(Message::Text(text))) => {
                                if in_tx.send(Incoming::Text(text.to_string())).await.is_err() {
                                    break None;
                                }
                            }
                            Some(Ok(Message::Close(frame))) => {
                                break Some(match frame {
                                    Some(f) => (u16::from(f.code), f.reason.to_string()),
                                    None => (1005, String::new()),
                                });
                            }
                            Some(Ok(_)) => {} // pings are answered by the library, the rest isn't sent
                            Some(Err(e)) => break Some((1006, cause(&e))),
                            None => break Some((1006, String::new())),
                        },
                        out = out_rx.recv() => match out {
                            Some(Outgoing::Text(text)) => {
                                if sink.send(Message::text(text)).await.is_err() {
                                    break Some((1006, "couldn't send".into()));
                                }
                            }
                            Some(Outgoing::Close(code, reason)) => {
                                let frame = CloseFrame { code: CloseCode::from(code), reason: reason.into() };
                                let _ = sink.send(Message::Close(Some(frame))).await;
                                break None;
                            }
                            None => break None, // the loop let go of it
                        },
                    }
                };
                if let Some((code, reason)) = closed {
                    let _ = in_tx.send(Incoming::Closed(code, reason)).await;
                }
            });
            Ok(Conn { incoming, outgoing })
        })
    }
}
