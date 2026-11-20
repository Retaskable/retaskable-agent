//! Small helpers of the project loops.
use super::Shared;

/// Whether Claude Code was found signed out on this computer recently enough that no new or resumed
/// session should start yet ([`Machine::claude_signed_out_until`](super::Machine), section 7.3).
pub(super) fn signed_out(shared: &Shared<'_>) -> bool {
    shared
        .claude_signed_out_until
        .is_some_and(|until| std::time::Instant::now() < until)
}

/// Whether Claude is at its usage limit or the weekly cutoff on this computer, and when it has time again
/// (`intentBlock`'s `out_of_time`): what `intent_seen` says and what a refused session row is released for.
pub(super) fn out_of_time(shared: &Shared<'_>) -> Option<crate::intents::OutOfTime> {
    let now = std::time::Instant::now();
    shared
        .engine_limits
        .claude_at_limit(now)
        .then(|| crate::intents::OutOfTime {
            until_ms: shared.engine_limits.claude_until(now).map(|until| {
                crate::telemetry::events::now_ms() + until.saturating_duration_since(now).as_millis() as i64
            }),
        })
}
