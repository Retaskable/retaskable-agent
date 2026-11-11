//! The weekly cutoff: an implementer session ending with the week used up at the
//! cutoff limits Claude to the week's reset, and nothing new is claimed (`driver/settle.rs`, `driver/project.rs`,
//! docs/specs/agent/07.1-model-tiers.md).
use super::*;
use crate::engines::EngineId;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn week(u: f64) -> Value {
    let now = now();
    json!({"unifiedWindows": {
        "five_hour": {"utilization": 0.1, "resetsAt": now + 3600},
        "seven_day": {"utilization": u, "resetsAt": now + 86_400}}})
}

/// A rig with the item "a" implementing on lane 1; `result` ends its session, carrying `usage`.
async fn ended(lanes: Option<[&str; 2]>, result: SessionResult, usage: Value) -> (Rig, Asked, Arc<Fake>) {
    let (base, asked) = claim_stub(
        json!([item("b", "claimed")]),
        Some(json!({"item": item("b", "claimed"), "token": "t"})),
    )
    .await;
    let mut rig = rig(2, &[&base]);
    if let Some(lanes) = lanes {
        rig.machine.links.engine_choice =
            serde_json::from_value(json!({ "default": lanes[0], "lanes": lanes })).unwrap();
    }
    let fake = rig.go_live();
    at(&mut rig, 0, "a", ItemState::Implementing, Some(1));
    rig.machine.outcome_sender().send(SessionOutcome {
        project: "p0".into(),
        item_id: "a".into(),
        lane: 1,
        result,
        usage: Some(usage),
        context_tokens: None,
        notes: vec![],
        session_id: None,
    });
    rig.machine.poll().await;
    (rig, asked, fake)
}

fn claims(asked: &Asked) -> usize {
    ops(asked).iter().filter(|o| *o == "claim_next").count()
}

fn limited(rig: &Rig, after: u64) -> bool {
    let t = std::time::Instant::now() + Duration::from_secs(after);
    rig.machine.engine_limits.is_limited(EngineId::Claude, t)
}

#[tokio::test]
async fn at_the_cutoff_claude_is_limited_to_the_weeks_reset_and_nothing_new_is_claimed() {
    let (mut rig, asked, _) = ended(None, SessionResult::NeedsHuman("x".into()), week(0.96)).await;
    assert!(limited(&rig, 80_000) && !limited(&rig, 86_402));
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 0);
}

#[tokio::test]
async fn below_the_cutoff_work_still_starts() {
    let (mut rig, asked, _) = ended(None, SessionResult::NeedsHuman("x".into()), week(0.90)).await;
    assert!(!limited(&rig, 1));
    rig.machine.pass().await;
    assert!(claims(&asked) > 0);
}

#[tokio::test]
async fn a_weekly_rejection_holds_until_the_weeks_reset_not_the_five_hour_one() {
    let now = now();
    let usage = json!({"status": "rejected", "rateLimitType": "seven_day", "resetsAt": now + 86_400});
    let result = SessionResult::Limited {
        why: "limit".into(),
        resets_in: Some(Duration::from_secs(3600)),
    };
    let (rig, _, _) = ended(None, result, usage).await;
    assert!(limited(&rig, 80_000) && !limited(&rig, 86_402));
}

#[tokio::test]
async fn a_cursor_lane_still_starts_while_claude_is_cut_off() {
    let (mut rig, asked, fake) = ended(
        Some(["claude", "cursor"]),
        SessionResult::NeedsHuman("x".into()),
        week(0.96),
    )
    .await;
    rig.machine.pass().await;
    assert!(claims(&asked) > 0);
    let started = fake.started.lock().unwrap().clone();
    assert!(started.iter().any(|s| s.engine == EngineId::Cursor));
}

#[tokio::test]
async fn a_lone_weekly_reading_is_saved_as_the_week_usage_and_not_as_the_usage() {
    let week =
        json!({"status": "allowed", "rateLimitType": "seven_day", "utilization": 0.5, "resetsAt": 4_000_000_000u64});
    {
        let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
        let mut rig = rig(2, &[&base]);
        rig.go_live();
        at(&mut rig, 0, "a", ItemState::Implementing, Some(1));
        rig.machine.outcome_sender().send(SessionOutcome {
            project: "p0".into(),
            item_id: "a".into(),
            lane: 1,
            result: SessionResult::NeedsHuman("x".into()),
            usage: Some(week.clone()),
            context_tokens: None,
            notes: vec![],
            session_id: None,
        });
        rig.machine.poll().await;
        assert_eq!(rig.lp(0).store.week_usage(), Some(week.clone()));
        assert_eq!(rig.lp(0).store.usage(), None);
    }
}

#[tokio::test]
async fn a_saved_weekly_reading_limits_claude_on_a_started_loop_only_while_ahead() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // (utilization, reset offset, limited)
    let cases: [(f64, i64, bool); 3] = [(0.96, 86_400, true), (0.96, -60, false), (0.94, 86_400, false)];
    for (u, offset, limited) in cases {
        let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
        let mut rig = rig(2, &[&base]);
        let at_ = (now as i64 + offset) as u64;
        rig.lp(0)
            .store
            .set_week_usage(json!({"rateLimitType": "seven_day", "utilization": u, "resetsAt": at_}));
        rig.go_live();
        rig.machine.poll().await;
        let id = crate::engines::EngineId::Claude;
        assert_eq!(
            rig.machine.engine_limits.is_limited(id, std::time::Instant::now()),
            limited
        );
    }
}
