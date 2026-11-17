//! An idea's board, open (TS `designBoard.ts` `openIdeaBoard`): what is on it, and a way to write operations that
//! waits for the hub to have them. The hub takes 20 messages a second on a socket, so one operation goes at a time
//! with a gap, as the board's own clients write, and each is acked by sender and sequence number
//! (`shared/boardProtocol.ts`: `op` with `sender` and `seq`, answered `ack`; the first message is the whole `doc`).
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::timeout;

use super::error::DesignError;
use super::things::Thing;
use crate::backlog_socket::{Conn, Incoming, Outgoing, Transport};

/// How long opening the board and writing to it may take, and the gap between two messages.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub send_gap: Duration,
    pub open: Duration,
    pub write: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            send_gap: Duration::from_millis(60),
            open: Duration::from_secs(60),
            write: Duration::from_secs(60),
        }
    }
}

/// One change to the board's things.
#[derive(Debug, Clone)]
pub enum Op {
    Add(Thing),
    Remove(String),
}

impl Op {
    fn wire(&self, sender: &str, seq: u64) -> String {
        let op = match self {
            Op::Add(t) => json!({"kind": "add", "coll": "things", "item": Value::Object(t.0.clone())}),
            Op::Remove(id) => json!({"kind": "remove", "coll": "things", "id": id}),
        };
        json!({"type": "op", "sender": sender, "seq": seq, "op": op}).to_string()
    }
}

pub struct IdeaBoard {
    /// What is on the board when it opened.
    pub things: Vec<Thing>,
    conn: Conn,
    sender: String,
    seq: u64,
    acked: u64,
    timing: Timing,
}

/// A number in base 36, as JavaScript's `toString(36)` writes it.
pub fn base36(mut n: u64) -> String {
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(char::from_digit((n % 36) as u32, 36).unwrap_or('0'));
        n /= 36;
    }
    out.iter().rev().collect()
}

