//! A session's helpers (docs/specs/expert-chat.md "Helpers"; `agent/src/expert/helpers.ts`): separate scout runs on the
//! lighter tier, asked for by the person (`expert_helper`) or by a turn's `helpers`. A `read` helper runs read-only in
//! the session's own lane beside its turn; a `lane` helper waits for a free lane (never taking one over or adding one)
//! and works there on `<the session's branch>-h<n>`, from the session's last commit. Its result is noted in the
//! history and goes to the session as a message: at once when the session is idle in its lane, else with the person's
//! next message. A helper stops with the session (`expert_end`, a park, an idle park), never with `expert_stop`.
use super::*;
use crate::expert::helpers::{self as words, Ended};

/// One helper of a session, waiting for a lane or running.
pub(super) struct Helper {
    n: u32,
    task: String,
    /// It has a lane of its own, on its own branch, and may change files.
    own: bool,
    stop: Arc<Notify>,
    state: State,
}

enum State {
    /// Waiting for a free lane (`own` only); whether the history says so yet.
    Waiting { said: bool },
    Running {
        task: JoinHandle<Ended>,
        /// The lane it holds in the pool, when it is its own.
        lane: Option<u32>,
    },
}

/// Where and how a helper runs.
pub struct HelperJob {
    pub project: ProjectConfig,
    pub item: String,
    pub n: u32,
    pub task: String,
    /// The session's branch, which a lane helper's own branch is made from.
    pub from: String,
    /// The lane it works in: the session's own for a read helper, its own for a lane helper.
    pub lane: u32,
    pub own: bool,
    /// Fired to stop it; the host cuts its processes and answers [`Ended::Stopped`].
    pub stop: Arc<Notify>,
    pub sessions: Sessions,
}

fn holder_item(item: &str, n: u32) -> String {
    format!("{item}/{}", words::name(n))
}

/// Starts a helper for the session and says which (`startHelper`). A read helper runs now; a lane helper waits for a
/// lane (`poll`).
pub(super) async fn start(cx: &Cx<'_>, item: &str, s: &mut Session, task: &str, own: bool) -> Result<String, String> {
    if s.lane.is_none() {
        return Err("This session is parked: a helper needs its lane. Send a message first.".into());
    }
    if s.helping.len() >= words::MAX {
        let max = words::MAX;
        return Err(format!(
            "{max} helpers are running for this session already: wait for one to finish."
        ));
    }
    s.helpers += 1;
    let n = s.helpers;
    cx.note(item, &s.lease, &words::started(n, task)).await;
    let stop = Arc::new(Notify::new());
    let state = if own {
        State::Waiting { said: false }
    } else {
        let lane = s.lane.expect("checked above");
        let job = job(cx, item, s, n, task, lane, false, &stop);
        State::Running {
            task: tokio::spawn(cx.host.helper(job)),
            lane: None,
        }
    };
    s.helping.push(Helper {
        n,
        task: task.to_string(),
        own,
        stop,
        state,
    });
    Ok(words::name(n))
}

#[allow(clippy::too_many_arguments)]
fn job(
    cx: &Cx<'_>,
    item: &str,
    s: &Session,
    n: u32,
    task: &str,
    lane: u32,
    own: bool,
    stop: &Arc<Notify>,
) -> HelperJob {
    HelperJob {
        project: cx.project.clone(),
        item: item.to_string(),
        n,
        task: task.to_string(),
        from: s.branch.clone(),
        lane,
        own,
        stop: stop.clone(),
        sessions: cx.sessions.clone(),
    }
}

