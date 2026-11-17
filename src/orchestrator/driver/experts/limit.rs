//! Usage-limit waiting for a session's queue (docs/specs/expert-chat.md; Retaskable.com's `turns.ts` `settle` tail and
//! `limitResume`): a turn that ended at Claude's usage limit with messages still queued holds them, kept, and the
//! history says when they go. They go when the limit lifts (the 5-hour reset the usage reading gave, never sooner than
//! ten minutes, as the TypeScript agent's `limitResume`), or when the person sends another message or Send now, which
//! tries at once. Nothing else is held: a session that isn't at the limit never waits.
use std::time::Duration;

use super::*;
use crate::engines::EngineId;

/// When to try again at the least after a usage-limit hit: the hit may have been a different limit, whose reset isn't
/// known.
pub(super) const AT_LEAST: Duration = Duration::from_secs(10 * 60);

/// When a held queue goes, from when Claude's limit lifts (`None` when it isn't known), never sooner than `floor`.
pub(super) fn resume_at(lifts: Option<Instant>, now: Instant, floor: Duration) -> Instant {
    lifts.unwrap_or(now).max(now + floor)
}

/// The history's words: "At the usage limit: sends at 14:05" (the computer's own time, 24-hour).
pub(super) fn said(wait: Duration) -> String {
    let at = chrono::Local::now() + chrono::Duration::from_std(wait).unwrap_or_default();
    format!("At the usage limit: sends at {}", at.format("%H:%M"))
}

/// A turn ended at the usage limit: with messages queued they wait, kept, and the history says when they go.
pub(super) async fn hold(cx: &Cx<'_>, item: &str, s: &mut Session) {
    if s.queue.is_empty() {
        return;
    }
    let now = Instant::now();
    let until = resume_at(cx.limits.limited_until(EngineId::Claude, now), now, cx.limit_floor);
    s.limit_until = Some(until);
    cx.note(item, &s.lease, &said(until - now)).await;
}

/// Whether the session's queue is held at the usage limit now; once the time is past the hold is over.
pub(super) fn held(s: &mut Session) -> bool {
    match s.limit_until {
        Some(until) if until > Instant::now() => true,
        Some(_) => {
            s.limit_until = None;
            false
        }
        None => false,
    }
}
