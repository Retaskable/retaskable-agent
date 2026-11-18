//! A queue held at the usage limit: kept, told in the history, and sent when the limit lifts or the person writes again.
use super::*;
use crate::engines::EngineId;
use std::time::Instant;

fn notes(x: &Ex) -> Vec<String> {
    (x.st.args("report").iter())
        .filter_map(|n| n["note"].as_str().map(String::from))
        .collect()
}

fn limit_notes(x: &Ex) -> Vec<String> {
    notes(x)
        .into_iter()
        .filter(|n| n.starts_with("At the usage limit"))
        .collect()
}

/// The first message runs and is held at the limit while the second waits behind it.
async fn limited_with_one_queued(x: &mut Ex) {
    x.host.hold.store(true, Ordering::SeqCst);
    x.host.limited.store(true, Ordering::SeqCst);
    x.start("s1", "Fix it");
    x.until("a turn running", |x| x.jobs().len() == 1).await;
    x.say("m", ID, "second", false);
    x.quiet().await;
    x.host.hold.store(false, Ordering::SeqCst);
    x.host.go.notify_one();
    x.until("the hold told", |x| !limit_notes(x).is_empty()).await;
}

#[tokio::test]
async fn a_turn_at_the_limit_with_a_message_queued_holds_it_and_says_when_it_goes() {
    let mut x = ex().await;
    limited_with_one_queued(&mut x).await;
    let said = limit_notes(&x);
    assert_eq!(said.len(), 1);
    let at = said[0].strip_prefix("At the usage limit: sends at ").unwrap();
    assert!(at.len() == 5 && at.as_bytes()[2] == b':', "{at}");
    x.quiet().await;
    assert_eq!(x.jobs().len(), 1, "kept, not sent");
    assert_eq!(x.expert(ID).unwrap()["queue"].as_array().map(Vec::len), Some(1));
    assert!(
        notes(&x)
            .iter()
            .any(|n| n.starts_with("The turn failed: You've hit your limit"))
    );
}

#[tokio::test]
async fn a_held_queue_goes_when_the_time_comes() {
    let mut x = ex().await;
    // No reset time is known here, so the hold lasts the floor.
    x.rig.lp(0).experts.limit_floor = Some(Duration::from_millis(300));
    limited_with_one_queued(&mut x).await;
    x.host.limited.store(false, Ordering::SeqCst);
    x.quiet().await;
    assert_eq!(x.jobs().len(), 1, "not before");
    x.until("sent", |x| x.jobs().len() == 2).await;
    assert_eq!(x.jobs()[1].message, "second");
}

#[tokio::test]
async fn a_held_queue_goes_at_the_limits_reset_when_that_is_later_than_the_floor() {
    let mut x = ex().await;
    x.rig.lp(0).experts.limit_floor = Some(Duration::from_millis(1));
    let reset = Instant::now() + Duration::from_millis(400);
    x.rig.machine.engine_limits.hit(EngineId::Claude, reset);
    limited_with_one_queued(&mut x).await;
    x.host.limited.store(false, Ordering::SeqCst);
    // Seen sent, then the clock read: if that is still before the reset, it went early.
    x.until("sent", |x| {
        let sent = x.jobs().len() == 2;
        assert!(!sent || Instant::now() >= reset, "sent before the reset");
        sent
    })
    .await;
    // The reset is fixed up front, so a slow setup can't make this wait shorter: the floor was 1ms.
    assert!(Instant::now() >= reset, "waited for the reset, not the floor");
}

#[tokio::test]
async fn a_new_message_from_the_person_tries_again_at_once() {
    let mut x = ex().await;
    limited_with_one_queued(&mut x).await;
    x.host.limited.store(false, Ordering::SeqCst);
    x.say("m2", ID, "third", false);
    x.until("the oldest sent", |x| x.jobs().len() == 2).await;
    assert_eq!(x.jobs()[1].message, "second", "first in, first out");
}

#[tokio::test]
async fn nothing_is_held_when_nothing_is_queued_or_the_limit_wasnt_hit() {
    let mut x = ex().await;
    x.host.limited.store(true, Ordering::SeqCst);
    x.start("s1", "Fix it");
    x.until("failed", |x| notes(x).iter().any(|n| n.starts_with("The turn failed")))
        .await;
    x.quiet().await;
    assert!(limit_notes(&x).is_empty());
    x.host.limited.store(false, Ordering::SeqCst);
    x.say("m", ID, "again", false);
    x.until("it runs", |x| x.jobs().len() == 2).await;
}
