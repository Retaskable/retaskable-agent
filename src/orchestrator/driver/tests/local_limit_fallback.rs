//! The local limit fallback (docs/specs/agent/07.2-local-model.md): Claude limited, with the model up, holds no
//! fresh work for a Claude lane, and at most one session queues for the one local slot.
use super::*;
use crate::engines::EngineId;
use crate::local_model::LocalModel;

fn in_a_minute() -> std::time::Instant {
    std::time::Instant::now() + Duration::from_secs(60)
}

/// A live rig with two lanes, the store handing out "b", and the model `local`.
async fn rig_with(local: LocalModel) -> (Rig, Asked, Arc<Fake>) {
    let (base, asked) = claim_stub(
        json!([item("b", "claimed")]),
        Some(json!({"item": item("b", "claimed"), "token": "t"})),
    )
    .await;
    let mut rig = rig(2, &[&base]);
    rig.machine.local = local;
    let fake = rig.go_live();
    (rig, asked, fake)
}

fn blocked(rig: &mut Rig) -> Option<&'static str> {
    let m = &mut rig.machine;
    let shared = Shared {
        links: &m.links,
        local: &m.local,
        claim_paused_until: &mut m.claim_paused_until,
        claude_signed_out_until: &mut m.claude_signed_out_until,
        engine_limits: &mut m.engine_limits,
        stopping: false,
    };
    m.projects[0].start_blocked(&shared, &m.pool)
}

fn claims(asked: &Asked) -> usize {
    ops(asked).iter().filter(|o| *o == "claim_next").count()
}

fn resets(rig: &mut Rig) -> Option<String> {
    let blocked = blocked(rig);
    let m = &rig.machine;
    m.projects[0].report_limit(blocked, &m.engine_limits);
    m.projects[0].live.snapshot().projects["p0"].limit_resets_at.clone()
}

/// Claude limited by a 5-hour limit.
fn limit(rig: &Rig) {
    rig.machine.engine_limits.hit(EngineId::Claude, in_a_minute());
}

#[tokio::test]
async fn limited_claude_with_the_model_up_claims_and_starts_and_is_not_blocked() {
    let (mut rig, asked, fake) = rig_with(LocalModel::up()).await;
    limit(&rig);
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 1);
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert_eq!(blocked(&mut rig), None);
    assert_eq!(resets(&mut rig), None);
}

#[tokio::test]
async fn a_second_item_is_not_claimed_while_one_session_waits_for_the_slot() {
    let (mut rig, asked, fake) = rig_with(LocalModel::up()).await;
    limit(&rig);
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
    let (mut rig, asked, _) = rig_with(LocalModel::up()).await;
    limit(&rig);
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 1);
}

#[tokio::test]
async fn claude_signed_out_still_holds_starts() {
    let (mut rig, asked, _) = rig_with(LocalModel::up()).await;
    limit(&rig);
    rig.machine.claude_signed_out_until = Some(in_a_minute());
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 0);
    assert!(blocked(&mut rig).is_some());
}

#[tokio::test]
async fn with_the_model_down_lanes_are_held_as_before() {
    let (mut rig, asked, _) = rig_with(LocalModel::off()).await;
    limit(&rig);
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 0);
    assert_eq!(blocked(&mut rig), Some("usage limit"));
    assert!(resets(&mut rig).is_some());
}
