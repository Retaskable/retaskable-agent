//! Concurrency caps, stopping, a `do_next_item` start and the fetch for live projects.
use super::*;

#[tokio::test]
async fn concurrency_caps_starts_and_claims() {
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    rig.machine.concurrency = 1;
    rig.queue(0, &["q"]);
    fake.active.store(1, Ordering::SeqCst);
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty(), "no session slot left");
    assert!(ops(&asked).is_empty(), "and no claim");
    fake.active.store(0, Ordering::SeqCst);
    rig.machine.poll().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert!(ops(&asked).is_empty(), "the one slot went to the queued item");
}

#[tokio::test]
async fn stopping_blocks_new_claims_and_starts() {
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    rig.queue(0, &["q"]);
    rig.machine.stopping_override = Some(true);
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty(), "no start while stopping");
    assert!(ops(&asked).is_empty(), "no claim_next while stopping");
}

#[tokio::test]
async fn a_do_next_item_starts_without_a_session_slot_and_takes_none() {
    let (base, asked) = claim_stub(json!([]), handed_out()).await;
    let mut rig = rig(3, &[&base]);
    let fake = rig.go_live();
    rig.machine.concurrency = 1;
    rig.queue(0, &["plain", "rush"]);
    rig.lp(0).items.get_mut("rush").unwrap().do_next = true;
    fake.active.store(1, Ordering::SeqCst);
    rig.machine.poll().await;
    let started: Vec<_> = fake.started.lock().unwrap().iter().map(|r| r.item_id.clone()).collect();
    assert_eq!(started, ["rush"], "only the do-next item skips the full slots");
    assert!(ops(&asked).is_empty(), "and nothing is claimed without a slot");
    fake.active.store(0, Ordering::SeqCst);
    rig.machine.poll().await;
    assert_eq!(
        fake.started.lock().unwrap().len(),
        2,
        "the rush start took no slot from the plain item"
    );
}

#[tokio::test]
async fn the_fetch_runs_for_live_projects_and_a_failure_is_an_error_event() {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base, &base]);
    let repos = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repo_with_origin(repos.path());
    rig.lp(0).project.repo_path = repo.to_string_lossy().into_owned();
    rig.lp(1).project.repo_path = repos.path().join("missing").to_string_lossy().into_owned();
    rig.machine.fetch().await;
    assert!(rig.lp(1).events().recent().is_empty(), "a dry project isn't fetched");
    rig.go_live();
    rig.pause("p0", true);
    rig.machine.fetch().await;
    assert!(
        rig.lp(0).events().recent().is_empty(),
        "a paused project is fetched too, and it worked"
    );
    let errors: Vec<_> = rig.lp(1).events().recent().iter().map(Entry::to_value).collect();
    assert_eq!((errors.len(), errors[0]["where"].as_str()), (1, Some("fetch")));
}
