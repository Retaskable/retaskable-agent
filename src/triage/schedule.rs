//! Whether Claude should look at a stopped item at all, and whether it still counts as working on
//! it (docs/specs/agent/06.9-triage-claude-looks-at-needs-you-first.md bullets 16 and 20;
//! `agent/src/triage.ts`'s `triageCount`, `triageAlreadyDone`, `wantsLook`/`wantsTriage`,
//! `claudeOnIt` and `nothingToContinue`). Pure: these read the few facts about an item's stop a
//! caller already has, named after the shared backlog's own fields for the day something
//! assembles them from a real item record — there is no such record in this port yet (`mod.rs`'s
//! own module doc), so nothing here gathers them.

use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use regex::Regex;

use crate::engines::signed_out_text;

/// Automatic decisions allowed per slice before a stop needs a person outright, counting
/// start-overs (06.9 bullet 16; `MAX_TRIAGES`).
pub const MAX_TRIAGES: u32 = 2;

/// Reason text meaning Claude already finished looking at this stop: left a finding for a person,
/// or gave up retrying a failed look. Only the current stop's reason is ever checked against this
/// (callers replace it whenever the item enters `needs_human` again), so an earlier stop's text
/// can't block a later look.
static TRIAGE_DONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("Claude looked into it|Claude couldn't look into it").unwrap());

/// Automatic decisions already made for the item's current slice (`triages` as `(slice, n)`); a
/// mark from an earlier slice doesn't count (`triageCount`).
pub fn triage_count(triages: Option<(u32, u32)>, slice: u32) -> u32 {
    match triages {
        Some((triaged_slice, n)) if triaged_slice == slice => n,
        _ => 0,
    }
}

/// Whether Claude already left a finding (or a failure note) for this stop (`triageAlreadyDone`).
/// The TS also treats a `person_steps` wait's own reason shape as done, so a restart that loses
/// its `triaged` mark doesn't look again and drop the steps (retaskable-bug-1a7c896c15); this port
/// has no `shared/personSteps.ts` equivalent yet, nothing here writes that shape, so only the
/// plain finding text is checked for now.
pub fn triage_already_done(needs_human_reason: Option<&str>) -> bool {
    TRIAGE_DONE.is_match(needs_human_reason.unwrap_or(""))
}

/// The facts about a stopped item `wants_triage` and `claude_on_it` read, named after the shared
/// backlog's own fields (`triaged`, `pendingTriage`, `running`, `triages`) for whenever something
/// assembles these from a real item record. Not that record: this port has none yet.
#[derive(Debug, Clone, Copy)]
pub struct NeedsHumanFacts<'a> {
    pub is_needs_human: bool,
    pub triaged: bool,
    pub pending_triage: bool,
    pub running_triage: bool,
    pub needs_human_reason: Option<&'a str>,
    pub triages: Option<(u32, u32)>,
    pub slice: u32,
}

/// Whether Claude should look at this stop, now or once a failed look's wait is over
/// (`wantsLook`): stopped as `needs_human`, not already looked at or looking, under the cap, and
/// not stopped only because Claude was signed out (nothing to look into there; it's carried on
/// once it's signed in again).
fn wants_look(it: &NeedsHumanFacts, triage_on: bool, max: u32) -> bool {
    triage_on
        && it.is_needs_human
        && !it.triaged
        && !it.pending_triage
        && triage_count(it.triages, it.slice) < max
        && !signed_out_text(it.needs_human_reason)
        && !triage_already_done(it.needs_human_reason)
}

fn is_in_future(at: Option<&str>, now: DateTime<Utc>) -> bool {
    at.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|dt| dt.with_timezone(&Utc) > now)
}

/// Whether Claude should look at this stop right now, also waiting out a failed look's retry time
/// (`wantsTriage`).
pub fn wants_triage(
    it: &NeedsHumanFacts,
    triage_on: bool,
    max: u32,
    failed_look_at: Option<&str>,
    now: DateTime<Utc>,
) -> bool {
    wants_look(it, triage_on, max) && !is_in_future(failed_look_at, now)
}

/// Whether Claude is still looking into why an item stopped, or has a decision waiting for a lane
/// (06.9 bullet 20; `claudeOnIt`): the triager is running, has decided but can't carry it out yet,
/// or hasn't looked yet but wants to (a failed look waiting to retry counts). What a report of
/// "Claude is working on it" vs. "stopped, needs a person" would read.
pub fn claude_on_it(it: &NeedsHumanFacts, triage_on: bool) -> bool {
    it.is_needs_human && (it.running_triage || it.pending_triage || wants_look(it, triage_on, MAX_TRIAGES))
}

