use std::collections::HashMap;
use std::path::Path;

use chrono::TimeZone;
use serde_json::{Value, json};

use super::*;
use crate::backlog::ItemStatus;
use crate::release_notes::group::group;
use crate::release_notes::test_support::{charts_item_and_pr, item, pr, range};

const TEMPLATE: &str = "Notes for {{projectNoun}} on {{baseBranch}} of {{githubRepo}}, {{from}} to {{to}} \
    ({{projectKind}}, {{projectUser}}):\n{{facts}}";

fn writer(model: &str) -> Writer<'static> {
    Writer {
        template: TEMPLATE,
        project_type: ProjectType::Game,
        base_branch: "main",
        github_repo: "x/y",
        cwd: Path::new("repo"),
        max_full_entries: 120,
        budget_usd: 0.5,
        model: model.to_string(),
        local: None,
        engine: None,
        cursor: None,
        sandbox: false,
    }
}

fn result(ok: bool, subtype: &str, structured: Option<Value>, error: Option<&str>) -> SessionResult {
    SessionResult {
        ok,
        subtype: subtype.to_string(),
        structured,
        error: error.map(str::to_string),
        cost_usd: Some(0.12),
        session_id: "s".to_string(),
        limit_hit: false,
        signed_out: false,
        context_tokens: None,
        usage: None,
        tokens: None,
    }
}

fn charts_sections() -> Vec<Section> {
    let (charts_item, charts_pr) = charts_item_and_pr();
    group(&[charts_pr], &[], &[charts_item], &HashMap::new())
}

/// A `run` that must never be called.
async fn no_session(_: SessionSpec) -> Result<SessionResult> {
    panic!("no session should run")
}

// writer_model / check_range

#[test]
fn writer_model_follows_a_local_scout_tier_and_is_haiku_otherwise() {
    assert_eq!(writer_model("local"), "local");
    assert_eq!(writer_model("sonnet"), "haiku");
    assert_eq!(writer_model("haiku"), "haiku");
}

#[test]
fn check_range_answers_utc_instants_for_a_good_range() {
    let now = Utc.with_ymd_and_hms(2026, 9, 30, 12, 0, 0).unwrap();
    let (from, to) = check_range("2026-09-23", "2026-09-30T23:59:59+02:00", now).unwrap();
    assert_eq!(from, "2026-09-23T00:00:00.000Z");
    assert_eq!(to, "2026-09-30T21:59:59.000Z");
}

#[test]
fn check_range_refuses_bad_backwards_and_future_ranges() {
    let now = Utc.with_ymd_and_hms(2026, 9, 30, 12, 0, 0).unwrap();
    let err = |f: &str, t: &str| check_range(f, t, now).unwrap_err().to_string();
    assert_eq!(err("soon", "2026-09-30"), "from and to must be dates.");
    assert_eq!(err("2026-09-30", "2026-09-23"), "To can't be before From.");
    assert_eq!(err("2026-10-01", "2026-10-01"), "The range can't be in the future.");
    assert_eq!(err("2026-09-23", "2026-10-03"), "The range can't be in the future.");
    // To is the end of its day, so a day ahead (a time zone either side) is fine.
    assert!(check_range("2026-09-23", "2026-10-01T23:59:59Z", now).is_ok());
}

// session_spec

#[test]
fn session_spec_is_a_read_only_ephemeral_writer_session_on_the_budget() {
    let spec = session_spec(&writer("haiku"), &charts_sections(), &range()).unwrap();
    assert_eq!(spec.model, "haiku");
    assert_eq!(spec.budget_usd, 0.5);
    assert!(spec.readonly);
    assert_eq!(spec.start, SessionStart::Ephemeral);
    assert_eq!(spec.cwd, Path::new("repo"));
    assert_eq!(spec.schema, release_notes_result_schema());
    assert!(spec.prompt.starts_with("Notes for the game on main of x/y"));
    assert!(spec.prompt.contains("Add charts"));
}

// write_note

#[tokio::test]
async fn an_empty_range_gets_the_empty_note_without_a_session() {
    let w = write_note(&[], 0, &range(), &writer("haiku"), no_session).await;
    assert_eq!(w.markdown, empty_note(&range()));
    assert_eq!(w.model, "");
    assert!(w.error.is_none());
}

#[tokio::test]
async fn a_range_of_only_chores_gets_the_plain_list_without_a_session() {
    let items = vec![item("1", "Bump", ItemStatus::OnMain, None, &[], Some("chores"))];
    let p = pr(
        1,
        "Bump",
        "https://github.com/x/y/pull/1",
        "retaskable/1-bump",
        "2026-09-24T00:00:00Z",
    );
    let sections = group(&[p], &[], &items, &HashMap::new());
    let w = write_note(&sections, 1, &range(), &writer("haiku"), no_session).await;
    assert!(w.markdown.contains("Nothing new for users between"));
    assert_eq!(w.counts.chores, 1);
    assert!(w.error.is_none());
}

#[tokio::test]
async fn the_models_answer_becomes_the_note() {
    let sections = charts_sections();
    let answer = json!({
        "status": "ok",
        "title": "Game: 23–30 September",
        "summary": "Charts arrived.",
        "highlights": [{ "key": "item:1", "headline": "Charts for every tally", "text": "See it at a glance." }],
        "entries": [],
    });
    let w = write_note(&sections, 1, &range(), &writer("local"), |spec| async move {
        assert_eq!(spec.model, "local");
        Ok(result(true, "success", Some(answer), None))
    })
    .await;
    assert!(w.markdown.contains("Charts for every tally"));
    assert!(w.markdown.contains("Charts arrived."));
    assert_eq!(w.model, "local");
    assert_eq!(w.cost_usd, 0.12);
    assert_eq!(w.counts.prs, 1);
    assert!(w.error.is_none());
}

#[tokio::test]
async fn a_failed_session_leaves_the_plain_list_and_says_why() {
    let sections = charts_sections();
    let plain = markdown(&sections, &range(), None);
    let w = write_note(&sections, 1, &range(), &writer("haiku"), |_| async {
        Ok(result(false, "error_max_budget_usd", None, None))
    })
    .await;
    assert_eq!(w.markdown, plain);
    assert_eq!(w.model, "haiku");
    assert_eq!(w.cost_usd, 0.12);
    assert_eq!(
        w.error.as_deref(),
        Some("Claude hit its $0.5 budget (spent $0.12), so this is the plain list of what's new.")
    );
}

#[tokio::test]
async fn a_failed_answer_or_a_session_that_wont_start_leaves_the_plain_list() {
    let sections = charts_sections();
    let failed = json!({ "status": "failed", "error": "no facts" });
    let w = write_note(&sections, 1, &range(), &writer("haiku"), |_| async {
        Ok(result(true, "success", Some(failed), None))
    })
    .await;
    assert_eq!(
        w.error.as_deref(),
        Some("Claude couldn't write them: no facts, so this is the plain list of what's new.")
    );
    assert_eq!(w.markdown, markdown(&sections, &range(), None));

    let w = write_note(&sections, 1, &range(), &writer("haiku"), |_| async {
        Ok(result(true, "success", None, None))
    })
    .await;
    assert_eq!(
        w.error.as_deref(),
        Some("Claude couldn't write them, so this is the plain list of what's new.")
    );

    let w = write_note(&sections, 1, &range(), &writer("haiku"), |_| async {
        anyhow::bail!("no claude")
    })
    .await;
    assert_eq!(
        w.error.as_deref(),
        Some("Claude couldn't write them (no claude), so this is the plain list of what's new.")
    );
    assert_eq!(w.cost_usd, 0.0);
}
