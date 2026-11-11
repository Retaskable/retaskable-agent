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

/// "a" claimed on a Cursor lane, its first session ended at Cursor's limit with a pull request open.
async fn cursor_hit(engine: &str) -> (Rig, Arc<Fake>, Asked) {
    limit_hit([engine, engine], None).await
}

/// Like [`cursor_hit`] on lanes with these engines, `out` already limited when the session ends.
async fn limit_hit(lanes: [&str; 2], out: Option<EngineId>) -> (Rig, Arc<Fake>, Asked) {
    let choice = json!({ "default": lanes[0], "lanes": lanes });
    hit_with(choice, out, false, true).await
}

/// "a" on a lane of `choice`, its first session ended at Cursor's limit, with a pull request open when `pushed`
/// (else nothing is pushed and Cursor's limit gives the item back).
async fn hit_with(choice: Value, out: Option<EngineId>, signed_out: bool, pushed: bool) -> (Rig, Arc<Fake>, Asked) {
    let (base, asked) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    rig.machine.links.engine_choice = serde_json::from_value(choice).unwrap();
    if let Some(engine) = out {
        rig.machine.engine_limits.hit(engine, out_for(3600));
    }
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(cursor_limited());
    rig.machine.pass().await;
    if pushed {
        rig.lp(0).items.get_mut("a").unwrap().pr_url = Some("https://github.com/o/p/pull/3".into());
    }
    // Claude signs out after the session started, before its limit is read.
    rig.machine.claude_signed_out_until = signed_out.then(|| out_for(600));
    rig.machine.pass().await;
    (rig, fake, asked)
}

#[tokio::test]
async fn a_cursor_limit_keeps_the_item_implementing_and_resumes_it_after_30_minutes() {
    let (mut rig, fake, asked) = cursor_hit("cursor").await;
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
    let (mut rig, fake, _) = cursor_hit("cursor").await;
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
    let (mut rig, fake, _) = cursor_hit("cursor").await;
    let saved = rig.lp(0).store.runs()["a"].clone();
    let back: crate::store::RunRecord = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
    assert_eq!(back.cursor_limit_hits, 1);
    wait_over(&mut rig, &fake, done(ImplementerOutcome::Finished)).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 0);
}

async fn carries_on(lanes: [&str; 2], from: EngineId, to: EngineId, line: &str) {
    let (mut rig, fake, _) = limit_hit(lanes, None).await;
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
    let (mut rig, _fake, _) = limit_hit(["cursor", "claude"], Some(EngineId::Claude)).await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 1);
    assert!(rig.lp(0).silenced.contains_key("a"));
}

#[tokio::test]
async fn a_claude_limit_goes_to_a_person_as_before() {
    let (mut rig, _fake, _) = cursor_hit("claude").await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 0);
}

#[tokio::test]
async fn fall_over_carries_on_with_claude_used_only_by_another_role() {
    let choice = json!({ "default": "cursor", "lanes": ["cursor", "cursor"], "roles": { "planner": "claude" } });
    let (mut rig, fake, _) = hit_with(choice, None, false, true).await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 0);
    assert!(!rig.lp(0).silenced.contains_key("a"));
    let line = "Hit Cursor's usage limit; carrying on now with Claude";
    assert!(
        rig.lp(0)
            .events
            .recent()
            .iter()
            .any(|e| format!("{e:?}").contains(line))
    );
    rig.machine.pass().await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!((started[1].engine, started[1].resume), (EngineId::Claude, false));
}

#[tokio::test]
async fn fall_over_waits_30_minutes_while_claude_is_signed_out() {
    let (mut rig, _fake, _) = hit_with(
        json!({ "default": "cursor", "lanes": ["cursor", "claude"] }),
        None,
        true,
        true,
    )
    .await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 1);
    assert!(rig.lp(0).silenced.contains_key("a"));
}

#[tokio::test]
async fn fall_over_with_only_cursor_still_waits_and_goes_to_a_person_after_three() {
    let (mut rig, fake, _) = hit_with(
        json!({ "default": "cursor", "lanes": ["cursor", "cursor"] }),
        None,
        false,
        true,
    )
    .await;
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 1);
    assert!(rig.lp(0).silenced.contains_key("a"));
    for _ in 0..3 {
        wait_over(&mut rig, &fake, cursor_limited()).await;
        rig.machine.pass().await;
    }
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
}

fn cursor_choice() -> Value {
    json!({ "default": "cursor", "lanes": ["cursor", "cursor"] })
}

#[tokio::test]
async fn a_cursor_limit_with_nothing_pushed_gives_the_item_back_at_once() {
    let (mut rig, fake, asked) = hit_with(cursor_choice(), None, false, false).await;
    let machine = rig.machine.links.machine.clone();
    let sent = ops(&asked);
    let at = sent.iter().position(|o| o == "report").unwrap();
    assert_eq!(sent[at + 1], "release");
    let note = asked.lock().unwrap().iter().find(|(o, _)| o == "report").unwrap().1["note"].clone();
    assert_eq!(
        note,
        format!("Given back: Cursor's usage limit was hit on {machine} before anything was pushed")
    );
    assert!(!rig.lp(0).runs.contains_key("a"));
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), None);
    assert!(!rig.lp(0).leases.contains_key("a"));
    assert!(rig.lp(0).unreleased.is_empty());
    assert!(!rig.lp(0).store.runs().contains_key("a"));
    assert!(rig.machine.claim_paused_until.is_none(), "claiming isn't paused");
    assert!(!rig.lp(0).silenced.contains_key("a"));
    rig.machine.engine_limits = Default::default();
    rig.machine.pass().await;
    assert!(
        fake.started.lock().unwrap().iter().all(|s| !s.resume),
        "not resumed after 30 minutes"
    );
}

#[tokio::test]
async fn a_cursor_limit_with_a_pull_request_keeps_the_retries_even_with_give_back_on() {
    let (base, _asked) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    rig.machine.links.engine_choice = serde_json::from_value(cursor_choice()).unwrap();
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(cursor_limited());
    rig.machine.pass().await;
    rig.lp(0).items.get_mut("a").unwrap().pr_url = Some("https://github.com/o/p/pull/3".into());
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert_eq!(rig.lp(0).store.runs()["a"].cursor_limit_hits, 1);
    for _ in 0..2 {
        wait_over(&mut rig, &fake, cursor_limited()).await;
        rig.machine.pass().await;
    }
    wait_over(&mut rig, &fake, cursor_limited()).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
}

#[tokio::test]
async fn a_cursor_limit_with_claude_free_carries_on_instead_of_giving_back() {
    let choice = json!({ "default": "cursor", "lanes": ["cursor", "claude"] });
    let (mut rig, _fake, asked) = hit_with(choice, None, false, false).await;
    assert!(!ops(&asked).iter().any(|o| o == "release"));
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
}
