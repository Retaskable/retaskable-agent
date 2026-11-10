//! What a restart does with the items a computer still holds (`readopt` in `driver.rs`).
use super::*;
use crate::engines::EngineId;

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
        store_paused: Default::default(),
        item_commands: Default::default(),
        work: Default::default(),
        machine: "my-pc".into(),
        environment: "retaskable.com".into(),
        tags: Default::default(),
        env: Env::empty(),
        engine_choice: EngineChoiceConfig::default(),
        cursor_models: Default::default(),
        checks: Default::default(),
        bugs: Default::default(),
        bug_version: "test".into(),
    };
    let mut machine = Machine::new(2, 8, links);
    machine.base_scope = "restart:".into();
    let store = Store::load(&_dir.path().join("0")).unwrap();
    let mut lp = ProjectLoop::new(
        "p0".into(),
        item_lifecycle_cfg(),
        store,
        EventLog::with_now(|| 0),
        Client::new(base),
        live.clone(),
        150_000,
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
    after_restart_sized(result, listed, None).await
}

/// `after_restart`, the saved run first noting its session had grown to `tokens`.
async fn after_restart_sized(
    result: Option<SessionResult>,
    listed: &str,
    tokens: Option<u64>,
) -> (Rig, Arc<Fake>, RunRecord) {
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    fake.script.lock().unwrap().extend(result);
    rig.machine.pass().await;
    rig.machine.pass().await;
    if tokens.is_some() {
        let store = &mut rig.lp(0).store;
        let mut runs = store.runs();
        runs.get_mut("a").unwrap().impl_context_tokens = tokens;
        store.set_runs(&runs);
        store.save().unwrap();
    }
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

#[tokio::test]
async fn a_restart_starts_a_session_over_the_size_limit_fresh_in_its_lane() {
    let (mut rig, fake, saved) = after_restart_sized(None, "building", Some(150_001)).await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert!(!started[0].resume);
    assert_ne!(Some(&started[0].session_id), saved.impl_session_id.as_ref());
    let prompt = started[0].follow_up.as_deref().unwrap();
    assert!(prompt.starts_with("You're picking up"), "{prompt}");
    assert!(prompt.ends_with("Carry on with the slice."));
    let noted = (rig.lp(0).events().recent().iter())
        .any(|e| e.to_value()["note"] == "Its session had grown to 150001 tokens; starting a fresh one");
    assert!(noted);
    assert_eq!(Some(started[0].lane), saved.lane);
}

#[tokio::test]
async fn a_restart_resumes_a_session_at_the_size_limit_or_of_unknown_size() {
    for tokens in [Some(150_000), None] {
        let (_rig, fake, saved) = after_restart_sized(None, "building", tokens).await;
        let started = fake.started.lock().unwrap().clone();
        assert_eq!(started.len(), 1);
        assert!(started[0].resume);
        assert_eq!(Some(&started[0].session_id), saved.impl_session_id.as_ref());
        assert_eq!(started[0].follow_up, None);
    }
}

#[tokio::test]
async fn a_resume_keeps_the_saved_size_and_a_fresh_start_clears_it() {
    let (mut rig, _fake, _) = after_restart_sized(None, "building", Some(100_000)).await;
    assert_eq!(rig.lp(0).store.runs()["a"].impl_context_tokens, Some(100_000));
    let (mut rig, _fake, _) = after_restart_sized(None, "building", Some(150_001)).await;
    assert_eq!(rig.lp(0).store.runs()["a"].impl_context_tokens, None);
}

#[tokio::test]
async fn a_restart_starts_a_session_fresh_when_its_lane_moved_to_another_engine() {
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    rig.machine.pass().await;
    rig.machine.pass().await;
    let saved = rig.lp(0).store.runs()["a"].clone();
    assert_eq!(saved.impl_session_engine, EngineId::Claude);
    let (base, _) = claim_stub(json!([item("a", "building")]), Some(Value::Null)).await;
    let mut rig = restarted(rig, &base);
    rig.machine.links.engine_choice = serde_json::from_value(json!({ "lanes": ["cursor", "cursor"] })).unwrap();
    let fake_after = rig.go_live();
    rig.machine.pass().await;
    drop(fake);
    let started = fake_after.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert!(!started[0].resume);
    assert_eq!(started[0].engine, EngineId::Cursor);
    assert!(
        started[0]
            .follow_up
            .as_deref()
            .unwrap()
            .starts_with("You're picking up")
    );
    let noted = (rig.lp(0).events().recent().iter()).any(|e| {
        e.to_value()["note"] == "Moved to a lane on cursor, so this carries on in a fresh cursor session on the branch"
    });
    assert!(noted);
    assert_eq!(rig.lp(0).store.runs()["a"].impl_session_engine, EngineId::Cursor);
}

#[tokio::test]
async fn a_restart_starts_a_session_fresh_on_another_engine_when_its_engine_is_at_its_limit() {
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    rig.machine.pass().await;
    rig.machine.pass().await;
    let (base, _) = claim_stub(json!([item("a", "building")]), Some(Value::Null)).await;
    let mut rig = restarted(rig, &base);
    // Lane 1 stays on Claude; lane 2 runs Cursor, so Cursor is an engine the implementer may use here.
    rig.machine.links.engine_choice = serde_json::from_value(json!({ "lanes": ["claude", "cursor"] })).unwrap();
    let limited_until = std::time::Instant::now() + Duration::from_secs(600);
    rig.machine.engine_limits.hit(EngineId::Claude, limited_until);
    let fake_after = rig.go_live();
    rig.machine.pass().await;
    drop(fake);
    let started = fake_after.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert!(!started[0].resume);
    assert_eq!(started[0].engine, EngineId::Cursor);
    assert!(
        started[0]
            .follow_up
            .as_deref()
            .unwrap()
            .starts_with("You're picking up")
    );
    let noted = (rig.lp(0).events().recent().iter()).any(|e| {
        e.to_value()["note"]
            == "claude is at its usage limit, so this carries on in a fresh cursor session on the branch"
    });
    assert!(noted);
    assert_eq!(rig.lp(0).store.runs()["a"].impl_session_engine, EngineId::Cursor);
}

#[tokio::test]
async fn a_restart_resumes_when_its_engine_is_limited_but_nothing_else_can_take_it() {
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    rig.machine.pass().await;
    rig.machine.pass().await;
    let (base, _) = claim_stub(json!([item("a", "building")]), Some(Value::Null)).await;
    let mut rig = restarted(rig, &base);
    let limited_until = std::time::Instant::now() + Duration::from_secs(600);
    rig.machine.engine_limits.hit(EngineId::Claude, limited_until);
    let fake_after = rig.go_live();
    rig.machine.pass().await;
    drop(fake);
    let started = fake_after.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert!(started[0].resume);
}

#[tokio::test]
async fn with_the_engine_start_rules_on_an_item_taking_back_its_lane_waits_while_its_engine_is_out() {
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    rig.machine.pass().await;
    rig.machine.pass().await;
    let (base, _) = claim_stub(json!([item("a", "building")]), Some(Value::Null)).await;
    let mut rig = restarted(rig, &base);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["engine-start-rules"]));
    rig.machine
        .engine_limits
        .hit(EngineId::Claude, std::time::Instant::now() + Duration::from_secs(600));
    let fake_after = rig.go_live();
    rig.machine.pass().await;
    drop(fake);
    let started = fake_after.started.lock().unwrap().clone();
    assert!(started.is_empty());
    assert!(rig.machine.pool.lane_of("p0", "a").is_some());
}

