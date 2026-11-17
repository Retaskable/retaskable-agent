use super::*;
use crate::backlog::pool::Tier;
use serde_json::json;

fn task(subject: &str, extra: serde_json::Value) -> TaskInput {
    let mut v = json!({ "id": "task-1", "subject": subject, "prompt": "  Do the thing.  ", "tier": "fast" });
    v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    serde_json::from_value(v).unwrap()
}

#[test]
fn a_rotation_chore_is_titled_by_its_label_and_a_fix_main_names_the_red_commit() {
    let tidy = work_of(&task("chore:tidy", json!({})), ChoreKind::Tidy, Some("abc"));
    assert_eq!(tidy.title, "Tidy: finding something worth doing");
    assert_eq!(tidy.red_sha, None, "only a fix main is queued for a commit");
    assert_eq!(tidy.prompt, "Do the thing.");
    let fix = work_of(&task("chore:fix_main", json!({})), ChoreKind::FixMain, Some("abc"));
    assert_eq!(fix.title, "Fix main: main's checks failing");
    assert_eq!(fix.red_sha.as_deref(), Some("abc"));
    assert_eq!(fix.tier.as_deref(), Some(Tier::Fast.item_tier()));
}

#[test]
fn a_fix_task_takes_the_title_of_its_fix_and_a_smoke_fix_is_not_queued_for_a_commit() {
    let smoke = task("fix_smoke:abc", json!({ "fix": { "title": "Staging smoke failed" } }));
    let work = work_of(&smoke, ChoreKind::FixMain, Some("def"));
    assert_eq!(work.title, "Fix main: Staging smoke failed");
    assert_eq!(work.red_sha, None, "`addFixChore` sets no redSha");
    let playtest = task(
        "fix_playtest:k1",
        json!({ "fix": { "title": "Button is broken", "itemId": "item-7" } }),
    );
    let work = work_of(&playtest, ChoreKind::PlaytestFix, Some("def"));
    assert_eq!(work.title, "Playtest fix: Button is broken");
    assert_eq!(work.fixes_item.as_deref(), Some("item-7"));
}

#[test]
fn the_item_is_the_tasks_own_with_no_store_item_behind_it() {
    let work = work_of(&task("chore:bugs", json!({})), ChoreKind::Bugs, None);
    let item = work.item("task-1");
    assert_eq!((item.id.as_str(), item.slice), ("task-1", 1));
    assert_eq!(item_kind(&item), Some(ChoreKind::Bugs));
    assert_eq!(item.tier.as_deref(), Some(crate::tiers::SONNET));
    assert_eq!(Flow::of(&item), Flow::default());
    // A roadmap item is never read as a chore, whatever its kind says.
    let roadmap = Item {
        kind: Some("bugs".into()),
        ..Item::default()
    };
    assert_eq!(item_kind(&roadmap), None);
}

#[test]
fn a_fix_main_skips_the_baseline_and_a_main_sync_skips_the_review() {
    let fix = work_of(&task("chore:fix_main", json!({})), ChoreKind::FixMain, None).item("t");
    assert_eq!(
        Flow::of(&fix),
        Flow {
            no_baseline: true,
            merge_sync: false
        }
    );
    let sync = sync_work(
        &task("chore:sync_main:dev", json!({})),
        "0123456789",
        "main",
        "dev",
        &["a.rs".into()],
    );
    let item = sync.item("t");
    assert_eq!(item.branch.as_deref(), Some("retaskable/sync-main-0123456"));
    assert_eq!(
        item.remaining.as_deref(),
        Some("Merging main into dev conflicts in: a.rs")
    );
    assert!(Flow::of(&item).merge_sync);
}

#[test]
fn the_chore_the_prompt_reads_carries_the_stores_words_and_a_playtest_fix_always_has_some() {
    let chore = work_of(&task("chore:tidy", json!({})), ChoreKind::Tidy, None)
        .chore()
        .unwrap();
    assert_eq!(chore.instructions, "Do the thing.");
    let mut empty = task("fix_playtest:k", json!({}));
    empty.prompt = String::new();
    let chore = work_of(&empty, ChoreKind::PlaytestFix, None).chore().unwrap();
    assert!(chore.instructions.contains("a person found"));
}

#[test]
fn the_work_survives_the_state_file() {
    let work = sync_work(&task("chore:sync_main:dev", json!({})), "abcdef0", "main", "dev", &[]);
    let back: ChoreWork = serde_json::from_value(serde_json::to_value(&work).unwrap()).unwrap();
    assert_eq!(back, work);
}

#[test]
fn only_the_session_kinds_become_items_through_session_kind() {
    for name in ["tidy", "tests", "bugs", "fix_main", "fix_playtest"] {
        assert!(session_kind(name).is_some(), "{name}");
    }
    for name in ["specs", "sync_main", "juggle"] {
        assert!(session_kind(name).is_none(), "{name}");
    }
}
