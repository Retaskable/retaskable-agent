//! What the triager is given about a stopped item, gathered from the loop's own state (06.9 bullet 1).
use crate::item_lifecycle::ItemState;

/// Why a step that moved an item to `needs_human` stopped it, by the state it ran in (the step's outcome is
/// the only thing that moved it, and each state has one way to escalate that a person can act on).
pub(super) fn step_stop_reason(from: ItemState) -> &'static str {
    match from {
        ItemState::AwaitingCi => "CI timed out or kept failing, past the allowed rounds.",
        ItemState::Checking => "The checks kept failing, past the allowed rounds.",
        ItemState::Reviewing => "The review escalated, or its rounds ran out.",
        ItemState::AwaitingPlaytest => "The playtest failed and the review rounds ran out.",
        ItemState::Merging => "The merge failed, or its conflict rounds ran out.",
        ItemState::AwaitingMerge => "The pull request was closed without merging.",
        _ => "The item stopped and needs a person.",
    }
}