/// A claimed "a" on lanes of `engines`, the computer restarted with `engine-start-rules` On and Claude signed out.
async fn restarted_claude_out(engines: serde_json::Value) -> (Rig, Arc<Fake>) {
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    rig.machine.links.engine_choice = serde_json::from_value(json!({ "lanes": engines })).unwrap();
    rig.go_live();
    rig.machine.pass().await;
    rig.machine.pass().await;
    let (base, _) = claim_stub(json!([item("a", "building")]), Some(Value::Null)).await;
    let mut rig = restarted(rig, &base);
    rig.machine.links.engine_choice = serde_json::from_value(json!({ "lanes": engines })).unwrap();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["engine-start-rules"]));
    rig.machine.claude_signed_out_until = Some(std::time::Instant::now() + Duration::from_secs(600));
    let fake = rig.go_live();
    rig.machine.pass().await;
    (rig, fake)
}

#[tokio::test]
async fn with_claude_signed_out_and_the_start_rules_on_a_cursor_session_resumes() {
    let (_rig, fake) = restarted_claude_out(json!(["cursor", "cursor"])).await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert!(started[0].resume);
    assert_eq!(started[0].engine, EngineId::Cursor);
}

#[tokio::test]
async fn with_claude_signed_out_and_the_start_rules_on_a_claude_session_waits_then_resumes() {
    let (mut rig, fake) = restarted_claude_out(json!(["claude", "claude"])).await;
    assert!(fake.started.lock().unwrap().is_empty());
    rig.machine.claude_signed_out_until = None;
    rig.machine.pass().await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert!(started[0].resume);
}

