//! The weekly cutoff behind the `weekly-cutoff` flag: an implementer session ending with the week used up at the
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

/// A rig with the item "a" implementing on lane 1 and these flags On; `result` ends its session, carrying `usage`.
async fn ended(
    flags: &[&str],
    lanes: Option<[&str; 2]>,
    result: SessionResult,
    usage: Value,
) -> (Rig, Asked, Arc<Fake>) {
    let (base, asked) = claim_stub(
        json!([item("b", "claimed")]),
        Some(json!({"item": item("b", "claimed"), "token": "t"})),
    )
    .await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(flags));
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
    for flags in [&["weekly-cutoff"][..], &["weekly-cutoff", "engine-start-rules"][..]] {
        let (mut rig, asked, _) = ended(flags, None, SessionResult::NeedsHuman("x".into()), week(0.96)).await;
        assert!(limited(&rig, 80_000) && !limited(&rig, 86_402), "{flags:?}");
        rig.machine.pass().await;
        assert_eq!(claims(&asked), 0, "{flags:?}");
    }
}

#[tokio::test]
async fn below_the_cutoff_work_still_starts() {
    let (mut rig, asked, _) = ended(
        &["weekly-cutoff"],
        None,
        SessionResult::NeedsHuman("x".into()),
        week(0.90),
    )
    .await;
    assert!(!limited(&rig, 1));
    rig.machine.pass().await;
    assert!(claims(&asked) > 0);
}

#[tokio::test]
async fn with_the_flag_off_nothing_changes() {
    let (mut rig, asked, _) = ended(&[], None, SessionResult::NeedsHuman("x".into()), week(0.96)).await;
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
    let (rig, _, _) = ended(&["weekly-cutoff"], None, result, usage).await;
    assert!(limited(&rig, 80_000) && !limited(&rig, 86_402));
}

#[tokio::test]
async fn a_cursor_lane_still_starts_while_claude_is_cut_off() {
    let flags = ["weekly-cutoff", "engine-start-rules", "engine-placement"];
    let (mut rig, asked, fake) = ended(
        &flags,
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
