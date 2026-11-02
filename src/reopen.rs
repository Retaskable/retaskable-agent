//! A try-out found a problem after its slice's merge: the item itself reopens, rather than a
//! separate playtest-fix chore being queued every time (docs/specs/agent/06.4-playtests.md,
//! "Problem reopens the item itself"; Retaskable.com's `agent/src/reopen.ts` is the ground truth).
//! [`crate::backlog::Client`] now has the `reopen` op this needs, but nothing yet calls it:
//! receiving the store's forwarded `tryout_problem`, deciding chore vs. item with [`may_reopen`],
//! and resuming the session that built it (with [`resume_for_reopen`]'s message, on a new branch, in
//! its own lane) are the not-yet-built orchestrator-loop's job, the same as
//! [`crate::playtest_groups`] and [`crate::smoke`]. When the store keeps the item as it is (it's
//! being built) or doesn't know `reopen` (an older store), the caller queues a playtest-fix chore
//! with the same notes and steps instead, so the person's report is never lost — that chore isn't
//! ported here either.

/// Whether a problem with `item_id` may reopen its item rather than queue a chore
/// (`agent/src/reopen.ts`'s `mayReopen`): never for a chore's own try-out. The store has the final
/// say (an item already being built can't reopen either, [`crate::backlog::ReopenAnswer::Kept`]);
/// this is only the agent's cheap pre-filter before it even asks.
pub fn may_reopen(item_id: &str, is_chore: bool) -> bool {
    !item_id.starts_with("chore-") && !is_chore
}

/// What the session that built the item is told when it resumes for the reopened slice
/// (`agent/src/reopen.ts`'s `resumeForReopen`): `remaining` is the item's own, already carrying the
/// person's notes and steps (the store's `reopenText` built it), and `branch` is the new branch this
/// folder now sits on, off `origin/<base_branch>`.
pub fn resume_for_reopen(item_id: &str, remaining: &str, branch: &str, base_branch: &str) -> String {
    format!(
        "A person tried what you built after it merged, and found a problem. The item is reopened \
for it: this slice is the fix.\n\n\
{remaining}\n\n\
That work is merged, so this folder is now on a new branch `{branch}` from origin/{base_branch}. \
Reproduce the problem if you can, fix it at the root cause, and add a test or verify scenario that \
would have caught it.\n\
Then commit; Retaskable pushes the branch and opens its PR. Return the result through the \
structured output tool as before (status, `changeTitle` `{item_id}: Fix: <what you fixed>`, \
`changeBody`, summary, sliceDescription, itemDone when nothing else is left of the item or else \
remaining, and playtest so the person can check it again)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // may_reopen

    #[test]
    fn may_reopen_is_false_for_a_chores_own_tryout() {
        assert!(!may_reopen("chore-lighthouse-2026-10-28", false));
    }

    #[test]
    fn may_reopen_is_false_when_the_item_itself_is_a_chore() {
        assert!(!may_reopen("some-item", true));
    }

    #[test]
    fn may_reopen_is_true_for_an_ordinary_item() {
        assert!(may_reopen("playtests-group-finished-work", false));
    }

    // resume_for_reopen

    #[test]
    fn resume_for_reopen_carries_the_items_remaining_and_new_branch() {
        let msg = resume_for_reopen("widgets", "Fix the chart: it didn't load.", "widgets-fix-1", "main");
        assert!(msg.contains("Fix the chart: it didn't load."));
        assert!(msg.contains("`widgets-fix-1`"));
        assert!(msg.contains("origin/main"));
        assert!(msg.contains("widgets: Fix:"));
        assert!(msg.contains("reopened"));
    }
}
