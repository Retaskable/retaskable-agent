use std::collections::BTreeMap;

use serde_json::json;

use super::last::{HostRelease, Last, pick_last};
use super::*;
use crate::release_notes::StoreRelease;

/// A store that keeps rows by kind with a version, counts writes, and can be told to fail.
#[derive(Default)]
struct Fake {
    rows: Mutex<BTreeMap<&'static str, (Value, u64)>>,
    writes: Mutex<Vec<(&'static str, Value, Option<u64>)>>,
    /// Errors the next calls to `set` answer, oldest first.
    fail: Mutex<Vec<BacklogError>>,
    /// A write another computer makes just before the next `set`.
    race: Mutex<Option<(&'static str, Value)>>,
}

impl Fake {
    fn with(kind: &'static str, data: Value) -> Self {
        let f = Self::default();
        f.rows.lock().unwrap().insert(kind, (data, 5));
        f
    }
    fn row(&self, kind: &str) -> Option<Value> {
        self.rows.lock().unwrap().get(kind).map(|r| r.0.clone())
    }
    fn written(&self) -> usize {
        self.writes.lock().unwrap().len()
    }
}

fn refusal(code: &str) -> BacklogError {
    BacklogError::BadAnswer {
        status: 400,
        code: Some(code.into()),
        message: "no".into(),
    }
}

fn down() -> BacklogError {
    BacklogError::Unreachable("offline".into())
}

impl FactStore for Fake {
    fn get(&self, kind: &'static str) -> BoxFuture<'_, Result<Option<(Value, u64)>, BacklogError>> {
        async move { Ok(self.rows.lock().unwrap().get(kind).cloned()) }.boxed()
    }

    fn set(&self, kind: &'static str, data: Value, at: Option<u64>) -> BoxFuture<'_, Result<(), BacklogError>> {
        async move {
            if let Some((k, v)) = self.race.lock().unwrap().take() {
                let next = self.rows.lock().unwrap().get(k).map_or(1, |r| r.1 + 1);
                self.rows.lock().unwrap().insert(k, (v, next));
            }
            if !self.fail.lock().unwrap().is_empty() {
                return Err(self.fail.lock().unwrap().remove(0));
            }
            let mut rows = self.rows.lock().unwrap();
            let now = rows.get(kind).map_or(0, |r| r.1);
            if at.is_some_and(|a| a != now) {
                return Err(refusal("conflict"));
            }
            self.writes.lock().unwrap().push((kind, data.clone(), at));
            rows.insert(kind, (data, now + 1));
            Ok(())
        }
        .boxed()
    }
}

fn files(n: usize) -> Value {
    json!([{ "path": "docs/specs/a.md", "content": "x".repeat(n), "deleted": false }])
}

#[tokio::test]
async fn a_drafts_files_are_written_into_the_row_beside_other_computers() {
    let store = Fake::with(DRAFT_FILES, json!({ "other": [], "7": files(1) }));
    let facts = KeptFacts::default();
    facts.keep_draft_files("7", &files(10));
    facts.keep_draft_files("8", &files(2));
    facts.flush(&store).await;
    let row = store.row(DRAFT_FILES).unwrap();
    assert_eq!(row["other"], json!([]), "another computer's draft stays");
    assert_eq!(
        row["7"][0]["content"].as_str().unwrap().len(),
        10,
        "a draft's own files are replaced"
    );
    assert!(row.get("8").is_some());
    assert_eq!(store.writes.lock().unwrap()[0].2, Some(5), "over the version it read");
    assert!(facts.owing().is_empty());
}

#[tokio::test]
async fn a_draft_over_its_size_is_not_kept_and_an_older_copy_goes() {
    let store = Fake::with(DRAFT_FILES, json!({ "7": files(5) }));
    let facts = KeptFacts::default();
    facts.keep_draft_files("7", &files(ONE_DRAFT_MAX + 1));
    facts.flush(&store).await;
    assert_eq!(store.row(DRAFT_FILES).unwrap(), json!({}));
}

#[tokio::test]
async fn the_oldest_drafts_go_first_when_all_pass_the_rows_size() {
    let store = Fake::default();
    let facts = KeptFacts::default();
    for id in ["a", "b", "c"] {
        facts.keep_draft_files(id, &files(ONE_DRAFT_MAX - 100));
    }
    facts.flush(&store).await;
    let row = store.row(DRAFT_FILES).unwrap();
    assert!(size(&row) <= DRAFT_FILES_MAX - MARGIN);
    assert!(
        row.get("a").is_none() && row.get("c").is_some(),
        "{:?}",
        row.as_object().unwrap().keys()
    );
}

#[tokio::test]
async fn a_write_that_lost_a_race_reads_the_row_and_goes_again() {
    let store = Fake::with(DRAFT_FILES, json!({ "x": [] }));
    *store.race.lock().unwrap() = Some((DRAFT_FILES, json!({ "x": [], "y": [] })));
    let facts = KeptFacts::default();
    facts.keep_draft_files("7", &files(1));
    facts.flush(&store).await;
    let row = store.row(DRAFT_FILES).unwrap();
    assert!(
        row.get("y").is_some() && row.get("7").is_some(),
        "the other computer's write was not lost"
    );
}

#[tokio::test]
async fn an_unreachable_store_leaves_every_fact_owed_and_a_later_flush_sends_them() {
    let store = Fake::default();
    store.fail.lock().unwrap().push(down());
    let facts = KeptFacts::default();
    facts.keep_environments(&json!({ "staging": {} }));
    facts.keep_draft_files("7", &files(1));
    facts.flush(&store).await;
    assert_eq!(store.written(), 0);
    assert_eq!(facts.owing(), vec!["draft_files/7", "environments"]);
    facts.flush(&store).await;
    assert!(facts.owing().is_empty());
    assert!(store.row(ENVIRONMENTS).unwrap()["at"].is_string());
    assert!(store.row(DRAFT_FILES).is_some());
}

#[tokio::test]
async fn a_5xx_is_kept_but_a_coded_refusal_is_dropped() {
    let store = Fake::default();
    let facts = KeptFacts::default();
    facts.keep_environments(&json!({}));
    store.fail.lock().unwrap().push(BacklogError::BadAnswer {
        status: 503,
        code: None,
        message: "busy".into(),
    });
    facts.flush(&store).await;
    assert_eq!(facts.owing(), vec!["environments"]);
    store.fail.lock().unwrap().push(refusal("bad_request"));
    facts.flush(&store).await;
    assert!(
        facts.owing().is_empty(),
        "the store refused it; sending it again would not help"
    );
    assert!(store.row(ENVIRONMENTS).is_none());
}

#[tokio::test]
async fn owed_facts_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let facts = KeptFacts::default();
    facts.attach(dir.path());
    facts.keep_environments(&json!({ "a": 1 }));
    facts.keep_draft_files("7", &files(1));
    let after = KeptFacts::default();
    after.attach(dir.path());
    assert_eq!(after.owing(), vec!["draft_files/7", "environments"]);
    let store = Fake::default();
    after.flush(&store).await;
    assert!(after.owing().is_empty());
    let third = KeptFacts::default();
    third.attach(dir.path());
    assert!(third.owing().is_empty(), "what the store took is not owed again");
}

#[tokio::test]
async fn a_too_big_environments_view_is_not_kept() {
    let facts = KeptFacts::default();
    facts.keep_environments(&json!({ "big": "x".repeat(ENVIRONMENTS_MAX) }));
    assert!(facts.owing().is_empty());
}

#[tokio::test]
async fn the_last_release_is_written_only_when_it_changed() {
    let store = Fake::with(RELEASE_LAST, json!({ "last": { "at": "2026-11-01", "label": "v1" } }));
    let facts = KeptFacts::default();
    assert!(!facts.seeded());
    facts.seed(&store).await;
    assert!(facts.seeded());
    let v1 = Last {
        at: "2026-11-01".into(),
        label: "v1".into(),
    };
    facts.keep_release_last(Some(&v1));
    assert!(facts.owing().is_empty(), "the store already has it");
    facts.keep_release_last(Some(&Last {
        at: "2026-11-09".into(),
        label: "v2".into(),
    }));
    facts.flush(&store).await;
    assert_eq!(
        store.row(RELEASE_LAST).unwrap(),
        json!({ "last": { "at": "2026-11-09", "label": "v2" } })
    );
    facts.keep_release_last(None);
    facts.flush(&store).await;
    assert_eq!(store.row(RELEASE_LAST).unwrap(), json!({ "last": null }));
}

#[tokio::test]
async fn a_store_that_cant_be_read_leaves_the_facts_unseeded() {
    struct Down;
    impl FactStore for Down {
        fn get(&self, _: &'static str) -> BoxFuture<'_, Result<Option<(Value, u64)>, BacklogError>> {
            async { Err(down()) }.boxed()
        }
        fn set(&self, _: &'static str, _: Value, _: Option<u64>) -> BoxFuture<'_, Result<(), BacklogError>> {
            async { Err(down()) }.boxed()
        }
    }
    let facts = KeptFacts::default();
    facts.seed(&Down).await;
    assert!(!facts.seeded());
}

#[tokio::test]
async fn the_main_pr_draft_is_kept_per_ahead_count_and_head_and_emptied_when_nothing_is_ahead() {
    let facts = KeptFacts::default();
    let store = Fake::default();
    facts.seed(&store).await;
    facts.keep_main_pr_draft(None);
    assert!(
        facts.owing().is_empty(),
        "nothing held and nothing ahead: nothing to write"
    );
    facts.keep_main_pr_draft(Some(("T", "B", 3, Some("abc"))));
    facts.flush(&store).await;
    assert_eq!(
        store.row(MAIN_PR_DRAFT).unwrap(),
        json!({ "title": "T", "body": "B", "ahead": 3, "sha": "abc" })
    );
    assert_eq!(facts.held_main_pr(), Some((3, Some("abc".into()))));
    facts.keep_main_pr_draft(None);
    facts.flush(&store).await;
    assert_eq!(
        store.row(MAIN_PR_DRAFT).unwrap(),
        json!({ "title": "", "body": "", "ahead": 0 })
    );
    facts.keep_main_pr_draft(None);
    assert!(facts.owing().is_empty(), "an empty draft is written once");
}

#[tokio::test]
async fn a_restart_empties_an_old_draft_the_store_still_holds() {
    let store = Fake::with(MAIN_PR_DRAFT, json!({ "title": "T", "body": "B", "ahead": 2 }));
    let facts = KeptFacts::default();
    facts.seed(&store).await;
    facts.keep_main_pr_draft(None);
    facts.flush(&store).await;
    assert_eq!(store.row(MAIN_PR_DRAFT).unwrap()["ahead"], 0);
}

#[test]
fn the_release_check_waits_five_minutes_between_looks() {
    let facts = KeptFacts::default();
    assert!(facts.release_due(last::EVERY));
    assert!(!facts.release_due(last::EVERY));
    assert!(facts.release_due(std::time::Duration::ZERO));
}

fn note(id: &str, to: &str) -> StoreRelease {
    serde_json::from_value(json!({ "id": id, "title": "t", "from": "2026-01-01", "to": to })).unwrap()
}

#[test]
fn the_last_release_is_the_newest_note_else_the_host_release_else_the_tag() {
    let release = HostRelease {
        published_at: "2026-10-01T00:00:00Z".into(),
        tag_name: "v1.2".into(),
    };
    let saved = [note("2026-11-02.md", "2026-11-02"), note("older", "2026-10-01")];
    assert_eq!(
        pick_last(&saved, Some(&release), Some(("t", "d"))),
        Some(Last {
            at: "2026-11-02".into(),
            label: "2026-11-02".into()
        })
    );
    assert_eq!(
        pick_last(&[], Some(&release), Some(("t", "d"))),
        Some(Last {
            at: "2026-10-01T00:00:00Z".into(),
            label: "v1.2".into()
        })
    );
    assert_eq!(
        pick_last(&[], None, Some(("v0", "2026-09-01T00:00:00Z")))
            .unwrap()
            .label,
        "v0"
    );
    assert_eq!(pick_last(&[], None, Some(("v0", ""))), None);
    assert_eq!(pick_last(&[], None, None), None);
}

#[tokio::test]
async fn the_base_branchs_results_are_kept_as_the_newest_of_each_kind_and_written_whole() {
    let facts = KeptFacts::default();
    let store = Fake::default();
    facts.keep_result(BASELINE, &json!({ "sha": "a", "results": [], "at": "t" }));
    facts.keep_result(BASELINE, &json!({ "sha": "b", "results": [], "at": "t" }));
    facts.keep_result(MAIN_CI, &json!({ "sha": "b", "state": "fail" }));
    facts.flush(&store).await;
    assert_eq!(
        store.row(BASELINE).unwrap()["sha"],
        "b",
        "the newest replaced the older"
    );
    assert_eq!(store.row(MAIN_CI).unwrap()["state"], "fail");
    assert!(facts.owing().is_empty());
    // A kind that is not a result of the base branch, and one over its cap, are not kept.
    facts.keep_result("smoke", &json!({}));
    facts.keep_result(CI_COVERAGE, &json!({ "tree": "x".repeat(9000) }));
    assert!(facts.owing().is_empty());
}

#[tokio::test]
async fn a_result_the_store_cannot_take_stays_owed_until_it_can() {
    let facts = KeptFacts::default();
    let store = Fake::default();
    store.fail.lock().unwrap().push(down());
    facts.keep_result(
        CI_COVERAGE,
        &json!({ "base": "main", "tree": "t", "coverage": "covered" }),
    );
    facts.flush(&store).await;
    assert_eq!(facts.owing(), [CI_COVERAGE]);
    facts.flush(&store).await;
    assert!(facts.owing().is_empty() && store.row(CI_COVERAGE).is_some());
}

#[tokio::test]
async fn an_issue_import_row_holds_the_error_then_is_cleared_once_the_tracker_reads_again() {
    let facts = KeptFacts::default();
    let store = Fake::default();
    facts.keep_issue_import(Some("Couldn't read GitHub issues: HTTP 404"));
    facts.flush(&store).await;
    let row = store.row(ISSUE_IMPORT).unwrap();
    assert_eq!(row["error"], "Couldn't read GitHub issues: HTTP 404");
    assert!(row["at"].as_str().is_some_and(|at| at.ends_with('Z')));
    facts.keep_issue_import(None);
    facts.flush(&store).await;
    let row = store.row(ISSUE_IMPORT).unwrap();
    assert!(row.get("error").is_none() && row["at"].is_string(), "{row}");
}

#[tokio::test]
async fn a_long_issue_error_is_cut_to_the_stores_limit() {
    let facts = KeptFacts::default();
    let store = Fake::default();
    facts.keep_issue_import(Some(&"x".repeat(5000)));
    facts.flush(&store).await;
    assert_eq!(store.row(ISSUE_IMPORT).unwrap()["error"].as_str().unwrap().len(), 900);
}

fn note_row(generated: &str, extra: Value) -> Value {
    let mut n = json!({ "from": "a", "to": "b", "state": "ready", "markdown": "# Hi", "generatedAt": generated });
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        n[k] = v;
    }
    json!({ "note": n })
}

fn page() -> Value {
    json!({ "id": "AbC", "url": "https://x/p/AbC", "at": "t", "markdown": "# Hi" })
}

#[tokio::test]
async fn a_published_page_is_written_into_the_note_it_was_made_for_over_the_version_read() {
    let store = Fake::with(RELEASE_NOTE, note_row("g1", json!({})));
    let facts = KeptFacts::default();
    facts.keep_published("g1", "AbC", Some(&page()));
    facts.flush(&store).await;
    let row = store.row(RELEASE_NOTE).unwrap();
    assert_eq!(row["note"]["published"], page());
    assert_eq!(row["note"]["markdown"], "# Hi");
    assert_eq!(store.writes.lock().unwrap()[0].2, Some(5));
    assert!(facts.owing().is_empty());
}

#[tokio::test]
async fn a_note_generated_again_meanwhile_is_left_as_it_is_and_the_debt_is_dropped() {
    let store = Fake::with(RELEASE_NOTE, note_row("g2", json!({})));
    let facts = KeptFacts::default();
    facts.keep_published("g1", "AbC", Some(&page()));
    facts.flush(&store).await;
    assert_eq!(store.written(), 0);
    assert!(store.row(RELEASE_NOTE).unwrap()["note"].get("published").is_none());
    assert!(facts.owing().is_empty());
}

#[tokio::test]
async fn a_store_that_cannot_be_reached_keeps_the_page_owed_and_a_lost_race_reads_again() {
    let store = Fake::with(RELEASE_NOTE, note_row("g1", json!({})));
    store.fail.lock().unwrap().push(down());
    let facts = KeptFacts::default();
    facts.keep_published("g1", "AbC", Some(&page()));
    facts.flush(&store).await;
    assert_eq!(facts.owing(), [PUBLISHED]);
    *store.race.lock().unwrap() = Some((RELEASE_NOTE, note_row("g1", json!({ "edited": true }))));
    facts.flush(&store).await;
    let row = store.row(RELEASE_NOTE).unwrap();
    assert_eq!(
        (row["note"]["edited"].clone(), row["note"]["published"].clone()),
        (json!(true), page())
    );
    assert!(facts.owing().is_empty());
}

#[tokio::test]
async fn an_unpublished_page_leaves_the_note_only_when_it_is_that_page() {
    let published = note_row("g1", json!({ "published": page() }));
    let store = Fake::with(RELEASE_NOTE, published.clone());
    let facts = KeptFacts::default();
    facts.keep_published("g1", "Other", None);
    facts.flush(&store).await;
    assert_eq!(store.row(RELEASE_NOTE).unwrap(), published);
    facts.keep_published("g1", "AbC", None);
    facts.flush(&store).await;
    assert!(store.row(RELEASE_NOTE).unwrap()["note"].get("published").is_none());
}

#[tokio::test]
async fn a_long_issue_error_in_wide_or_escaped_text_is_still_kept_within_the_stores_bytes() {
    for text in ["错".repeat(5000), "\"\n".repeat(2500)] {
        let facts = KeptFacts::default();
        let store = Fake::default();
        facts.keep_issue_import(Some(&text));
        facts.flush(&store).await;
        let row = store.row(ISSUE_IMPORT).expect("the error is cut to fit, not dropped");
        assert!(size(&row) <= 2048, "{}", size(&row));
        assert!(row["error"].as_str().is_some_and(|e| !e.is_empty()));
    }
}
