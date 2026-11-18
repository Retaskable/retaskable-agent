//! A session's open input (docs/specs/expert-chat.md "Steering"; `SessionSpec.steer` in Retaskable.com's `engine.ts` and
//! `runner.ts`): a Claude Code session started with `--input-format stream-json --replay-user-messages` keeps its stdin
//! open, so a message the person sends while it works is one more line in, taken at the session's next tool boundary
//! (or as a turn of its own once the one running ends), rather than a stop and a resume. Claude Code echoes each
//! message it takes as a `user` line with `isReplay`; a `result` that follows a message not yet taken ends a turn but
//! not the session, and the input closes at the `result` that follows the last one taken.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;
use tokio::sync::{Notify, mpsc};

use super::engine::SessionResult;

/// The extra arguments a steered Claude session takes.
pub const ARGS: [&str; 3] = ["--input-format", "stream-json", "--replay-user-messages"];

/// What the loop hears of a steered session while it runs.
// A turn's result is the only variant that carries data; boxing it would only add an allocation per turn.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum SteerEvent {
    /// The session took one of the messages sent to it (not its prompt): the next in the order they were sent.
    Taken,
    /// A turn of the session ended with this result, and the session goes on with a message it has not taken yet.
    Turn(SessionResult),
}

/// The loop's end: sends a message to the open input. Clones share the one input.
#[derive(Debug, Clone)]
pub struct SteerHandle {
    tx: mpsc::UnboundedSender<String>,
    open: Arc<AtomicBool>,
}

impl SteerHandle {
    /// Writes `text` to the session as a message of the person's. False when the input is closed already (or the
    /// session is gone): nothing was sent.
    pub fn send(&self, text: &str) -> bool {
        self.open.load(Ordering::SeqCst) && self.tx.send(text.to_string()).is_ok()
    }

    /// Whether the input still takes messages.
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }
}

/// The runner's end: writes what the handle sends and tells the loop what the session took and finished.
pub struct Steer {
    input: mpsc::UnboundedReceiver<String>,
    open: Arc<AtomicBool>,
    events: mpsc::UnboundedSender<SteerEvent>,
}

/// A new open input: the loop's handle and the events it hears, and the runner's end.
pub fn channel() -> (SteerHandle, mpsc::UnboundedReceiver<SteerEvent>, Steer) {
    let (tx, input) = mpsc::unbounded_channel();
    let (events, heard) = mpsc::unbounded_channel();
    let open = Arc::new(AtomicBool::new(true));
    (
        SteerHandle { tx, open: open.clone() },
        heard,
        Steer { input, open, events },
    )
}

impl Steer {
    /// The next message sent to the session, or `None` once the handle is gone. For a stand-in that plays Claude Code.
    #[cfg(test)]
    pub(crate) async fn next_message(&mut self) -> Option<String> {
        self.input.recv().await
    }

    /// Tells the loop the session took a message (not its prompt). For a stand-in that plays Claude Code.
    #[cfg(test)]
    pub(crate) fn took(&self) {
        let _ = self.events.send(SteerEvent::Taken);
    }

    /// Tells the loop a turn ended with `result` and the session goes on. For a stand-in that plays Claude Code.
    #[cfg(test)]
    pub(crate) fn turn_ended(&self, result: SessionResult) {
        let _ = self.events.send(SteerEvent::Turn(result));
    }

    /// Closes the input, as the last result does. For a stand-in that plays Claude Code.
    #[cfg(test)]
    pub(crate) fn close(&self) {
        self.open.store(false, Ordering::SeqCst);
    }
}

/// One message as the open input takes it (`Engine.steer`).
pub fn message_line(text: &str) -> String {
    format!(
        "{}\n",
        json!({ "type": "user", "message": { "role": "user", "content": text } })
    )
}

/// Whether a line of the session's output is Claude Code echoing a message it took.
fn is_taken(line: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .is_ok_and(|v| v.get("type").and_then(Value::as_str) == Some("user") && v.get("isReplay") == Some(&json!(true)))
}

/// The runner's side of one steered session: writes the messages, counts what was taken, and decides at each result
/// whether the session goes on.
pub(super) struct Running {
    sent: Arc<AtomicUsize>,
    took: usize,
    open: Arc<AtomicBool>,
    events: mpsc::UnboundedSender<SteerEvent>,
    close: Arc<Notify>,
    writer: tokio::task::JoinHandle<()>,
}

impl Running {
    /// Starts writing: the prompt first, then every message the handle sends, until the input closes.
    pub(super) fn start(steer: Steer, mut stdin: ChildStdin, prompt: &str) -> Self {
        let sent = Arc::new(AtomicUsize::new(1));
        let close = Arc::new(Notify::new());
        let (count, stop, first) = (sent.clone(), close.clone(), message_line(prompt));
        let mut input = steer.input;
        let writer = tokio::spawn(async move {
            if stdin.write_all(first.as_bytes()).await.is_err() {
                return;
            }
            loop {
                tokio::select! {
                    () = stop.notified() => break,
                    next = input.recv() => {
                        let Some(text) = next else { break };
                        // Counted before it is written: a result read after this knows one is on its way.
                        count.fetch_add(1, Ordering::SeqCst);
                        if stdin.write_all(message_line(&text).as_bytes()).await.is_err() {
                            break;
                        }
                    }
                }
            }
            // Dropping the input is the end of it: the session exits once it has finished what it has.
            let _ = stdin.shutdown().await;
        });
        Self {
            sent,
            took: 0,
            open: steer.open,
            events: steer.events,
            close,
            writer,
        }
    }

    /// A line of the session's output: a message taken is told to the loop (all but the prompt).
    pub(super) fn line(&mut self, line: &str) {
        if is_taken(line) {
            self.took += 1;
            if self.took > 1 {
                let _ = self.events.send(SteerEvent::Taken);
            }
        }
    }

    /// A `result` was read: with a message sent and not taken yet the turn ends and the session goes on; else this was
    /// the last and the input closes.
    pub(super) fn result(&mut self, result: &SessionResult) {
        if self.took < self.sent.load(Ordering::SeqCst) {
            let _ = self.events.send(SteerEvent::Turn(result.clone()));
        } else {
            self.finish_input();
        }
    }

    fn finish_input(&self) {
        self.open.store(false, Ordering::SeqCst);
        self.close.notify_one();
    }

    /// The session is over: nothing more is taken, and the writer is done.
    pub(super) async fn end(self) {
        self.finish_input();
        let _ = self.writer.await;
    }
}

#[cfg(test)]
mod tests;
