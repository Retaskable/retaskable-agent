//! The triage facts an item's saved run holds (06.9 bullets 1 and 16), turned into what the pure parts read:
//! [`NeedsHumanFacts`] for [`super::schedule`], and a [`TriageContext`] for the triager. Nothing runs a
//! triager from here.

use std::path::Path;

use super::TriageContext;
use super::schedule::{NeedsHumanFacts, triage_count};
use crate::backlog::Item;
use crate::paths::run_dir;
use crate::store::RunRecord;

/// The facts about a run's stop for `wants_triage` and `claude_on_it`. `pending_triage` (a decision waiting
/// for a lane) and `running_triage` (a triager running) are in memory only, so the caller passes them.
pub fn needs_human_facts(record: &RunRecord, pending_triage: bool, running_triage: bool) -> NeedsHumanFacts<'_> {
    NeedsHumanFacts {
        is_needs_human: record.state == "needs_human",
        triaged: record.triaged,
        pending_triage,
        running_triage,
        needs_human_reason: record.needs_human_reason.as_deref(),
        triages: record.triages,
        slice: record.slice,
    }
}

/// What the triager is given about `item`'s stop: its record, the backlog's item (PR, branch), `notes` (the
/// item's recent `item_state` notes, newest first) and where its transcripts are. `known_bugs` is the hub's
/// list, which the caller fills in; there's no check-round result kept yet.
pub fn triage_context(
    record: &RunRecord,
    item: Option<&Item>,
    notes: Vec<String>,
    data_dir: &Path,
    project: &str,
    item_id: &str,
) -> TriageContext {
    let branch = item.and_then(|i| i.branch.clone()).or_else(|| record.branch.clone());
    TriageContext {
        reason: record.needs_human_reason.clone().unwrap_or_default(),
        failed_in: record.failed_in.clone().unwrap_or_default(),
        timeline: notes,
        last_check_result: None,
        pr_url: item.and_then(|i| i.pr_url.clone()),
        has_pushed_branch: branch.is_some(),
        branch,
        has_session_to_continue: record.impl_session_id.is_some(),
        automatic_decisions: triage_count(record.triages, record.slice),
        transcripts_dir: Some(run_dir(data_dir, project, item_id).display().to_string()),
        known_bugs: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stopped() -> RunRecord {
        let mut r = RunRecord {
            state: "needs_human".into(),
            slice: 2,
            impl_session_id: Some("s1".into()),
            branch: Some("b-s2".into()),
            triages: Some((2, 1)),
            ..Default::default()
        };
        r.new_stop("reviewing", "Review escalated", 2);
        r
    }

    #[test]
    fn facts_read_the_record() {
        let r = stopped();
        let f = needs_human_facts(&r, true, false);
        assert!(f.is_needs_human && f.pending_triage && !f.running_triage && !f.triaged);
        assert_eq!(f.needs_human_reason, Some("Review escalated"));
        assert_eq!((f.triages, f.slice), (Some((2, 1)), 2));
    }

    #[test]
    fn a_second_stop_clears_the_finding_but_a_new_slice_resets_the_count() {
        let mut r = stopped();
        r.triaged = true;
        r.failed_looks = 2;
        r.next_look_at = Some("2026-01-01T00:00:00Z".into());
        r.new_stop("merging", "Merge failed", 2);
        assert!(!r.triaged && r.failed_looks == 0 && r.next_look_at.is_none());
        assert_eq!((r.failed_in.as_deref(), r.triages), (Some("merging"), Some((2, 1))));
        r.new_stop("reviewing", "again", 3);
        assert_eq!(r.triages, None);
    }

    #[test]
    fn context_gathers_record_item_and_notes() {
        let r = stopped();
        let item = Item {
            pr_url: Some("https://github.com/x/y/pull/9".into()),
            branch: Some("b-s2".into()),
            ..Default::default()
        };
        let c = triage_context(&r, Some(&item), vec!["n1".into()], Path::new("/d"), "p", "a");
        assert_eq!(
            (c.reason.as_str(), c.failed_in.as_str()),
            ("Review escalated", "reviewing")
        );
        assert_eq!(c.timeline, ["n1"]);
        assert_eq!(c.pr_url.as_deref(), Some("https://github.com/x/y/pull/9"));
        assert!(c.has_pushed_branch && c.has_session_to_continue);
        assert_eq!(c.automatic_decisions, 1);
        assert_eq!(
            c.transcripts_dir.as_deref(),
            Some(run_dir(Path::new("/d"), "p", "a").to_str().unwrap())
        );
        let bare = triage_context(&RunRecord::default(), None, vec![], Path::new("/d"), "p", "a");
        assert!(!bare.has_pushed_branch && !bare.has_session_to_continue && bare.pr_url.is_none());
    }

    #[test]
    fn an_old_record_without_the_fields_loads_and_round_trips() {
        let old = r#"{"state":"needs_human","slice":1}"#;
        let r: RunRecord = serde_json::from_str(old).unwrap();
        assert!(!r.triaged && r.triages.is_none() && r.reported_bugs.is_empty());
        let mut r = stopped();
        r.reported_bugs = vec!["sig".into()];
        r.triaged = true;
        let back: RunRecord = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }
}
