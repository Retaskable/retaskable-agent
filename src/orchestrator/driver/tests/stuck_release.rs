//! A release the store keeps refusing is dropped after 24 hours.
use super::settling::ended;
use super::*;

const DAY: i64 = 24 * 60 * 60;

fn error_events(rig: &mut Rig) -> usize {
    rig.lp(0)
        .events()
        .recent()
        .iter()
        .map(crate::telemetry::Entry::to_value)
        .filter(|e| e["kind"] == "error" && e["item"] == "a" && e["where"] == "release")
        .count()
}

/// A stuck release (refused, listed at clock 1000) with the clock moved `later` seconds.
async fn stuck(later: i64) -> Rig {
    let fail = Arc::new(AtomicBool::new(true));
    let (mut rig, _fake, _asked) = ended(SessionResult::Failed("x".into()), &fail).await;
    rig.lp(0).unreleased_since.insert("a".into(), 1000);
    rig.lp(0).clock = Some(1000 + later);
    rig.machine.pass().await;
    rig
}

#[tokio::test]
async fn a_release_refused_for_a_day_is_dropped_with_one_error_event() {
    let mut rig = stuck(DAY).await;
    assert!(rig.lp(0).unreleased.is_empty());
    assert!(rig.lp(0).store.unreleased().is_empty());
    assert_eq!(error_events(&mut rig), 1);
    rig.machine.pass().await;
    assert_eq!(error_events(&mut rig), 1, "a further pass adds none");
}

#[tokio::test]
async fn a_release_refused_for_less_than_a_day_is_kept() {
    let mut rig = stuck(DAY - 1).await;
    assert_eq!(rig.lp(0).store.unreleased()["a"].0, "lease-1");
    assert_eq!(error_events(&mut rig), 0);
}

#[tokio::test]
async fn listing_an_item_again_keeps_its_first_time_and_the_time_survives_a_restart() {
    let fail = Arc::new(AtomicBool::new(true));
    let (mut rig, _fake, _asked) = ended(SessionResult::Failed("x".into()), &fail).await;
    rig.lp(0).clock = Some(5000);
    rig.lp(0).unreleased_since.insert("a".into(), 1000);
    rig.lp(0).list_unreleased("a".into(), "lease-1".into(), "again".into());
    assert_eq!(rig.lp(0).unreleased_since["a"], 1000);
    rig.lp(0).save_leases();
    let reloaded = Store::load(&rig._dir.path().join("0")).unwrap();
    assert_eq!(reloaded.unreleased_since(9)["a"], 1000);
}
