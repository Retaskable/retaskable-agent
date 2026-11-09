//! `stuck-release-drop`: a release the store keeps refusing is dropped after 24 hours.
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

/// A stuck release (refused, listed at clock 1000) with `flag` and the clock moved `later` seconds.
async fn stuck(flag: bool, later: i64) -> Rig {
    let fail = Arc::new(AtomicBool::new(true));
    let (mut rig, _fake, _asked) = ended(SessionResult::Failed("x".into()), &fail).await;
    let flags: &[&str] = if flag { &["stuck-release-drop"] } else { &[] };
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(flags));
    rig.lp(0).unreleased_since.insert("a".into(), 1000);
    rig.lp(0).clock = Some(1000 + later);
    rig.machine.pass().await;
    rig
}

#[tokio::test]
async fn a_release_refused_for_a_day_is_dropped_with_one_error_event() {
    let mut rig = stuck(true, DAY).await;
    assert!(rig.lp(0).unreleased.is_empty());
    assert!(rig.lp(0).store.unreleased().is_empty());
    assert_eq!(error_events(&mut rig), 1);
    rig.machine.pass().await;
    assert_eq!(error_events(&mut rig), 1, "a further pass adds none");
}

#[tokio::test]
async fn a_release_refused_for_less_than_a_day_is_kept() {
    let mut rig = stuck(true, DAY - 1).await;
    assert_eq!(rig.lp(0).store.unreleased()["a"].0, "lease-1");
    assert_eq!(error_events(&mut rig), 0);
}

#[tokio::test]
async fn with_the_flag_off_an_old_stuck_release_is_kept() {
    let mut rig = stuck(false, DAY * 3).await;
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
