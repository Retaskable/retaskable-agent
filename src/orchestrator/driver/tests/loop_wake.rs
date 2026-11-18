//! The project loop's wake (`spawn.rs`): a kick ends the sleep, a burst is one pass, and what raises it.
use super::*;
use crate::orchestrator::driver::spawn::run_until_kicked;
use std::time::Duration;
use tokio::time::Instant;

/// Runs the loop over no projects (a pass is instant) until `want` passes, kicking at `kicks` (offsets from the start); returns
/// each pass's offset from the start.
async fn pass_times(poll: Duration, kicks: &[(Duration, u32)], want: u32) -> Vec<Duration> {
    let mut rig = rig(1, &[]);
    let kick = rig.machine.links.work.kick_handle();
    let start = Instant::now();
    let seen = Mutex::new(Vec::new());
    let driver = async {
        for (at, times) in kicks {
            tokio::time::sleep_until(start + *at).await;
            for _ in 0..*times {
                kick.notify_one();
            }
        }
        std::future::pending::<()>().await;
    };
    let looping = run_until_kicked(&mut rig.machine, poll, &kick, |n| {
        seen.lock().unwrap().push(start.elapsed());
        n >= want
    });
    tokio::select! {
        () = looping => {}
        () = driver => {}
    }
    seen.into_inner().unwrap()
}

const S: fn(u64) -> Duration = Duration::from_secs;

#[tokio::test(start_paused = true)]
async fn a_kick_ends_the_sleep_at_once_and_a_burst_is_one_pass_a_second_on() {
    // pass 1 at the first tick; a kick at 5 s runs at once (not at 30 s); two kicks at 5.5 s make one pass, at the 1 s floor;
    // the next beat is a period after that pass.
    let kicks = [(S(5), 1), (Duration::from_millis(5500), 2)];
    let times = pass_times(S(30), &kicks, 4).await;
    assert_eq!(times, vec![S(0), S(5), S(6), S(36)]);
}

#[tokio::test(start_paused = true)]
async fn without_a_kick_the_beat_is_the_period() {
    let times = pass_times(S(30), &[], 3).await;
    assert_eq!(times, vec![S(0), S(30), S(60)]);
}

#[tokio::test(start_paused = true)]
async fn kicks_in_a_steady_stream_cannot_make_passes_closer_than_a_second() {
    let kicks: Vec<_> = (1..=40).map(|i| (Duration::from_millis(100 * i), 1)).collect();
    let times = pass_times(S(30), &kicks, 5).await;
    assert!(times.windows(2).skip(1).all(|w| w[1] - w[0] >= S(1)), "{times:?}");
}

#[tokio::test]
async fn a_run_ending_raises_the_wake_and_an_unkicked_sender_does_not() {
    use crate::orchestrator::driver::{RunEnd, RunEndSender};
    use futures_util::FutureExt;
    let end = || RunEnd {
        token: "t".into(),
        outcome: crate::backlog::runs::RunOutcome::Merged,
        result: json!({}),
    };
    let work = crate::work_signal::WorkSignals::default();
    let kick = work.kick_handle();
    let (plain, _rx) = RunEndSender::channel();
    plain.send(end());
    assert!(kick.notified().now_or_never().is_none());
    let (kicking, mut rx) = RunEndSender::channel();
    kicking.kicking(kick.clone()).send(end());
    assert!(kick.notified().now_or_never().is_some());
    assert!(rx.try_recv().is_ok(), "the end is still sent");
}
