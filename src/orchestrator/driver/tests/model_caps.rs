//! Model concurrency caps: a project whose next implementer start would wait for a full model cap claims nothing.
use super::*;
use crate::config::ModelConcurrency;
use crate::engines::model_slots::ModelSlots;

/// A `sonnet` cap: a claim names no tier here, so a fresh implementer runs on `sonnet`.
fn caps(sonnet: Option<u32>) -> ModelSlots {
    ModelSlots::new(ModelConcurrency { opus: None, sonnet })
}

/// A rig whose Fake holds `slots`.
async fn capped(slots: ModelSlots, choice: Value) -> (Rig, Arc<Fake>, Asked) {
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    rig.machine.links.engine_choice = serde_json::from_value(choice).unwrap();
    let fake = rig.go_live();
    *fake.slots.lock().unwrap() = Some(slots);
    (rig, fake, asked)
}

fn claimed(asked: &Asked) -> bool {
    ops(asked).contains(&"claim_next".to_string())
}

#[tokio::test]
async fn a_full_sonnet_cap_claims_nothing_until_the_session_ends() {
    let slots = caps(Some(1));
    let running = slots.acquire("sonnet").await;
    let (mut rig, fake, asked) = capped(slots, json!({})).await;
    rig.machine.pass().await;
    assert!(!claimed(&asked), "{:?}", ops(&asked));
    assert!(fake.started.lock().unwrap().is_empty());
    drop(running);
    rig.machine.pass().await;
    assert!(claimed(&asked), "{:?}", ops(&asked));
    assert_eq!(fake.started.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_full_opus_cap_and_the_unset_cap_claim_as_before() {
    // A claim with no tier runs on sonnet, so the opus cap doesn't hold it.
    let s = ModelSlots::new(ModelConcurrency {
        opus: Some(1),
        sonnet: None,
    });
    let _held = futures_util::FutureExt::now_or_never(s.acquire("opus")).unwrap();
    let (mut rig, _fake, asked) = capped(s, json!({})).await;
    rig.machine.pass().await;
    assert!(claimed(&asked), "opus cap: {:?}", ops(&asked));

    let (mut rig, _fake, asked) = capped(caps(None), json!({})).await;
    rig.machine.pass().await;
    assert!(claimed(&asked), "cap unset: {:?}", ops(&asked));
}

#[tokio::test]
async fn a_cursor_lane_is_never_held_by_the_cap() {
    let s = caps(Some(1));
    let _held = s.acquire("sonnet").await;
    let (mut rig, _fake, asked) = capped(s, json!({ "default": "cursor", "lanes": ["cursor", "cursor"] })).await;
    rig.machine.pass().await;
    assert!(claimed(&asked), "{:?}", ops(&asked));
}

#[tokio::test]
async fn with_a_claude_and_a_cursor_lane_the_claimed_item_starts_on_the_cursor_lane() {
    let s = caps(Some(1));
    let _held = s.acquire("sonnet").await;
    let (mut rig, fake, asked) = capped(s, json!({ "lanes": ["claude", "cursor"] })).await;
    rig.machine.pass().await;
    assert!(claimed(&asked), "{:?}", ops(&asked));
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].lane, 2);
}

#[tokio::test]
async fn starts_made_in_one_pass_count_against_the_cap() {
    let claims = |asked: &Asked| ops(asked).iter().filter(|o| *o == "claim_next").count();
    let (mut rig, fake, asked) = capped(caps(Some(1)), json!({})).await;
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert_eq!(claims(&asked), 1, "two free lanes, one opus slot: no second claim");
    let (mut rig, _fake, asked) = capped(caps(None), json!({})).await;
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 2, "without a cap both lanes ask");
}

#[tokio::test]
async fn starts_made_for_one_project_count_against_the_cap_for_the_next() {
    // The slots are shared by the computer, so project B sees project A's start made this pass.
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(2, &[&base, &base]);
    let fake = rig.go_live();
    *fake.slots.lock().unwrap() = Some(caps(Some(1)));
    rig.machine.pass().await;
    let claims = ops(&asked).iter().filter(|o| *o == "claim_next").count();
    assert_eq!(claims, 1, "{:?}", ops(&asked));
    assert_eq!(fake.started.lock().unwrap().len(), 1);
}
