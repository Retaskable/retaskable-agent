//! What stops new work from starting keeps the watchdog's `idle_lanes` and `no_progress` quiet
//! (`ProjectLoop::start_blocked`, docs/specs/agent-telemetry.md section 4), behind `watchdog-anomaly-rules`.
use super::*;
use crate::engines::EngineId;
use crate::telemetry::Event;

/// A live rig with two lanes, `idle_lanes` shown at once, and these flags On.
async fn idle_rig(flags: &[&str]) -> Rig {
    let (base, _asked) = claim_stub(json!([]), None).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(flags));
    rig.lp(0).project.tuning.idle_lanes_minutes = 0;
    rig.go_live();
    rig
}

fn idle_starts(rig: &mut Rig) -> usize {
    rig.lp(0)
        .events
        .recent()
        .iter()
        .filter(|e| matches!(e.event(), Some(Event::Anomaly(a)) if a.id.as_deref() == Some("idle_lanes") && a.phase.as_deref() == Some("start")))
        .count()
}

fn pause_claims(rig: &mut Rig) {
    rig.machine.claim_paused_until = Some(std::time::Instant::now() + Duration::from_secs(3600));
}

#[tokio::test]
async fn a_claim_pause_keeps_idle_lanes_quiet_until_it_clears() {
    let mut rig = idle_rig(&["watchdog-anomaly-rules"]).await;
    pause_claims(&mut rig);
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(idle_starts(&mut rig), 0);
    rig.machine.claim_paused_until = None;
    rig.machine.pass().await;
    assert_eq!(idle_starts(&mut rig), 1);
}

#[tokio::test]
async fn with_the_flag_off_a_claim_pause_does_not_silence_idle_lanes() {
    let mut rig = idle_rig(&[]).await;
    pause_claims(&mut rig);
    rig.machine.pass().await;
    assert_eq!(idle_starts(&mut rig), 1);
}

async fn blocked(rig: &mut Rig) -> Option<&'static str> {
    let m = &mut rig.machine;
    let shared = Shared {
        links: &m.links,
        claim_paused_until: &mut m.claim_paused_until,
        claude_signed_out_until: &mut m.claude_signed_out_until,
        engine_limits: &mut m.engine_limits,
        local: &crate::local_model::LocalModel::off(),
        stopping: false,
    };
    m.projects[0].start_blocked(&shared, &m.pool).await
}

fn in_a_minute() -> std::time::Instant {
    std::time::Instant::now() + Duration::from_secs(60)
}

#[tokio::test]
async fn with_the_start_rules_off_claude_signed_out_blocks_even_a_cursor_lane() {
    let mut rig = idle_rig(&[]).await;
    rig.machine.links.engine_choice =
        serde_json::from_value(json!({ "default": "claude", "lanes": ["cursor", "claude"] })).unwrap();
    assert_eq!(blocked(&mut rig).await, None);
    rig.machine.claude_signed_out_until = Some(in_a_minute());
    assert_eq!(blocked(&mut rig).await, Some("Claude signed out"));
}

#[tokio::test]
async fn with_the_start_rules_on_a_lane_that_can_fall_over_or_use_another_engine_is_not_blocked() {
    let mut rig = idle_rig(&["engine-start-rules"]).await;
    rig.machine.links.engine_choice =
        serde_json::from_value(json!({ "default": "claude", "lanes": ["claude", "cursor"] })).unwrap();
    // Claude signed out, Cursor free: the Cursor lane may start.
    rig.machine.claude_signed_out_until = Some(in_a_minute());
    assert_eq!(blocked(&mut rig).await, None);
    // Both out: nothing may start.
    rig.machine.engine_limits.hit(EngineId::Cursor, in_a_minute());
    assert_eq!(blocked(&mut rig).await, Some("usage limit"));
}

#[tokio::test]
async fn with_the_start_rules_on_a_limited_claude_falls_over_to_the_other_engine_in_use() {
    let mut rig = idle_rig(&["engine-start-rules"]).await;
    rig.machine.links.engine_choice =
        serde_json::from_value(json!({ "default": "claude", "lanes": ["claude", "cursor"] })).unwrap();
    rig.machine.engine_limits.hit(EngineId::Claude, in_a_minute());
    assert_eq!(blocked(&mut rig).await, None);
}
