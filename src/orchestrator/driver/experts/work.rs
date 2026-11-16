//! A session's work summary (`expert.work` in the snapshot: docs/specs/expert-chat.md "The right panel's data"): what
//! its branch changed, its commits and its pull requests, read after every turn and, while a pull request is open,
//! again every beat without a new turn (checks finishing, auto-merge, a merge or a close show up). A read is a task of
//! its own, taken in on a later pass, so a slow `git` or `gh` never holds the loop.
use super::*;
use crate::expert::work::{Pr, Work};

/// What the host read: the branch's files and commits (only when it had a lane to read), and its pull requests
/// (`None` when the host couldn't be read: the last list stays).
#[derive(Debug, Clone, Default)]
pub struct WorkRead {
    pub work: Option<Work>,
    pub prs: Option<Vec<Pr>>,
}

pub(super) struct Reading {
    task: JoinHandle<WorkRead>,
    /// Read with the lane's git too, after a turn, rather than the pull requests alone.
    full: bool,
}

/// Starts reading the session's work: its lane's git and its pull requests. A read already running is replaced.
pub(super) fn read_after_turn(cx: &Cx<'_>, s: &mut Session) {
    let task = tokio::spawn(cx.host.work(cx.project, s.lane, &s.branch, s.helpers));
    s.reading = Some(Reading { task, full: true });
}

/// Takes in the reads that are done, and starts the pull requests' read for each idle session whose change is open.
pub(super) async fn poll(cx: &Cx<'_>, ex: &mut Experts) {
    let every = ex.beat_every;
    for (id, s) in ex.sessions.iter_mut() {
        if s.reading.as_ref().is_some_and(|r| r.task.is_finished())
            && let Some(Reading { task, full }) = s.reading.take()
        {
            match task.await {
                // A read that came back while a turn runs is left: the turn's own read follows it.
                Ok(read) if full || s.turn.is_none() => take_in(s, read),
                Ok(_) => {}
                Err(e) => tracing::warn!("{}: expert {id}: reading its work failed: {e}", cx.id),
            }
        }
        let open = s.work.as_ref().is_some_and(Work::has_open_pr);
        if open && s.turn.is_none() && s.reading.is_none() && s.prs_at.is_none_or(|t| t.elapsed() >= every) {
            s.prs_at = Some(Instant::now());
            let task = tokio::spawn(cx.host.work(cx.project, None, &s.branch, s.helpers));
            s.reading = Some(Reading { task, full: false });
        }
    }
}

/// The new read over the last: a lane git that couldn't be read, or a host that couldn't, keeps its part.
fn take_in(s: &mut Session, read: WorkRead) {
    let last = s.work.take();
    let prs = read.prs.or_else(|| last.as_ref().map(|w| w.prs.clone()));
    s.work = match (read.work, last) {
        (Some(mut w), _) => {
            w.prs = prs.unwrap_or_default();
            Some(w)
        }
        (None, Some(mut w)) => {
            w.prs = prs.unwrap_or_default();
            Some(w)
        }
        (None, None) => None,
    };
}