/// Moves the helpers on: a lane helper takes a free lane when there is one, and one that has ended is noted and its
/// result given to the session.
pub(super) async fn poll(cx: &Cx<'_>, ex: &mut Experts, pool: &mut LanePool) {
    for (id, s) in ex.sessions.iter_mut() {
        let mut i = 0;
        while i < s.helping.len() {
            let ended = match &s.helping[i].state {
                State::Waiting { .. } => None,
                State::Running { task, .. } => task.is_finished().then_some(()),
            };
            if ended.is_some() {
                let h = s.helping.remove(i);
                let State::Running { task, lane } = h.state else {
                    continue;
                };
                let end = task
                    .await
                    .unwrap_or_else(|e| Ended::Failed(format!("the helper crashed: {e}")));
                finished(cx, pool, id, s, h.n, &h.task, h.own, lane, &end).await;
                continue;
            }
            if let State::Waiting { said } = s.helping[i].state {
                let (n, task) = (s.helping[i].n, s.helping[i].task.clone());
                let want = Want {
                    item: holder_item(id, n),
                    ..cx.want(id)
                };
                match pool.claim(&want) {
                    Some(lane) => {
                        let stop = s.helping[i].stop.clone();
                        let job = job(cx, id, s, n, &task, lane, true, &stop);
                        let on = words::branch(&s.branch, n);
                        cx.note(id, &s.lease, &format!("Helper {n} is in lane {lane} on {on}"))
                            .await;
                        s.helping[i].state = State::Running {
                            task: tokio::spawn(cx.host.helper(job)),
                            lane: Some(lane),
                        };
                    }
                    None if !said => {
                        s.helping[i].state = State::Waiting { said: true };
                        cx.note(id, &s.lease, &format!("Helper {n} is waiting for a free lane"))
                            .await;
                    }
                    None => {}
                }
            }
            i += 1;
        }
    }
}

/// A helper has ended: its lane goes back, its end is noted and its result is the session's.
#[allow(clippy::too_many_arguments)]
async fn finished(
    cx: &Cx<'_>,
    pool: &mut LanePool,
    id: &str,
    s: &mut Session,
    n: u32,
    task: &str,
    own: bool,
    lane: Option<u32>,
    end: &Ended,
) {
    if let Some(lane) = lane {
        pool.release(
            lane,
            &Holder::Item {
                project: cx.id.to_string(),
                item: holder_item(id, n),
            },
        );
    }
    let on = own.then(|| words::branch(&s.branch, n));
    let (note, told) = words::end_lines(n, task, end, on.as_deref());
    cx.note(id, &s.lease, &note).await;
    if let Some(text) = told {
        deliver(s, text);
    }
}

/// A result goes to the session at once when it's idle in its lane, else with the person's next message: it never
/// interrupts a turn, nor a question waiting for its answer (`deliver`).
fn deliver(s: &mut Session, text: String) {
    s.tell_now |= s.lane.is_some() && s.turn.is_none() && s.detail == Detail::Idle;
    s.results.push(text);
}

/// Stops the session's helpers, those running and those waiting for a lane, and drops the results not yet given
/// (`stopHelpers`).
pub(super) async fn stop_all(cx: &Cx<'_>, pool: &mut LanePool, id: &str, s: &mut Session) {
    s.results.clear();
    s.tell_now = false;
    for h in std::mem::take(&mut s.helping) {
        match h.state {
            State::Waiting { .. } => {
                let on = h.own.then(|| words::branch(&s.branch, h.n));
                let (note, _) = words::end_lines(h.n, &h.task, &Ended::Stopped, on.as_deref());
                cx.note(id, &s.lease, &note).await;
            }
            State::Running { task, lane } => {
                h.stop.notify_one();
                let end = task
                    .await
                    .unwrap_or_else(|e| Ended::Failed(format!("the helper crashed: {e}")));
                // What it left is noted; a result that came in first isn't given to the session any more.
                finished(cx, pool, id, s, h.n, &h.task, h.own, lane, &end).await;
            }
        }
    }
    s.results.clear();
    s.tell_now = false;
}

/// The results given to the session's next turn, ahead of the person's message.
pub(super) fn with_results(s: &mut Session, message: String) -> String {
    s.tell_now = false;
    let mut parts = std::mem::take(&mut s.results);
    parts.push(message);
    parts.join("\n\n")
}

/// Gives an idle session the results that came in while it was idle, as a turn of its own (`tell`): nothing the person
/// said, so nothing of it goes in the conversation.
pub(super) fn tell(cx: &Cx<'_>, id: &str, s: &mut Session) {
    let (Some(lane), true) = (s.lane, s.tell_now && s.detail == Detail::Idle && !s.results.is_empty()) else {
        return;
    };
    let message = std::mem::take(&mut s.results).join("\n\n");
    s.tell_now = false;
    begin_turn(cx, id, s, lane, message, Pictures::default(), Vec::new());
}
