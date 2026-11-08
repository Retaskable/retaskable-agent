use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

const KEY: &str = "h7-mmm";
const T0: i64 = 1_800_000_000_000;

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).unwrap().to_rfc3339()
}

fn st(computer: &str, state: &str, at: i64) -> Status {
    Status {
        context: report::PR_CONTEXT.into(),
        state: state.into(),
        description: describe(&format!("agent-ci {computer}: running"), KEY),
        creator: "bot".into(),
        updated_at: iso(at),
    }
}

const MIN: i64 = 60_000;

#[test]
fn the_earliest_claim_wins_and_a_tie_goes_to_the_lower_name() {
    let s = [st("b", "pending", T0), st("a", "pending", T0 + MIN)];
    assert_eq!(decide(&s, KEY, "b", T0 + 2 * MIN), Verdict::Run { stale: vec![] });
    assert_eq!(decide(&s, KEY, "a", T0 + 2 * MIN), Verdict::Lose);
    let tie = [st("b", "pending", T0), st("a", "pending", T0)];
    assert_eq!(decide(&tie, KEY, "a", T0), Verdict::Run { stale: vec![] });
    assert_eq!(decide(&tie, KEY, "b", T0), Verdict::Lose);
}

#[test]
fn a_refreshed_claim_keeps_its_original_start() {
    let s = [
        st("b", "pending", T0 + 10 * MIN),
        st("b", "pending", T0),
        st("a", "pending", T0 + 5 * MIN),
    ];
    assert_eq!(decide(&s, KEY, "a", T0 + 11 * MIN), Verdict::Lose);
}

#[test]
fn a_claim_fifteen_minutes_old_is_stale_and_fourteen_is_not() {
    let s = [st("b", "pending", T0)];
    assert_eq!(decide(&s, KEY, "a", T0 + 14 * MIN), Verdict::Lose);
    assert_eq!(
        decide(&s, KEY, "a", T0 + 15 * MIN),
        Verdict::Run {
            stale: vec!["b".into()]
        }
    );
}

#[test]
fn a_finished_key_is_never_run_again() {
    for state in ["success", "failure"] {
        let s = [st("b", state, T0), st("b", "pending", T0 - MIN)];
        assert_eq!(decide(&s, KEY, "a", T0 + 99 * MIN), Verdict::Finished);
    }
    // Another key, or another context, doesn't count.
    let mut other = st("b", "success", T0);
    other.description = describe("agent-ci b: passed", "x-y");
    let mut push = st("b", "success", T0);
    push.context = report::PUSH_CONTEXT.into();
    assert!(matches!(decide(&[other, push], KEY, "a", T0), Verdict::Run { .. }));
}

#[tokio::test]
async fn a_refresh_is_posted_every_interval_during_a_long_run() {
    let n = AtomicU32::new(0);
    let out = refreshing(
        Duration::from_millis(20),
        || async {
            n.fetch_add(1, Ordering::SeqCst);
        },
        async {
            tokio::time::sleep(Duration::from_millis(110)).await;
            7
        },
    )
    .await;
    assert_eq!(out, 7);
    assert!(
        (4..=5).contains(&n.load(Ordering::SeqCst)),
        "{}",
        n.load(Ordering::SeqCst)
    );
    assert_eq!(REFRESH, Duration::from_secs(300));
}

#[test]
fn the_description_keeps_the_key_when_cut() {
    let d = describe(&"x".repeat(300), KEY);
    assert_eq!(d.chars().count(), 140);
    assert!(d.ends_with(" (h7-mmm)"));
}
