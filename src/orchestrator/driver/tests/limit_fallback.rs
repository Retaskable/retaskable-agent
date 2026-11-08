//! `local-limit-fallback`: with the model up, Claude at its limit (not signed out) no longer holds fresh work for a
//! Claude lane; it claims and starts, and the snapshot says running (docs/specs/agent/07.2-local-model.md).
use super::*;
use crate::engines::EngineId;
use crate::local_model::LocalModel;

fn in_a_minute() -> std::time::Instant {
    std::time::Instant::now() + Duration::from_secs(60)
}

/// One lane, a claimable item "b", these flags On, the fallback flag as `fallback`, the model as `local`, and
/// Claude limited (the 5-hour limit, or the weekly cutoff when `weekly`).
async fn limited_rig(flags: &[&str], fallback: bool, local: LocalModel, weekly: bool) -> (Rig, Asked, Arc<Fake>) {
    crate::tiers::set_local_ladder_for_test(true);
    crate::tiers::set_local_limit_fallback_for_test(fallback);
    let (base, asked) = claim_stub(
        json!([item("b", "claimed")]),
        Some(json!({"item": item("b", "claimed"), "token": "t"})),
    )
    .await;
    let mut rig = rig(1, &[&base]);
    let mut all = flags.to_vec();
    if weekly {
        all.push("weekly-cutoff");
    }
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&all));
    let fake = rig.go_live();
    rig.machine.local = local;
    rig.machine.engine_limits.hit(EngineId::Claude, in_a_minute());
    if weekly {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let week = json!({"unifiedWindows": {"seven_day": {"utilization": 0.96, "resetsAt": at + 3600}}});
        assert!(rig.machine.engine_limits.cut_off_for_week(&week));
    }
    (rig, asked, fake)
}

fn claims(asked: &Asked) -> usize {
    ops(asked).iter().filter(|o| *o == "claim_next").count()
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

fn limit_shown(rig: &Rig) -> bool {
    rig.machine.projects[0].live.snapshot().projects["p0"]
        .limit_resets_at
        .is_some()
}

#[tokio::test]
async fn at_the_five_hour_limit_a_claude_lane_claims_and_starts_with_the_model_up() {
    let (mut rig, asked, fake) = limited_rig(&["engine-start-rules"], true, LocalModel::up(), false).await;
    assert_eq!(blocked(&mut rig).await, None);
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 1);
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert!(!limit_shown(&rig));
}

#[tokio::test]
async fn at_the_weekly_cutoff_with_the_start_rules_off_a_claude_lane_claims_and_starts() {
    let (mut rig, asked, fake) = limited_rig(&[], true, LocalModel::up(), true).await;
    assert_eq!(blocked(&mut rig).await, None);
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 1);
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert!(!limit_shown(&rig));
}

#[tokio::test]
async fn a_session_waiting_for_the_slot_holds_the_next_claim() {
    let (mut rig, asked, fake) = limited_rig(&["engine-start-rules"], true, LocalModel::up(), false).await;
    let local = rig.machine.local.clone();
    let holder = local.try_take().unwrap();
    let waiter = local.clone();
    let wait = tokio::spawn(async move { waiter.take().await });
    tokio::task::yield_now().await;
    assert_eq!(local.waiting(), 1);
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 0);
    assert!(fake.started.lock().unwrap().is_empty());
    // The snapshot still reads running: the lane can start once the slot clears.
    assert_eq!(blocked(&mut rig).await, None);
    wait.abort();
    drop(holder);
}

#[tokio::test]
async fn claude_signed_out_still_holds_starts() {
    let (mut rig, asked, _) = limited_rig(&["engine-start-rules"], true, LocalModel::up(), false).await;
    rig.machine.claude_signed_out_until = Some(in_a_minute());
    assert_eq!(blocked(&mut rig).await, Some("usage limit"));
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 0);
}

#[tokio::test]
async fn with_the_model_down_or_the_flag_off_the_lanes_are_held_as_before() {
    for (fallback, local) in [(true, LocalModel::off()), (false, LocalModel::up())] {
        let (mut rig, asked, fake) = limited_rig(&["engine-start-rules"], fallback, local, false).await;
        assert_eq!(blocked(&mut rig).await, Some("usage limit"));
        rig.machine.pass().await;
        assert_eq!(claims(&asked), 0);
        assert!(fake.started.lock().unwrap().is_empty());
        assert!(limit_shown(&rig));
    }
    let (mut rig, asked, _) = limited_rig(&[], false, LocalModel::up(), true).await;
    assert_eq!(blocked(&mut rig).await, Some("weekly limit"));
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 0);
}
