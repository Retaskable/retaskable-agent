//! What the agent reads of an item's triage marks and when Continue has nothing to carry on from
//! (docs/specs/agent/06.9-triage-claude-looks-at-needs-you-first.md). Pure. Whether Claude looks at a stop at all, the
//! cap on looks and the retries are the store's (the `triage` job); nothing here decides them.

//! Whether Claude should look at a stopped item at all, and whether it still counts as working on
//! it (docs/specs/agent/06.9-triage-claude-looks-at-needs-you-first.md bullets 16 and 20;
//! `agent/src/triage.ts`'s `triageCount`, `triageAlreadyDone`, `wantsLook`/`wantsTriage`,
//! `claudeOnIt` and `nothingToContinue`). Pure: these read the few facts about an item's stop a
//! caller already has, named after the shared backlog's own fields. [`super::record`] assembles
//! them from a run's saved record; nothing here gathers them.

/// Automatic decisions already made for the item's current slice (`triages` as `(slice, n)`); a
/// mark from an earlier slice doesn't count (`triageCount`).
pub fn triage_count(triages: Option<(u32, u32)>, slice: u32) -> u32 {
    match triages {
        Some((triaged_slice, n)) if triaged_slice == slice => n,
        _ => 0,
    }
}

/// No change and no session's pushed work to carry on from: what Continue refuses, starting the
/// item over instead (`nothingToContinue`).
pub fn nothing_to_continue(branch: Option<&str>, pr_url: Option<&str>, impl_session_id: Option<&str>) -> bool {
    branch.is_none() || (pr_url.is_none() && impl_session_id.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
