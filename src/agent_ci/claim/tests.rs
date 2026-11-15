use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

const T0: i64 = 1_800_000_000_000;
const MIN: i64 = 60_000;

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).unwrap().to_rfc3339()
}

fn st(description: &str, state: &str, at: i64) -> Status {
    Status {
        context: crate::agent_ci::report::PR_CONTEXT.into(),
        state: state.into(),
        description: description.into(),
        creator: "bot".into(),
        updated_at: iso(at),
    }
}

const ME: &str = "agent-ci a";

#[test]
fn no_status_wants_a_run() {
    assert!(wants_run(None, ME, T0, true));
}

#[test]
fn another_cis_status_wants_a_run_unless_it_passed() {
    assert!(wants_run(Some(&st("buildkite", "failure", T0)), ME, T0, true));
    assert!(wants_run(Some(&st("buildkite", "pending", T0)), ME, T0, true));
    assert!(!wants_run(Some(&st("buildkite", "success", T0)), ME, T0, true));
}

#[test]
fn a_finished_agent_ci_status_wants_no_run() {
    for state in ["success", "failure"] {
        assert!(!wants_run(
            Some(&st("agent-ci b: x", state, T0)),
            ME,
            T0 + 99 * MIN,
            true
        ));
    }
}

#[test]
fn a_leased_pending_status_wants_a_run_whoever_wrote_it() {
    let s = st("agent-ci b: running", "pending", T0);
    assert!(wants_run(Some(&s), ME, T0, true));
}

#[test]
fn without_a_lease_our_own_pending_wants_a_run_and_anothers_only_once_stale() {
    let own = st("agent-ci a: running", "pending", T0);
    assert!(wants_run(Some(&own), ME, T0, false));
    let other = st("agent-ci b: running", "pending", T0);
    assert!(!wants_run(Some(&other), ME, T0 + 8 * MIN, false));
    assert!(wants_run(Some(&other), ME, T0 + 8 * MIN + 1, false));
    let mut unreadable = other.clone();
    unreadable.updated_at = "nope".into();
    assert!(!wants_run(Some(&unreadable), ME, T0 + 99 * MIN, false));
}

#[test]
fn the_newest_status_of_a_context_is_the_first_listed() {
    let mut push = st("agent-ci b: passed", "success", T0);
    push.context = crate::agent_ci::report::PUSH_CONTEXT.into();
    let list = [
        push,
        st("agent-ci a: running", "pending", T0),
        st("old", "failure", T0 - MIN),
    ];
    assert_eq!(
        newest(&list, crate::agent_ci::report::PR_CONTEXT).unwrap().state,
        "pending"
    );
    assert!(newest(&list, "other").is_none());
}

// On paused time: the clock moves only when every task waits, so the count is exact, not a race with real time.
#[tokio::test(start_paused = true)]
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
    // At 20, 40, 60, 80 and 100 ms; the work ends at 110.
    assert_eq!(n.load(Ordering::SeqCst), 5);
    assert_eq!(REFRESH, Duration::from_secs(300));
}
