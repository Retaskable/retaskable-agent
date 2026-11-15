//! The usage recheck's reading applied to a held Claude (`driver/usage_recheck.rs`).
use super::*;
use crate::engines::EngineId;
use crate::usage_check::Reading;

fn held(rig: &Rig, after: u64) -> bool {
    let t = std::time::Instant::now() + Duration::from_secs(after);
    rig.machine.engine_limits.is_limited(EngineId::Claude, t)
}

#[tokio::test]
async fn a_reading_ends_the_hold_holds_again_until_the_reset_or_leaves_it_alone() {
    let mut rig = rig(1, &[]);
    let hold = std::time::Instant::now() + Duration::from_secs(3600);
    rig.machine.engine_limits.hit(EngineId::Claude, hold);
    rig.machine.apply_reading(Reading::Unread);
    assert!(held(&rig, 3000), "no reading leaves the hold");

    rig.machine.apply_reading(Reading::Answered);
    assert!(!held(&rig, 1), "an answer with room ends it");

    let resets = crate::telemetry::now_secs() + 7200;
    rig.machine
        .apply_reading(Reading::Event(json!({"status": "rejected", "resetsAt": resets})));
    assert!(
        held(&rig, 7000) && !held(&rig, 7300),
        "a rejection holds until its reset"
    );

    rig.machine
        .apply_reading(Reading::Event(json!({"status": "allowed", "resetsAt": resets})));
    assert!(!held(&rig, 1), "an allowed event ends it");
}
