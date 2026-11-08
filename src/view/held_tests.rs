//! The snapshot's `items` and `milestones` (agent/09.2-what-the-computer-reports.md).

use super::tests::reporting_view;
use super::*;
use serde_json::json;
use std::path::Path;

fn held(id: &str, kind_chore: bool) -> crate::orchestrator::live::HeldItem {
    crate::orchestrator::live::HeldItem {
        id: id.into(),
        title: "T".into(),
        state: "implementing".into(),
        lane: Some(2),
        branch: None,
        pr_url: None,
        attempt: 0,
        chore: kind_chore,
    }
}

#[test]
fn snapshot_reports_held_items_and_the_chores_milestone() {
    let v = reporting_view(true);
    let st = |v: &AgentView| v.snapshot("a", Path::new("."))["state"].clone();
    v.live.register("a");
    assert_eq!(
        (st(&v)["items"].clone(), st(&v)["milestones"].clone()),
        (json!([]), json!([]))
    );
    let mut rx = v.live.subscribe();
    rx.mark_unchanged();
    v.live.set_items("a", vec![held("R-1", false)]);
    assert!(rx.has_changed().unwrap());
    let s = st(&v);
    assert_eq!(
        s["items"],
        json!([{ "id": "R-1", "title": "T", "state": "implementing", "lane": 2, "attempt": 0, "chore": false }])
    );
    assert_eq!(s["milestones"], json!([]));
    v.live.set_items("a", vec![held("R-1", false), held("chore-x", true)]);
    assert_eq!(st(&v)["milestones"], json!([{ "id": "chores", "title": "Chores" }]));
    v.live.set_items("a", vec![]);
    assert_eq!(st(&v)["items"], json!([]));
    assert_eq!(st(&v)["milestones"], json!([]));
}

#[test]
fn snapshot_items_stay_empty_with_the_flag_off() {
    let v = reporting_view(false);
    v.live.register("a");
    v.live.set_items("a", vec![held("chore-x", true)]);
    let s = v.snapshot("a", Path::new("."))["state"].clone();
    assert_eq!((s["items"].clone(), s["milestones"].clone()), (json!([]), json!([])));
}
