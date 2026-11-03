//! Small helpers of the project loops.
use super::Shared;

/// Whether Claude Code was found signed out on this computer recently enough that no new or resumed
/// session should start yet ([`Machine::claude_signed_out_until`](super::Machine), section 7.3).
pub(super) fn signed_out(shared: &Shared<'_>) -> bool {
    shared
        .claude_signed_out_until
        .is_some_and(|until| std::time::Instant::now() < until)
}
