//! What a restart does with the items a computer still holds (`readopt` in `driver.rs`).
use super::*;

fn done(result: ImplementerOutcome) -> SessionResult {
    SessionResult::Implementer(result)
}

/// The computer started again: the same state files, a new machine with its lanes free.
fn restarted(old: Rig, base: &str) -> Rig {
    let Rig { keys, live, _dir, .. } = old;
    let links = Links {
        backlog_url: "https://backlog.example".into(),
        keychain: keys.clone(),
        paused: PauseState::default(),
        machine: "my-pc".into(),
        environment: "retaskable.com".into(),
        tags: Vec::new(),
        env: Env::empty(),
        engine_choice: EngineChoiceConfig::default(),
    };
    let mut machine = Machine::new(2, 8, links);
    let store = Store::load(&_dir.path().join("0")).unwrap();
    let mut lp = ProjectLoop::new(
        "p0".into(),
        item_lifecycle_cfg(),
        store,
        EventLog::with_now(|| 0),
        Client::new(base),
        live.clone(),
    );
    lp.repo = Some(7);
    machine.add(lp);
    Rig {
        machine,
        keys,
        live,
        _dir,
    }
}

/// Claims "a" and runs two passes, the session ending with `result` (none: still running). Then the
/// computer restarts and the store lists "a" as `listed`; the next pass is returned.
async fn after_restart(result: Option<SessionResult>, listed: &str) -> (Rig, Arc<Fake>, RunRecord) {
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    fake.script.lock().unwrap().extend(result);
    rig.machine.pass().await;
    rig.machine.pass().await;
    let saved = rig.lp(0).store.runs()["a"].clone();
    let (base, _) = claim_stub(json!([item("a", listed)]), Some(Value::Null)).await;
    let mut rig = restarted(rig, &base);
    let fake = rig.go_live();
    rig.machine.pass().await;
    (rig, fake, saved)
}

#[tokio::test]
async fn a_run_is_saved_when_its_session_starts_and_updated_when_it_ends() {
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(done(ImplementerOutcome::Finished));
    rig.machine.pass().await;
    let id = fake.started.lock().unwrap()[0].session_id.clone();
    let started = rig.lp(0).store.runs()["a"].clone();
    assert_eq!(
        (started.state.as_str(), started.lane, started.impl_session_id.as_deref()),
        ("implementing", Some(1), Some(id.as_str()))
    );
    assert_eq!(started.branch.as_deref(), Some("retaskable/a-a-s0"));
    rig.machine.pass().await;
    let settled = rig.lp(0).store.runs()["a"].clone();
    assert_eq!(
        (settled.state.as_str(), settled.lane, settled.impl_session_id),
        ("checking", Some(1), Some(id))
    );
}

#[tokio::test]
async fn a_restart_resumes_an_implementing_item_in_its_saved_lane() {
    let (mut rig, fake, saved) = after_restart(None, "building").await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert_eq!(
        (started[0].resume, Some(&started[0].session_id), Some(started[0].lane)),
        (true, saved.impl_session_id.as_ref(), saved.lane)
    );
    assert_eq!(started[0].token, "lease-1");
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), saved.lane);
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
}

#[tokio::test]
async fn an_interrupted_session_is_readopted_and_resumed_by_its_id() {
    let (mut rig, fake, saved) = after_restart(Some(SessionResult::Interrupted), "building").await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert_eq!(
        (started[0].resume, Some(&started[0].session_id), Some(started[0].lane)),
        (true, saved.impl_session_id.as_ref(), saved.lane)
    );
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
}

#[tokio::test]
async fn a_resumed_session_is_not_started_twice() {
    let (mut rig, fake, _) = after_restart(None, "building").await;
    rig.machine.pass().await;
    rig.machine.poll().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert!(fake.stepped.lock().unwrap().is_empty(), "its session is its step");
}

#[tokio::test]
async fn a_restart_keeps_a_needs_human_item_without_a_lane_or_a_session() {
    let (mut rig, fake, _) = after_restart(Some(SessionResult::NeedsHuman("gave up".into())), "building").await;
    assert!(fake.started.lock().unwrap().is_empty());
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), None);
    assert_eq!(rig.lp(0).store.leases()["a"], "lease-1");
    assert_eq!(rig.lp(0).store.runs()["a"].state, "needs_human");
}

#[tokio::test]
async fn a_restart_keeps_a_checking_item_in_its_lane() {
    let (mut rig, fake, _) = after_restart(Some(done(ImplementerOutcome::Finished)), "building").await;
    assert!(fake.started.lock().unwrap().is_empty());
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Checking);
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
    assert_eq!(rig.lp(0).store.leases()["a"], "lease-1");
}

#[tokio::test]
async fn a_restart_drops_a_run_the_store_no_longer_shows_as_ours() {
    let (mut rig, fake, _) = after_restart(None, "on_main").await;
    assert!(fake.started.lock().unwrap().is_empty() && rig.lp(0).runs.is_empty());
    assert!(rig.lp(0).store.leases().is_empty() && rig.lp(0).store.runs().is_empty());
}

#[tokio::test]
async fn a_run_is_removed_when_its_lease_is_given_back() {
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    fake.script
        .lock()
        .unwrap()
        .push(SessionResult::Failed("git is gone".into()));
    rig.machine.pass().await;
    assert!(!rig.lp(0).store.runs().is_empty(), "saved while it runs");
    rig.machine.pass().await;
    assert!(rig.lp(0).store.runs().is_empty() && rig.lp(0).store.leases().is_empty());
}
