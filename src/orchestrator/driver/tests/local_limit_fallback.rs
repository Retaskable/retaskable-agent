//! `local-limit-fallback`: with the local model up, Claude at its limit (5-hour or weekly) doesn't hold fresh work
//! for Claude lanes, so they claim and start (piece 1's tier sends them to the local model); one session queues
//! behind the slot at most; Claude signed out still holds (docs/specs/agent/07.2-local-model.md).
use super::*;
use crate::engines::EngineId;
use crate::local_model::LocalModel;

fn in_a_minute() -> std::time::Instant {
    std::time::Instant::now() + Duration::from_secs(60)
}

fn week_used() -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    json!({"unifiedWindows": {
        "five_hour": {"utilization": 0.1, "resetsAt": now + 3600},
        "seven_day": {"utilization": 0.96, "resetsAt": now + 86_400}}})
}

/// A rig with two lanes, a queue holding "b" and these flags; Claude is limited (`weekly`: by the weekly cutoff).
async fn rig_at_limit(flags: &[&str], weekly: bool, local: LocalModel) -> (Rig, Asked, Arc<Fake>) {
    let (base, asked) = claim_stub(
        json!([item("b", "claimed")]),
        Some(json!({"item": item("b", "claimed"), "token": "t"})),
    )
    .await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(flags));
    rig.machine.local = local;
    let fake = rig.go_live();
    if weekly {
        rig.machine.engine_limits.set_week_cutoff(true);
        assert!(rig.machine.engine_limits.cut_off_for_week(&week_used()));
    } else {
        rig.machine.engine_limits.hit(EngineId::Claude, in_a_minute());
    }
    (rig, asked, fake)
}

fn claims(asked: &Asked) -> usize {
    ops(asked).iter().filter(|o| *o == "claim_next").count()
}

fn resets_at(rig: &Rig) -> Option<String> {
    rig.live.snapshot().projects["p0"].limit_resets_at.clone()
}

async fn blocked(rig: &mut Rig) -> Option<&'static str> {
    let m = &mut rig.machine;
    let shared = Shared {
        links: &m.links,
        claim_paused_until: &mut m.claim_paused_until,
        claude_signed_out_until: &mut m.claude_signed_out_until,
        engine_limits: &mut m.engine_limits,
        local: &m.local,
        stopping: false,
    };
    m.projects[0].start_blocked(&shared, &m.pool).await
}

const CASES: [(&str, &[&str], bool); 3] = [
    ("5-hour, rules on", &["engine-start-rules"], false),
    ("weekly, rules on", &["engine-start-rules", "weekly-cutoff"], true),
    ("weekly, rules off", &["weekly-cutoff"], true),
];

#[tokio::test]
async fn at_the_limit_a_claude_lane_claims_and_starts_and_the_project_is_running() {
    crate::tiers::set_local_limit_fallback_for_test(true);
    crate::tiers::set_local_ladder_for_test(true);
    for (name, flags, weekly) in CASES {
        let (mut rig, asked, fake) = rig_at_limit(flags, weekly, LocalModel::up()).await;
        assert_eq!(blocked(&mut rig).await, None, "{name}");
        rig.machine.pass().await;
        assert_eq!(claims(&asked), 1, "{name}");
        assert_eq!(fake.started.lock().unwrap().len(), 1, "{name}");
        assert_eq!(resets_at(&rig), None, "{name}");
    }
}

#[tokio::test]
async fn a_second_item_is_not_claimed_while_one_session_waits_for_the_slot() {
    crate::tiers::set_local_limit_fallback_for_test(true);
    crate::tiers::set_local_ladder_for_test(true);
    for (name, flags, weekly) in CASES {
        let local = LocalModel::up();
        let _held = local.try_take().unwrap();
        let waiter = local.clone();
        let wait = tokio::spawn(async move { waiter.take().await });
        tokio::task::yield_now().await;
        assert_eq!(local.waiting(), 1);
        let (mut rig, asked, _) = rig_at_limit(flags, weekly, local).await;
        rig.machine.pass().await;
        assert_eq!(claims(&asked), 0, "{name}");
        assert_eq!(
            blocked(&mut rig).await,
            None,
            "{name}: still running, not waiting for the limit"
        );
        wait.abort();
    }
}

#[tokio::test]
async fn claude_signed_out_still_holds_starts() {
    crate::tiers::set_local_limit_fallback_for_test(true);
    crate::tiers::set_local_ladder_for_test(true);
    for (name, flags, weekly) in CASES {
        let (mut rig, asked, _) = rig_at_limit(flags, weekly, LocalModel::up()).await;
        rig.machine.claude_signed_out_until = Some(in_a_minute());
        rig.machine.pass().await;
        assert_eq!(claims(&asked), 0, "{name}");
    }
    let (mut rig, _, _) = rig_at_limit(&["engine-start-rules"], false, LocalModel::up()).await;
    rig.machine.claude_signed_out_until = Some(in_a_minute());
    assert_eq!(blocked(&mut rig).await, Some("usage limit"));
}

#[tokio::test]
async fn with_the_model_down_or_the_flag_off_lanes_are_held_and_the_limit_shows() {
    crate::tiers::set_local_ladder_for_test(true);
    for fallback in [true, false] {
        crate::tiers::set_local_limit_fallback_for_test(fallback);
        let local = if fallback { LocalModel::off() } else { LocalModel::up() };
        for (name, flags, weekly) in CASES {
            let (mut rig, asked, _) = rig_at_limit(flags, weekly, local.clone()).await;
            let why = if weekly && !flags.contains(&"engine-start-rules") {
                "weekly limit"
            } else {
                "usage limit"
            };
            assert_eq!(blocked(&mut rig).await, Some(why), "{name} {fallback}");
            rig.machine.pass().await;
            assert_eq!(claims(&asked), 0, "{name} {fallback}");
            assert!(resets_at(&rig).is_some(), "{name} {fallback}");
        }
    }
}
