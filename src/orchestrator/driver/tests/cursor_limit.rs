//! Cursor's usage limit and carrying on at once on the other engine (`driver/settle.rs`, engines/03 section 3.5).
use super::settling::{out_for, resume_now};
use super::*;
use crate::engines::EngineId;

fn done(result: ImplementerOutcome) -> SessionResult {
    SessionResult::Implementer(result)
}

/// The 30 minutes are up: Cursor's stamp has lapsed and the resume is due now.
async fn wait_over(rig: &mut Rig, fake: &Fake, next: SessionResult) {
    rig.machine.engine_limits = Default::default();
    resume_now(rig, fake, next).await;
}

fn cursor_limited() -> SessionResult {
    SessionResult::Limited {
        why: "usage limit".into(),
        resets_in: None,
    }
}

/// "a" claimed on a Cursor lane with `cursor-limit-retry` as asked, its first session ended at Cursor's limit.
async fn cursor_hit(flag: bool, engine: &str) -> (Rig, Arc<Fake>, Asked) {
    limit_hit(flag, [engine, engine], None).await
}

/// Like [`cursor_hit`] on lanes with these engines, `out` already limited when the session ends.
async fn limit_hit(flag: bool, lanes: [&str; 2], out: Option<EngineId>) -> (Rig, Arc<Fake>, Asked) {
    let (base, asked) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    rig.machine.links.engine_choice = serde_json::from_value(json!({ "default": lanes[0], "lanes": lanes })).unwrap();
    if let Some(engine) = out {
        rig.machine.engine_limits.hit(engine, out_for(3600));
    }
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(if flag {
        &["cursor-limit-retry"]
    } else {
        &[]
    }));
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(cursor_limited());
    rig.machine.pass().await;
    rig.machine.pass().await;
    (rig, fake, asked)
}

#[tokio::test]
async fn a_cursor_limit_keeps_the_item_implementing_and_resumes_it_after_30_minutes() {
    let (mut rig, fake, asked) = cursor_hit(true, "cursor").await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
    assert_eq!(rig.lp(0).store.leases()["a"], "lease-1");
    assert!(!ops(&asked).iter().any(|o| o == "release" || o == "park"));
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 1);
    let due = rig.lp(0).silenced["a"];
    let wait = due - std::time::Instant::now();
    assert!(wait > Duration::from_secs(29 * 60) && wait <= Duration::from_secs(30 * 60));
    assert_eq!(fake.started.lock().unwrap().len(), 1, "not resumed before the wait");
    wait_over(&mut rig, &fake, done(ImplementerOutcome::Finished)).await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 2);
    assert!(started[1].resume && started[1].session_id == started[0].session_id);
}

#[tokio::test]
async fn the_fourth_cursor_limit_in_a_row_goes_to_a_person() {
    let (mut rig, fake, _) = cursor_hit(true, "cursor").await;
    for _ in 0..2 {
        wait_over(&mut rig, &fake, cursor_limited()).await;
        rig.machine.pass().await;
        assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    }
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 3);
    wait_over(&mut rig, &fake, cursor_limited()).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    let record = rig.lp(0).store.runs()["a"].clone();
    assert_eq!(
        record.needs_human_reason.as_deref(),
        Some("Cursor's usage limit was hit 3 times in a row")
    );
    assert_eq!(record.cursor_limit_hits, 0);
}

#[tokio::test]
async fn an_answer_between_cursor_limits_resets_the_count_and_it_survives_a_reload() {
    let (mut rig, fake, _) = cursor_hit(true, "cursor").await;
    let saved = rig.lp(0).store.runs()["a"].clone();
    let back: crate::store::RunRecord = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
    assert_eq!(back.cursor_limit_hits, 1);
    wait_over(&mut rig, &fake, done(ImplementerOutcome::Finished)).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 0);
}

async fn carries_on(lanes: [&str; 2], from: EngineId, to: EngineId, line: &str) {
    let (mut rig, fake, _) = limit_hit(true, lanes, None).await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 0);
    assert!(!rig.lp(0).silenced.contains_key("a"), "not held for 30 minutes");
    let noted = rig
        .lp(0)
        .events
        .recent()
        .iter()
        .any(|e| format!("{e:?}").contains(line));
    assert!(noted, "history line {line:?}");
    rig.machine.pass().await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 2);
    assert_eq!(started[0].engine, from);
    assert_eq!(started[1].engine, to);
    assert!(!started[1].resume, "a fresh session on the branch");
}

#[tokio::test]
async fn a_cursor_limit_with_claude_free_carries_on_with_claude_at_once() {
    carries_on(
        ["cursor", "claude"],
        EngineId::Cursor,
        EngineId::Claude,
        "Hit Cursor's usage limit; carrying on now with Claude",
    )
    .await;
}

#[tokio::test]
async fn a_claude_limit_with_cursor_free_carries_on_with_cursor_at_once() {
    carries_on(
        ["claude", "cursor"],
        EngineId::Claude,
        EngineId::Cursor,
        "Hit Claude's usage limit; carrying on now with Cursor",
    )
    .await;
}

#[tokio::test]
async fn a_cursor_limit_with_the_other_engine_limited_too_waits_30_minutes() {
    let (mut rig, _fake, _) = limit_hit(true, ["cursor", "claude"], Some(EngineId::Claude)).await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 1);
    assert!(rig.lp(0).silenced.contains_key("a"));
}

#[tokio::test]
async fn a_claude_limit_or_the_flag_off_goes_to_a_person_as_before() {
    for (flag, engine) in [(true, "claude"), (false, "cursor")] {
        let (mut rig, _fake, _) = cursor_hit(flag, engine).await;
        assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman, "{flag} {engine}");
        assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 0);
    }
}
