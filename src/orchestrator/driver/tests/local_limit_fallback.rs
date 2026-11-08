//! `local-limit-fallback` (docs/specs/agent/07.2-local-model.md): Claude limited, with the model up, holds no
//! fresh work for a Claude lane, and at most one session queues for the one local slot.
use super::*;
use crate::engines::EngineId;
use crate::local_model::LocalModel;

fn in_a_minute() -> std::time::Instant {
    std::time::Instant::now() + Duration::from_secs(60)
}

/// A live rig with two lanes, the store handing out "b", these flags On, and the model `local`.
async fn rig_with(flags: &[&str], local: LocalModel, fallback: bool) -> (Rig, Asked, Arc<Fake>) {
    crate::tiers::set_local_ladder_for_test(true);
    crate::tiers::set_local_limit_fallback_for_test(fallback);
    let (base, asked) = claim_stub(
        json!([item("b", "claimed")]),
        Some(json!({"item": item("b", "claimed"), "token": "t"})),
    )
    .await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(flags));
    rig.machine.local = local;
    let fake = rig.go_live();
    (rig, asked, fake)
}

async fn blocked(rig: &mut Rig) -> Option<&'static str> {
    let m = &mut rig.machine;
    let shared = Shared {
        links: &m.links,
        local: &m.local,
        claim_paused_until: &mut m.claim_paused_until,
        claude_signed_out_until: &mut m.claude_signed_out_until,
        engine_limits: &mut m.engine_limits,
        stopping: false,
    };
    m.projects[0].start_blocked(&shared, &m.pool).await
}

fn claims(asked: &Asked) -> usize {
    ops(asked).iter().filter(|o| *o == "claim_next").count()
}

async fn resets(rig: &mut Rig) -> Option<String> {
    let blocked = blocked(rig).await;
    let m = &rig.machine;
    m.projects[0].report_limit(blocked, &m.engine_limits);
    m.projects[0].live.snapshot().projects["p0"].limit_resets_at.clone()
}

/// Claude limited: for the weekly set, the week cut off (the only limit the gate-Off path holds on), else a 5-hour one.
fn limit(rig: &Rig, flags: &[&str]) {
    if flags == SETS[1] {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let usage = json!({"unifiedWindows": {"seven_day": {"utilization": 0.96, "resetsAt": at + 3600}}});
        assert!(rig.machine.engine_limits.cut_off_for_week(&usage));
    } else {
        rig.machine.engine_limits.hit(EngineId::Claude, in_a_minute());
    }
}

const SETS: [&[&str]; 2] = [&["engine-start-rules"], &["weekly-cutoff"]];

#[tokio::test]
async fn limited_claude_with_the_model_up_claims_and_starts_and_is_not_blocked() {
    for flags in SETS {
        let (mut rig, asked, fake) = rig_with(flags, LocalModel::up(), true).await;
        limit(&rig, flags);
        rig.machine.pass().await;
        assert_eq!(claims(&asked), 1, "{flags:?}");
        assert_eq!(fake.started.lock().unwrap().len(), 1, "{flags:?}");
        assert_eq!(blocked(&mut rig).await, None, "{flags:?}");
        assert_eq!(resets(&mut rig).await, None, "{flags:?}");
    }
}

#[tokio::test]
async fn a_second_item_is_not_claimed_while_one_session_waits_for_the_slot() {
    let flags = SETS[0];
    let (mut rig, asked, fake) = rig_with(flags, LocalModel::up(), true).await;
    limit(&rig, flags);
    let _held = rig.machine.local.try_take().unwrap();
    let local = rig.machine.local.clone();
    let wait = tokio::spawn(async move { local.take().await });
    tokio::task::yield_now().await;
    assert_eq!(rig.machine.local.waiting(), 1);
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 0);
    assert!(fake.started.lock().unwrap().is_empty());
    wait.abort();
}

#[tokio::test]
async fn one_pass_claims_one_item_for_the_slot_not_the_whole_queue() {
    let flags = SETS[0];
    let (mut rig, asked, _) = rig_with(flags, LocalModel::up(), true).await;
    limit(&rig, flags);
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 1);
}

#[tokio::test]
async fn claude_signed_out_still_holds_starts() {
    for flags in SETS {
        let (mut rig, asked, _) = rig_with(flags, LocalModel::up(), true).await;
        limit(&rig, flags);
        rig.machine.claude_signed_out_until = Some(in_a_minute());
        rig.machine.pass().await;
        assert_eq!(claims(&asked), 0, "{flags:?}");
        assert!(blocked(&mut rig).await.is_some(), "{flags:?}");
    }
}

#[tokio::test]
async fn with_the_model_down_or_the_flag_off_lanes_are_held_as_before() {
    for (local, fallback) in [(LocalModel::off(), true), (LocalModel::up(), false)] {
        for (flags, why) in [(SETS[0], "usage limit"), (SETS[1], "weekly limit")] {
            let (mut rig, asked, _) = rig_with(flags, local.clone(), fallback).await;
            limit(&rig, flags);
            rig.machine.pass().await;
            assert_eq!(claims(&asked), 0, "{flags:?}");
            assert_eq!(blocked(&mut rig).await, Some(why), "{flags:?}");
            assert!(resets(&mut rig).await.is_some(), "{flags:?}");
        }
    }
}
