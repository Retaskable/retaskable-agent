//! The Fix button's rows on a project's loop (`driver/fix_requests.rs`): only the rollout's runner lists and claims
//! them, and not while the project is paused (this computer's pause, or the store's: both are the loop's `paused`) or the
//! computer is signed out of Claude. Each case ends with the row claimed once the hold is lifted, so a pass that claimed nothing is a real hold.
use super::Asked;
use super::asks::{Listed, ops_named, push, store_failing};
use super::ci_pass::{Host, Shared};
use super::*;
use crate::orchestrator::driver::CiWatchHost;

fn row() -> Value {
    json!({ "id": "in-1", "kind": "fix_requested", "status": "requested", "issuedBy": "ana",
        "args": { "pr": 12, "sha": "abc1234" } })
}

/// A live project watched by a fake code host whose store lists the row; the runner flag is `runner`.
async fn rig_fixing(runner: bool) -> (Rig, Asked, Listed) {
    let (base, asked, listed) = store_failing("").await;
    let mut rig = rig(1, &[&base]);
    rig.go_live();
    let watch: Arc<dyn CiWatchHost> = Arc::new(Shared(Arc::new(Host::default())));
    rig.machine.ci_watch = Some(watch);
    rig.machine.runners.flag("p0").store(runner, Ordering::SeqCst);
    rig.lp(0).fixes.poll_every = Duration::ZERO;
    push(&listed, row());
    (rig, asked, listed)
}

fn claims(asked: &Asked) -> usize {
    ops_named(asked, "intent_claim").len()
}

fn lists(asked: &Asked) -> usize {
    ops_named(asked, "intent_list")
        .iter()
        .filter(|a| a["kinds"] == json!(["fix_requested"]))
        .count()
}

#[tokio::test]
async fn a_computer_that_is_not_the_runner_leaves_the_row_alone_and_the_runner_claims_it() {
    let (mut rig, asked, _) = rig_fixing(false).await;
    rig.machine.pass().await;
    // Not even listed, so not claimed to be refused.
    assert_eq!((lists(&asked), claims(&asked)), (0, 0));
    // The store names it the runner: the same row is taken on the next pass.
    rig.machine.runners.flag("p0").store(true, Ordering::SeqCst);
    rig.machine.pass().await;
    assert_eq!((lists(&asked), claims(&asked)), (1, 1));
    assert_eq!(ops_named(&asked, "intent_claim")[0]["id"], "in-1");
}

#[tokio::test]
async fn a_paused_project_claims_no_fix_request_until_it_is_resumed() {
    let (mut rig, asked, _) = rig_fixing(true).await;
    rig.pause("p0", true);
    rig.machine.pass().await;
    assert_eq!((lists(&asked), claims(&asked)), (1, 0));
    rig.pause("p0", false);
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 1);
}

#[tokio::test]
async fn a_computer_signed_out_of_claude_claims_no_fix_request_until_it_is_signed_in() {
    let (mut rig, asked, _) = rig_fixing(true).await;
    rig.machine.claude_signed_out_until = Some(std::time::Instant::now() + Duration::from_secs(3600));
    rig.machine.pass().await;
    assert_eq!((lists(&asked), claims(&asked)), (1, 0));
    rig.machine.claude_signed_out_until = None;
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 1);
}