/// A held item saved at `needs_human` for `reason`, the computer restarted with `flags` on and the store
/// listing "a" with `pr_url`; the first pass is returned.
async fn held_needs_human(reason: &str, flags: &[&str], pr_url: Option<&str>) -> (Rig, Arc<Fake>, Asked) {
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let _fake = rig.go_live();
    rig.machine.pass().await;
    let store = &mut rig.lp(0).store;
    let mut runs = store.runs();
    let record = runs.get_mut("a").unwrap();
    record.state = "needs_human".into();
    record.needs_human_reason = Some(reason.into());
    store.set_runs(&runs);
    store.save().unwrap();
    let mut listed = item("a", "needs_person");
    if let Some(url) = pr_url {
        listed["prUrl"] = json!(url);
    }
    let (base, asked) = claim_stub(json!([listed]), Some(Value::Null)).await;
    let mut rig = restarted(rig, &base);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(flags));
    let fake = rig.go_live();
    rig.machine.pass().await;
    (rig, fake, asked)
}

const LIMIT: &str = crate::orchestrator::driver::settle::CURSOR_LIMIT_STOP;

#[tokio::test]
async fn a_restart_gives_back_an_item_held_only_for_cursors_limit() {
    let (mut rig, _fake, asked) = held_needs_human(LIMIT, &["cursor-give-back"], None).await;
    let sent = ops(&asked);
    let at = sent.iter().position(|o| o == "report").unwrap();
    assert_eq!(sent[at + 1], "release");
    let note = asked.lock().unwrap().iter().find(|(o, _)| o == "report").unwrap().1["note"].clone();
    let machine = rig.machine.links.machine.clone();
    assert_eq!(
        note,
        format!("Given back: Cursor's usage limit was hit on {machine} before anything was pushed")
    );
    assert!(!rig.lp(0).runs.contains_key("a"));
    assert!(!rig.lp(0).store.runs().contains_key("a"));
    assert!(rig.lp(0).unreleased.is_empty());
    // A later pass gives nothing back again.
    let before = ops(&asked).iter().filter(|o| *o == "release").count();
    rig.machine.pass().await;
    assert_eq!(ops(&asked).iter().filter(|o| *o == "release").count(), before);
}

#[tokio::test]
async fn a_restart_readopts_the_other_needs_human_items() {
    for (reason, flags, pr) in [
        (LIMIT, &["cursor-give-back"][..], Some("https://github.com/o/p/pull/3")),
        ("Review escalated", &["cursor-give-back"][..], None),
        (LIMIT, &[][..], None),
    ] {
        let (mut rig, _fake, asked) = held_needs_human(reason, flags, pr).await;
        assert!(!ops(&asked).iter().any(|o| o == "release"));
        assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    }
}

/// "a" restarted with Claude limited, the model `local` and `local-limit-fallback` as given;
/// takeover exists. Its first transcript line names `model`. Returns what the pass started.
async fn restarted_limited(
    model: &str,
    fallback: bool,
    local: crate::local_model::LocalModel,
    rules: bool,
) -> Vec<StartRequest> {
    crate::tiers::set_local_ladder_for_test(true);
    crate::tiers::set_local_limit_fallback_for_test(fallback);
    let (base, _) = claim_stub(json!([item("a", "claimed")]), handed_out()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    rig.machine.pass().await;
    rig.machine.pass().await;
    let session = fake.started.lock().unwrap()[0].session_id.clone();
    let (base, _) = claim_stub(json!([item("a", "building")]), Some(Value::Null)).await;
    let mut rig = restarted(rig, &base);
    let dir = rig._dir.path().to_path_buf();
    let runs = crate::paths::run_dir(&dir, "p0", "a");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(
        runs.join(format!("{session}.jsonl")),
        format!("{{\"engine\":\"claude\",\"model\":\"{model}\"}}\n"),
    )
    .unwrap();
    rig.lp(0).data_dir = Some(dir);
    if rules {
        rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["engine-start-rules", "engine-fall-over"]));
    }
    rig.machine.local = local;
    rig.machine.links.engine_choice = serde_json::from_value(json!({ "lanes": ["claude", "claude"] })).unwrap();
    let limited_until = std::time::Instant::now() + Duration::from_secs(600);
    rig.machine.engine_limits.hit(EngineId::Claude, limited_until);
    let fake_after = rig.go_live();
    rig.machine.pass().await;
    fake_after.started.lock().unwrap().clone()
}

#[tokio::test]
async fn at_the_limit_a_local_session_resumes_and_an_anthropic_one_waits() {
    use crate::local_model::LocalModel;
    let local = restarted_limited("local", true, LocalModel::up(), true).await;
    assert_eq!(local.len(), 1);
    assert!(local[0].resume);
    assert_eq!(local[0].engine, EngineId::Claude);
    assert_eq!(local[0].takeover, None);
    // An Anthropic session is held until the reset, and never moves to the local model.
    assert!(
        restarted_limited("sonnet", true, LocalModel::up(), true)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn with_the_flag_off_or_the_model_down_a_local_session_is_held_like_any_claude_one() {
    use crate::local_model::LocalModel;
    for (fallback, local) in [(false, LocalModel::up()), (true, LocalModel::off())] {
        assert!(restarted_limited("local", fallback, local, true).await.is_empty());
    }
}
