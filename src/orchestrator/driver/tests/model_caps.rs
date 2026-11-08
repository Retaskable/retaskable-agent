//! `model-concurrency-caps`: a project whose next implementer start would wait for a full model cap claims nothing.
use super::*;
use crate::config::ModelConcurrency;
use crate::engines::model_slots::ModelSlots;

fn caps(opus: Option<u32>) -> ModelSlots {
    ModelSlots::new(ModelConcurrency { opus, sonnet: None })
}

/// A rig whose Fake holds `slots`, with the flag `on` and the usage reading `usage`.
async fn capped(slots: ModelSlots, on: bool, usage: Option<Value>, choice: Value) -> (Rig, Arc<Fake>, Asked) {
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    rig.machine.links.engine_choice = serde_json::from_value(choice).unwrap();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(if on {
        &["model-concurrency-caps"]
    } else {
        &[]
    }));
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
    let (mut rig, fake, asked) = capped(slots, true, None, json!({})).await;
    rig.machine.pass().await;
    assert!(!claimed(&asked), "{:?}", ops(&asked));
    assert!(fake.started.lock().unwrap().is_empty());
    drop(running);
    rig.machine.pass().await;
    assert!(claimed(&asked), "{:?}", ops(&asked));
    assert_eq!(fake.started.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_sonnet_tier_the_unset_cap_and_the_flag_off_claim_as_before() {
    let full = || {
        let s = caps(Some(1));
        let held = futures_util::FutureExt::now_or_never(s.acquire("opus")).unwrap();
        (s, held)
    };
    let (s, _held) = full();
    let (mut rig, _fake, asked) = capped(s, true, Some(sonnet_tier()), json!({})).await;
    rig.machine.pass().await;
    assert!(claimed(&asked), "sonnet tier: {:?}", ops(&asked));

    let (mut rig, _fake, asked) = capped(caps(None), true, None, json!({})).await;
    rig.machine.pass().await;
    assert!(claimed(&asked), "cap unset: {:?}", ops(&asked));

    let (s, _held) = full();
    let (mut rig, _fake, asked) = capped(s, false, None, json!({})).await;
    rig.machine.pass().await;
    assert!(claimed(&asked), "flag off: {:?}", ops(&asked));
}

#[tokio::test]
async fn a_cursor_lane_is_never_held_by_the_cap() {
    let s = caps(Some(1));
    let _held = s.acquire("opus").await;
    let (mut rig, _fake, asked) = capped(
        s,
        true,
        None,
        json!({ "default": "cursor", "lanes": ["cursor", "cursor"] }),
    )
    .await;
    rig.machine.pass().await;
    assert!(claimed(&asked), "{:?}", ops(&asked));
}