/// No change and no session's pushed work to carry on from: what Continue refuses, starting the
/// item over instead (`nothingToContinue`).
pub fn nothing_to_continue(branch: Option<&str>, pr_url: Option<&str>, impl_session_id: Option<&str>) -> bool {
    branch.is_none() || (pr_url.is_none() && impl_session_id.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> NeedsHumanFacts<'static> {
        NeedsHumanFacts {
            is_needs_human: true,
            triaged: false,
            pending_triage: false,
            running_triage: false,
            needs_human_reason: None,
            triages: None,
            slice: 1,
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-10-29T12:00:00Z".parse().unwrap()
    }

    // triage_count

    #[test]
    fn triage_count_is_zero_without_a_mark() {
        assert_eq!(triage_count(None, 1), 0);
    }

    #[test]
    fn triage_count_reads_the_mark_only_for_the_current_slice() {
        assert_eq!(triage_count(Some((1, 2)), 1), 2, "same slice: counts");
        assert_eq!(
            triage_count(Some((1, 2)), 2),
            0,
            "a later slice: an earlier mark doesn't count"
        );
    }

    // triage_already_done

    #[test]
    fn triage_already_done_reads_a_finding_or_a_failure_note() {
        assert!(!triage_already_done(None));
        assert!(!triage_already_done(Some("blocked on credentials")));
        assert!(triage_already_done(Some(
            "Claude looked into it and left it for you: ..."
        )));
        assert!(triage_already_done(Some(
            "Claude couldn't look into it (it hit its budget), so it waits for you"
        )));
    }

    // wants_triage

    #[test]
    fn wants_triage_is_false_when_triage_is_off() {
        assert!(!wants_triage(&facts(), false, MAX_TRIAGES, None, now()));
    }

    #[test]
    fn wants_triage_is_false_off_needs_human() {
        let it = NeedsHumanFacts {
            is_needs_human: false,
            ..facts()
        };
        assert!(!wants_triage(&it, true, MAX_TRIAGES, None, now()));
    }

    #[test]
    fn wants_triage_is_false_already_triaged_or_pending() {
        assert!(!wants_triage(
            &NeedsHumanFacts {
                triaged: true,
                ..facts()
            },
            true,
            MAX_TRIAGES,
            None,
            now()
        ));
        assert!(!wants_triage(
            &NeedsHumanFacts {
                pending_triage: true,
                ..facts()
            },
            true,
            MAX_TRIAGES,
            None,
            now()
        ));
    }

    #[test]
    fn wants_triage_is_false_at_the_cap() {
        let it = NeedsHumanFacts {
            triages: Some((1, 2)),
            ..facts()
        };
        assert!(!wants_triage(&it, true, 2, None, now()));
        assert!(wants_triage(
            &NeedsHumanFacts {
                triages: Some((1, 1)),
                ..facts()
            },
            true,
            2,
            None,
            now()
        ));
    }

    #[test]
    fn wants_triage_is_false_signed_out_or_already_done() {
        let signed_out = NeedsHumanFacts {
            needs_human_reason: Some("failed to authenticate"),
            ..facts()
        };
        assert!(!wants_triage(&signed_out, true, MAX_TRIAGES, None, now()));

        let done = NeedsHumanFacts {
            needs_human_reason: Some("Claude looked into it and left it for you: ..."),
            ..facts()
        };
        assert!(!wants_triage(&done, true, MAX_TRIAGES, None, now()));
    }

    #[test]
    fn wants_triage_waits_out_a_failed_look() {
        let it = facts();
        assert!(
            !wants_triage(&it, true, MAX_TRIAGES, Some("2026-10-29T12:10:00Z"), now()),
            "still waiting"
        );
        assert!(
            wants_triage(&it, true, MAX_TRIAGES, Some("2026-10-29T11:50:00Z"), now()),
            "wait is over"
        );
        assert!(wants_triage(&it, true, MAX_TRIAGES, None, now()), "no wait at all");
    }

    #[test]
    fn wants_triage_is_true_otherwise() {
        assert!(wants_triage(&facts(), true, MAX_TRIAGES, None, now()));
    }

    // claude_on_it

    #[test]
    fn claude_on_it_is_false_off_needs_human() {
        let it = NeedsHumanFacts {
            is_needs_human: false,
            ..facts()
        };
        assert!(!claude_on_it(&it, true));
    }

    #[test]
    fn claude_on_it_is_true_running_or_pending_even_past_the_cap() {
        let maxed_out = NeedsHumanFacts {
            triages: Some((1, MAX_TRIAGES)),
            ..facts()
        };
        assert!(claude_on_it(
            &NeedsHumanFacts {
                running_triage: true,
                ..maxed_out
            },
            true
        ));
        assert!(claude_on_it(
            &NeedsHumanFacts {
                pending_triage: true,
                ..maxed_out
            },
            true
        ));
    }

    #[test]
    fn claude_on_it_is_true_when_it_still_wants_to_look() {
        assert!(claude_on_it(&facts(), true));
    }

    #[test]
    fn claude_on_it_is_false_once_nothing_is_left_to_look_into() {
        let it = NeedsHumanFacts {
            triages: Some((1, MAX_TRIAGES)),
            ..facts()
        };
        assert!(!claude_on_it(&it, true), "at the cap, not running or pending");
        assert!(!claude_on_it(&facts(), false), "triage off, not running or pending");
    }

    // nothing_to_continue

    #[test]
    fn nothing_to_continue_without_a_branch() {
        assert!(nothing_to_continue(None, Some("https://example/pr/1"), None));
    }

    #[test]
    fn nothing_to_continue_with_a_branch_but_no_pr_and_no_session() {
        assert!(nothing_to_continue(Some("item-1"), None, None));
    }

    #[test]
    fn something_to_continue_with_a_branch_and_either_a_pr_or_a_session() {
        assert!(!nothing_to_continue(Some("item-1"), Some("https://example/pr/1"), None));
        assert!(!nothing_to_continue(Some("item-1"), None, Some("session-1")));
    }
}
