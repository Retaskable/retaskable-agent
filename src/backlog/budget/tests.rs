use super::*;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[test]
fn a_quiet_computer_sends_a_burst_at_once_then_one_call_every_interval() {
    let t0 = Instant::now();
    let mut b = Bucket::new(60, 5, t0);
    for _ in 0..5 {
        assert_eq!(b.take(t0), Ok(Duration::ZERO));
    }
    // The sixth waits one interval (a second at 60 a minute), the seventh two.
    assert_eq!(b.take(t0), Ok(secs(1)));
    assert_eq!(b.take(t0), Ok(secs(2)));
}

#[test]
fn the_bucket_refills_while_nothing_is_sent() {
    let t0 = Instant::now();
    let mut b = Bucket::new(60, 5, t0);
    for _ in 0..8 {
        let _ = b.take(t0);
    }
    assert_eq!(
        b.take(t0 + secs(60)),
        Ok(Duration::ZERO),
        "a quiet minute fills it again"
    );
}

#[test]
fn five_projects_calling_as_fast_as_they_like_stay_under_the_stores_limit() {
    // A minute of 5 projects x 40 calls: no 60-second window holds more than PER_MINUTE plus the burst.
    let t0 = Instant::now();
    let mut b = Bucket::new(PER_MINUTE, BURST, t0);
    let mut at = Vec::new();
    for _ in 0..200 {
        match b.take(t0) {
            Ok(wait) => at.push(wait),
            Err(_) => break,
        }
    }
    let window = at.iter().filter(|w| **w < secs(60)).count() as u32;
    assert!(window <= PER_MINUTE + BURST, "{window} calls in the first minute");
    assert!(window < 120, "under the store's 120 an agent");
}

#[test]
fn a_call_that_would_wait_too_long_is_refused_and_keeps_no_place() {
    let t0 = Instant::now();
    let mut b = Bucket::new(60, 1, t0);
    for _ in 0..91 {
        assert!(b.take(t0).is_ok());
    }
    assert!(b.take(t0).is_err());
    assert!(b.take(t0).is_err(), "the refused one took no turn");
    assert!(b.take(t0 + secs(30)).is_ok(), "30 s on, the line has moved");
}

#[test]
fn one_429_holds_every_call_for_its_retry_after() {
    let t0 = Instant::now();
    let mut b = Bucket::new(PER_MINUTE, BURST, t0);
    b.limited(t0, Some(secs(80)));
    assert_eq!(b.take(t0), Ok(secs(80)));
    assert_eq!(b.take(t0 + secs(60)), Ok(secs(20)), "any project, the same hold");
    assert_eq!(b.take(t0 + secs(80)), Ok(Duration::ZERO));
}

#[test]
fn a_429_without_retry_after_holds_a_minute_and_a_short_or_absurd_one_is_bounded() {
    let t0 = Instant::now();
    for (said, held) in [(None, 60), (Some(5), 60), (Some(200), 200), (Some(10_000_000), 300)] {
        let mut b = Bucket::new(PER_MINUTE, BURST, t0);
        b.limited(t0, said.map(secs));
        assert_eq!(b.held_for(t0), secs(held), "{said:?}");
    }
}

#[test]
fn a_second_429_during_a_hold_does_not_shorten_or_stack_it() {
    let t0 = Instant::now();
    let mut b = Bucket::new(PER_MINUTE, BURST, t0);
    b.limited(t0, Some(secs(120)));
    b.limited(t0 + secs(10), Some(secs(60)));
    assert_eq!(b.held_for(t0 + secs(10)), secs(110));
}

#[test]
fn after_a_hold_the_queue_drains_at_the_pace_not_all_at_once() {
    let t0 = Instant::now();
    let mut b = Bucket::new(PER_MINUTE, BURST, t0);
    b.limited(t0, None);
    let waits: Vec<_> = (0..BURST + 10).map(|_| b.take(t0).unwrap()).collect();
    assert!(waits[..BURST as usize].iter().all(|w| *w == secs(60)));
    assert!(waits[BURST as usize + 5] > secs(60), "the rest are spread out after it");
}

#[test]
fn a_store_on_this_computer_is_not_limited() {
    for url in ["http://127.0.0.1:8787", "http://localhost:3000/", "http://[::1]:80"] {
        assert!(for_store(url).0.is_none(), "{url}");
    }
    assert!(for_store("https://backlog.retaskable.com").0.is_some());
    assert!(
        Arc::ptr_eq(
            &for_store("https://backlog.retaskable.com"),
            &for_store("https://backlog.retaskable.com/")
        ),
        "one budget for the store, however many clients"
    );
}

#[tokio::test]
async fn a_call_over_its_longest_wait_is_a_429_naming_the_wait() {
    let b = Budget::limited_store();
    b.limited(Some(secs(200)));
    let e = b.wait().await.unwrap_err();
    assert!(e.retry_after().is_some_and(|w| w >= secs(199)), "{e}");
    let BacklogError::BadAnswer { status, .. } = e else {
        panic!()
    };
    assert_eq!(status, 429);
}

#[tokio::test(start_paused = true)]
async fn calls_wait_their_turn() {
    let b = Budget::limited_store();
    let start = tokio::time::Instant::now();
    for _ in 0..BURST + 3 {
        b.wait().await.unwrap();
    }
    let took = start.elapsed();
    assert!(took >= secs(2) && took < secs(5), "{took:?}");
}
