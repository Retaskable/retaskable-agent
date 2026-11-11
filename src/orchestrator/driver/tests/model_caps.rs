//! Model concurrency caps: a project whose next implementer start would wait for a full model cap claims nothing.
use super::*;
use crate::config::ModelConcurrency;
use crate::engines::model_slots::ModelSlots;

fn caps(opus: Option<u32>) -> ModelSlots {
    ModelSlots::new(ModelConcurrency { opus, sonnet: None })
}

/// A rig whose Fake holds `slots`, with the usage reading `usage`.
async fn capped(slots: ModelSlots, usage: Option<Value>, choice: Value) -> (Rig, Arc<Fake>, Asked) {
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    rig.machine.links.engine_choice = serde_json::from_value(choice).unwrap();
    if let Some(usage) = usage {
        rig.lp(0).store.set_usage(usage);
    }
    let fake = rig.go_live();
    *fake.slots.lock().unwrap() = Some(slots);
    (rig, fake, asked)
}

fn claimed(asked: &Asked) -> bool {
    ops(asked).contains(&"claim_next".to_string())
}

fn sonnet_tier() -> Value {
    let resets = crate::telemetry::now_ms() / 1000 + 3600;
    json!({"unifiedWindows": {"five_hour": {"utilization": 0.85, "resetsAt": resets}}})
}

#[tokio::test]
async fn a_full_opus_cap_on_the_opus_tier_claims_nothing_until_the_session_ends() {
    let slots = caps(Some(1));
    let running = slots.acquire("opus").await;
    let (mut rig, fake, asked) = capped(slots, None, json!({})).await;
    rig.machine.pass().await;
    assert!(!claimed(&asked), "{:?}", ops(&asked));
    assert!(fake.started.lock().unwrap().is_empty());
    drop(running);
    rig.machine.pass().await;
    assert!(claimed(&asked), "{:?}", ops(&asked));
    assert_eq!(fake.started.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_sonnet_tier_and_the_unset_cap_claim_as_before() {
    let full = || {
        let s = caps(Some(1));
        let held = futures_util::FutureExt::now_or_never(s.acquire("opus")).unwrap();
        (s, held)
    };
    let (s, _held) = full();
    let (mut rig, _fake, asked) = capped(s, Some(sonnet_tier()), json!({})).await;
    rig.machine.pass().await;
    assert!(claimed(&asked), "sonnet tier: {:?}", ops(&asked));

    let (mut rig, _fake, asked) = capped(caps(None), None, json!({})).await;
    rig.machine.pass().await;
    assert!(claimed(&asked), "cap unset: {:?}", ops(&asked));
}

#[tokio::test]
async fn a_cursor_lane_is_never_held_by_the_cap() {
    let s = caps(Some(1));
    let _held = s.acquire("opus").await;
    let (mut rig, _fake, asked) = capped(s, None, json!({ "default": "cursor", "lanes": ["cursor", "cursor"] })).await;
    rig.machine.pass().await;
    assert!(claimed(&asked), "{:?}", ops(&asked));
}

#[tokio::test]
async fn with_a_claude_and_a_cursor_lane_the_claimed_item_starts_on_the_cursor_lane() {
    let s = caps(Some(1));
    let _held = s.acquire("opus").await;
    let (mut rig, fake, asked) = capped(s, None, json!({ "lanes": ["claude", "cursor"] })).await;
    rig.machine.pass().await;
    assert!(claimed(&asked), "{:?}", ops(&asked));
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].lane, 2);
}

#[tokio::test]
async fn a_full_sonnet_cap_on_the_opus_tier_claims_nothing() {
    // A first try runs on sonnet whatever the tier says (`implementer_model`), so it would wait on that slot.
    let s = ModelSlots::new(ModelConcurrency {
        opus: None,
        sonnet: Some(1),
    });
    let _held = s.acquire("sonnet").await;
    let (mut rig, fake, asked) = capped(s, None, json!({})).await;
    rig.machine.pass().await;
    assert!(!claimed(&asked), "{:?}", ops(&asked));
    assert!(fake.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn starts_made_in_one_pass_count_against_the_cap() {
    let claims = |asked: &Asked| ops(asked).iter().filter(|o| *o == "claim_next").count();
    let (mut rig, fake, asked) = capped(caps(Some(1)), None, json!({})).await;
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert_eq!(claims(&asked), 1, "two free lanes, one opus slot: no second claim");
    let (mut rig, _fake, asked) = capped(caps(None), None, json!({})).await;
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
