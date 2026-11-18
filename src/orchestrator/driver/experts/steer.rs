//! Steering a running turn (docs/specs/expert-chat.md "Steering"; `turns.ts`'s `say`, `steers` and `onTaken`): a message
//! the person sends while a Claude turn runs is written to the turn's open input and taken at its next tool boundary,
//! instead of waiting for the turn to end. Its queue row is held by this computer first, and leaves the queue once the
//! session says it took it, when it is a turn of its own in the conversation. A message with pictures, or one sent when
//! the input has closed, waits for the next turn as before.
use std::collections::VecDeque;

use tokio::sync::mpsc::UnboundedReceiver;

use super::*;
use crate::engines::steer::{self, Steer, SteerEvent, SteerHandle};

/// The loop's end of a running turn's open input.
pub(super) struct Steering {
    handle: SteerHandle,
    heard: UnboundedReceiver<SteerEvent>,
    /// The rows written to the input and not taken yet, oldest first.
    pending: VecDeque<Queued>,
}

/// A new open input for a turn: the loop's end and the host's.
pub(super) fn open() -> (Steering, Steer) {
    let (handle, heard, steer) = steer::channel();
    (
        Steering {
            handle,
            heard,
            pending: VecDeque::new(),
        },
        steer,
    )
}

/// Writes the message `row` to the session's running turn, when it can: the input is open, the message has no
/// pictures and nothing queued is ahead of it but what is written already. The row is held by this computer first.
/// Whether it was sent; if not it waits in the queue for the next turn.
pub(super) async fn try_send(cx: &Cx<'_>, item: &str, s: &mut Session, row: &Queued) -> bool {
    let ahead_is_sent = |s: &Session, st: &Steering| {
        s.queue
            .iter()
            .all(|q| q.id == row.id || st.pending.iter().any(|p| p.id == q.id))
    };
    match s.turn.as_ref().and_then(|t| t.steering.as_ref()) {
        Some(st) if st.handle.is_open() && row.picture_ids.is_empty() && ahead_is_sent(s, st) => {}
        _ => return false,
    }
    let Some(taken) = cx.queue.take(item, std::slice::from_ref(row)).await else {
        return false;
    };
    let text = taken.first().map_or_else(|| row.text.clone(), |t| t.text.clone());
    let mut parts = s.results.clone();
    parts.push(text);
    let sent =
        (s.turn.as_mut().and_then(|t| t.steering.as_mut())).is_some_and(|st| st.handle.send(&parts.join("\n\n")));
    if sent {
        // What helpers found goes with it, as with a message that starts a turn.
        s.results.clear();
        s.tell_now = false;
        if let Some(st) = s.turn.as_mut().and_then(|t| t.steering.as_mut()) {
            st.pending.push_back(row.clone());
        }
    }
    sent
}

/// Takes in what the running turn's session has told: a message it took leaves the queue and is the person's turn in
/// the conversation; a turn it ended before the last has its answer noted.
pub(super) async fn take_in(cx: &Cx<'_>, item: &str, s: &mut Session, st: &mut Steering) {
    while let Ok(event) = st.heard.try_recv() {
        match event {
            SteerEvent::Taken => {
                let Some(row) = st.pending.pop_front() else { continue };
                s.queue.retain(|q| q.id != row.id);
                cx.queue.done(item, std::slice::from_ref(&row.id)).await;
                s.consumed.insert(row.id.clone());
                s.talk.push(format!("The person: {}", row.text));
            }
            SteerEvent::Turn(result) => {
                let reply = expert::parse(&result.structured.unwrap_or_default());
                let said = if result.ok {
                    expert::answer_line(&reply)
                } else {
                    format!("The turn failed: {}", result.error.unwrap_or(result.subtype))
                };
                if !said.is_empty() {
                    s.talk.push(said.clone());
                }
                cx.note(item, &s.lease, &said).await;
            }
        }
    }
}

/// [`take_in`] for the session's running turn, if it is steered.
pub(super) async fn poll(cx: &Cx<'_>, item: &str, s: &mut Session) {
    let Some(mut st) = s.turn.as_mut().and_then(|t| t.steering.take()) else {
        return;
    };
    take_in(cx, item, s, &mut st).await;
    if let Some(t) = s.turn.as_mut() {
        t.steering = Some(st);
    }
}