impl IdeaBoard {
    /// Opens the board's socket at `url` (the store's `/board`, with a ticket) and waits for its first `doc`. `sender`
    /// names this open board for the hub's acks.
    pub async fn open(
        transport: &dyn Transport,
        url: &str,
        access_token: &str,
        sender: String,
        timing: Timing,
    ) -> Result<Self, DesignError> {
        let mut conn = match timeout(timing.open, transport.open(url, access_token)).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(why)) => return Err(DesignError::conflict(format!("Couldn't open the board: {why}"))),
            Err(_) => return Err(DesignError::conflict("The board didn't open in time.")),
        };
        let first = timeout(timing.open, async {
            loop {
                match conn.incoming.recv().await {
                    Some(Incoming::Text(text)) => {
                        let Ok(msg) = serde_json::from_str::<Value>(&text) else {
                            continue;
                        };
                        if msg.get("type").and_then(Value::as_str) == Some("doc") {
                            return Ok(msg);
                        }
                    }
                    Some(Incoming::Closed(..)) | None => {
                        return Err(DesignError::conflict("The board closed before it opened."));
                    }
                }
            }
        })
        .await
        .map_err(|_| DesignError::conflict("The board didn't open in time."))??;
        let things = first
            .pointer("/doc/things")
            .and_then(Value::as_object)
            .map(|m| m.values().filter_map(Thing::from_value).collect())
            .unwrap_or_default();
        Ok(Self {
            things,
            conn,
            sender,
            seq: 0,
            acked: 0,
            timing,
        })
    }

    /// Writes `ops` one at a time and waits until the hub has acked the last.
    pub async fn write(&mut self, ops: &[Op]) -> Result<(), DesignError> {
        if ops.is_empty() {
            return Ok(());
        }
        let gap = self.timing.send_gap;
        let sent = timeout(self.timing.write, async {
            for op in ops {
                self.seq += 1;
                let text = op.wire(&self.sender, self.seq);
                if self.conn.outgoing.send(Outgoing::Text(text)).await.is_err() {
                    return Err(DesignError::conflict(
                        "The board closed while the designs were written.",
                    ));
                }
                tokio::time::sleep(gap).await;
            }
            while self.acked < self.seq {
                match self.conn.incoming.recv().await {
                    Some(Incoming::Text(text)) => {
                        let Ok(msg) = serde_json::from_str::<Value>(&text) else {
                            continue;
                        };
                        if msg.get("type").and_then(Value::as_str) == Some("ack")
                            && msg.get("sender").and_then(Value::as_str) == Some(self.sender.as_str())
                        {
                            self.acked = self.acked.max(msg.get("seq").and_then(Value::as_u64).unwrap_or(0));
                        }
                    }
                    Some(Incoming::Closed(..)) | None => {
                        return Err(DesignError::conflict(
                            "The board closed while the designs were written.",
                        ));
                    }
                }
            }
            Ok(())
        })
        .await;
        sent.map_err(|_| DesignError::conflict("The board didn't take the designs in time."))?
    }

    pub async fn close(self) {
        let _ = self.conn.outgoing.send(Outgoing::Close(1000, "done".into())).await;
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A board for tests: a transport that hands out a socket holding `things`, and acks what it is sent.
    use std::sync::{Arc, Mutex};

    use futures_util::future::BoxFuture;
    use tokio::sync::mpsc;

    use super::*;

    pub struct Board {
        pub things: Vec<Value>,
        /// What was written, parsed.
        pub sent: Arc<Mutex<Vec<Value>>>,
        pub opened: Arc<Mutex<Vec<(String, String)>>>,
        /// Ack nothing: the write must time out.
        pub silent: bool,
        /// Close right after the doc.
        pub close_after_doc: bool,
    }

    impl Board {
        pub fn new(things: Vec<Value>) -> Self {
            Self {
                things,
                sent: Default::default(),
                opened: Default::default(),
                silent: false,
                close_after_doc: false,
            }
        }
    }

    impl Transport for Board {
        fn open(&self, url: &str, access_token: &str) -> BoxFuture<'static, Result<Conn, String>> {
            self.opened
                .lock()
                .unwrap()
                .push((url.to_string(), access_token.to_string()));
            let (in_tx, incoming) = mpsc::channel(256);
            let (outgoing, mut out_rx) = mpsc::channel::<Outgoing>(256);
            let things: serde_json::Map<String, Value> = self
                .things
                .iter()
                .map(|t| (t["id"].as_str().unwrap().to_string(), t.clone()))
                .collect();
            let (sent, silent, close) = (self.sent.clone(), self.silent, self.close_after_doc);
            tokio::spawn(async move {
                let doc = json!({"type": "doc", "now": 1, "doc": {"version": 1, "things": things}});
                let _ = in_tx.send(Incoming::Text(doc.to_string())).await;
                if close {
                    let _ = in_tx.send(Incoming::Closed(1006, String::new())).await;
                    return;
                }
                while let Some(Outgoing::Text(text)) = out_rx.recv().await {
                    let m: Value = serde_json::from_str(&text).unwrap();
                    sent.lock().unwrap().push(m.clone());
                    if !silent {
                        let ack = json!({"type": "ack", "sender": m["sender"], "seq": m["seq"]});
                        let _ = in_tx.send(Incoming::Text(ack.to_string())).await;
                    }
                }
            });
            Box::pin(async move { Ok(Conn { incoming, outgoing }) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Board;
    use super::*;

    fn fast() -> Timing {
        Timing {
            send_gap: Duration::ZERO,
            open: Duration::from_millis(500),
            write: Duration::from_millis(300),
        }
    }

    fn sticky(id: &str) -> Value {
        json!({"id": id, "kind": "sticky", "x": 0, "y": 0, "w": 200, "h": 200, "text": "hi", "colour": 0})
    }

    #[tokio::test]
    async fn opens_on_the_first_doc_and_writes_one_op_at_a_time_in_order_until_acked() {
        let fake = Board::new(vec![sticky("a")]);
        let mut board = IdeaBoard::open(&fake, "wss://s/b", "tok", "agent-x".into(), fast())
            .await
            .unwrap();
        assert_eq!(board.things.len(), 1);
        assert_eq!(
            fake.opened.lock().unwrap()[0],
            ("wss://s/b".to_string(), "tok".to_string())
        );
        let new = Thing::from_value(&sticky("b")).unwrap();
        board.write(&[Op::Add(new), Op::Remove("a".into())]).await.unwrap();
        let sent = fake.sent.lock().unwrap().clone();
        assert_eq!(
            sent.iter().map(|m| m["seq"].as_u64().unwrap()).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(sent[0]["op"]["kind"], "add");
        assert_eq!(sent[0]["op"]["coll"], "things");
        assert_eq!(sent[0]["sender"], "agent-x");
        assert_eq!(
            (sent[1]["op"]["kind"].as_str(), sent[1]["op"]["id"].as_str()),
            (Some("remove"), Some("a"))
        );
        // A second write goes on counting.
        board.write(&[Op::Remove("b".into())]).await.unwrap();
        assert_eq!(fake.sent.lock().unwrap()[2]["seq"], 3);
        board.close().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_board_that_closes_before_its_doc_or_never_acks_is_a_reason_not_a_hang() {
        let mut gone = Board::new(vec![]);
        gone.close_after_doc = true;
        let mut open = IdeaBoard::open(&gone, "u", "t", "s".into(), fast()).await.unwrap();
        let why = open.write(&[Op::Remove("a".into())]).await.unwrap_err();
        assert!(why.message.contains("closed while the designs were written"), "{why}");
        let mut quiet = Board::new(vec![]);
        quiet.silent = true;
        let mut open = IdeaBoard::open(&quiet, "u", "t", "s".into(), fast()).await.unwrap();
        let why = open.write(&[Op::Remove("a".into())]).await.unwrap_err();
        assert_eq!(why.message, "The board didn't take the designs in time.");
        assert_eq!(why.status, 409);
    }

    #[test]
    fn base36_is_as_javascript_writes_it() {
        assert_eq!(
            (base36(0), base36(35), base36(36), base36(1_700_000_000_000)),
            ("0".into(), "z".into(), "10".into(), "loyw3v28".into())
        );
    }
}
