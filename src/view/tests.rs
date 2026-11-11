use super::*;

fn project(id: &str, repo: &str) -> ProjectView {
    ProjectView {
        id: id.into(),
        github_repo: repo.into(),
        backlog_path: "backlog".into(),
        repo_path: PathBuf::new(),
        worktrees_dir: PathBuf::new(),
        base_branch: "main".into(),
        play_command: String::new(),
    }
}

fn view(projects: Vec<ProjectView>) -> AgentView {
    crate::test_support::agent_view(vec!["windows".into()], projects, "1.2.3", 3)
}

#[test]
fn snapshot_reports_the_paced_number_and_reason_only_while_set() {
    let dir = tempfile::tempdir().unwrap();
    let v = view(vec![]);
    let usage = || v.snapshot("", dir.path())["state"]["usage"].clone();
    assert_eq!(usage(), json!({}));
    v.item_commands.set_auto(Some((2, "On pace".into())));
    assert_eq!(usage(), json!({"autoConcurrency": {"allowed": 2, "reason": "On pace"}}));
    v.item_commands.set_auto(None);
    assert_eq!(usage(), json!({}));
}

#[test]
fn snapshot_says_local_down_only_while_the_model_is_on_and_down_and_the_flag_is_on() {
    let dir = tempfile::tempdir().unwrap();
    let local_down = |v: &AgentView| v.snapshot("", dir.path())["state"]["usage"].get("localDown").cloned();
    let on = |up: bool, ladder: bool| {
        let local = crate::local_model::LocalModel::new(crate::config::LocalModelConfig {
            enabled: true,
            base_url: "http://localhost:1".into(),
            model: "m".into(),
        });
        crate::tiers::set_local_ladder_for_test(ladder);
        if up {
            local.mark_up_for_test();
        } else {
            local.mark_down_for_test();
        }
        let mut v = view(vec![]);
        v.local = local;
        v
    };
    assert_eq!(local_down(&on(false, true)), Some(json!(true)));
    assert_eq!(local_down(&on(true, true)), None, "up");
    assert_eq!(local_down(&on(false, false)), None, "flag Off");
    assert_eq!(local_down(&view(vec![])), None, "model off");
}

#[test]
fn pause_state_sets_replaces_and_clears_a_projects_reason() {
    let p = PauseState::default();
    assert_eq!(p.reason("a"), None);
    p.set("a", Some("lunch".into()));
    assert_eq!(p.reason("a").as_deref(), Some("lunch"));
    assert_eq!(p.reason("b"), None);
    p.set("a", Some("meeting".into()));
    assert_eq!(p.reason("a").as_deref(), Some("meeting"));
    p.set("a", None);
    assert_eq!(p.reason("a"), None);
}

#[test]
fn pause_state_notifies_subscribers_only_of_real_changes() {
    let p = PauseState::default();
    let mut rx = p.subscribe();
    p.set("a", Some("x".into()));
    assert!(rx.has_changed().unwrap());
    rx.borrow_and_update();
    p.set("a", Some("x".into()));
    assert!(!rx.has_changed().unwrap(), "same reason is no change");
    p.set("b", None);
    assert!(!rx.has_changed().unwrap(), "resuming an unpaused project is no change");
    p.set("a", None);
    assert!(rx.has_changed().unwrap());
}

#[test]
fn pause_states_compare_by_content() {
    let (a, b) = (PauseState::default(), PauseState::default());
    assert_eq!(a, b);
    a.set("p", Some("r".into()));
    assert_ne!(a, b);
    b.set("p", Some("r".into()));
    assert_eq!(a, b);
}

#[test]
fn keys_are_the_project_ids_or_the_agents_own_before_setup() {
    assert_eq!(view(vec![]).keys(), vec![String::new()]);
    assert_eq!(view(vec![project("a", "o/a"), project("b", "o/b")]).keys(), ["a", "b"]);
}

#[tokio::test]
async fn hello_carries_the_computers_settings_and_names_each_env_var_without_its_value() {
    let dir = tempfile::tempdir().unwrap();
    let env = crate::env::Env::from_pairs([("RETASKABLE_POLL_SEC", "30")]);
    let mut v = view(vec![]);
    v.settings = HelloSettings::new(
        dir.path().to_path_buf(),
        env,
        4700,
        Arc::new(crate::flags::Flags::fixed(&["machine-settings"])),
    );
    let hello = v.hello("").await;
    let s = &hello["settings"];
    assert_eq!(s["concurrency"], 1);
    assert_eq!(s["autoConcurrency"]["enabled"], true);
    assert_eq!(s["models"]["opusAbovePct"], 50);
    assert_eq!(s["port"], 4700);
    assert_eq!(s["lanes"], 2);
    assert_eq!(s["pollIntervalSec"], 30);
    assert_eq!(s["local"]["enabled"], false);
    assert_eq!(s["networkAccess"], false);
    assert!(s["engines"]["roles"].is_object() && s["cursorModels"].is_object() && s["tags"].is_array());
    assert_eq!(s["envVars"]["RETASKABLE_POLL_SEC"], true);
    assert_eq!(s["envVars"]["RETASKABLE_LANES"], false);
    assert_eq!(hello["capabilities"], json!(["agent_settings"]));
    // Off, the hello has no settings at all.
    v.settings = HelloSettings::new(
        dir.path().to_path_buf(),
        crate::env::Env::empty(),
        4700,
        Arc::new(crate::flags::Flags::fixed(&[])),
    );
    let off = v.hello("").await;
    assert!(off.get("settings").is_none());
    assert_eq!(off["capabilities"], json!([]));
    // The name is there, but the value 30 shows only as the computed pollIntervalSec, never as the variable's.
    assert!(!s["envVars"].to_string().contains("30"));
}

#[tokio::test]
async fn hello_names_the_project_or_null_before_setup_or_without_a_repo() {
    let v = view(vec![project("a", "o/a"), project("b", "")]);
    let hello = v.hello("a").await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["agentVersion"], "1.2.3");
    assert_eq!(hello["machine"], "pc");
    assert_eq!(hello["os"], platform());
    assert_eq!(hello["tags"], json!(["windows"]));
    assert_eq!(
        hello["project"],
        json!({ "githubRepo": "o/a", "backlogPath": "backlog" })
    );
    assert!(
        v.hello("b").await["project"].is_null(),
        "a project with no repo is not Setup-done"
    );
    assert!(v.hello("").await["project"].is_null());
    assert!(v.hello("unknown").await["project"].is_null());
}

fn view_with_checks_flag(on: bool) -> AgentView {
    let mut v = view(vec![project("a", "o/a")]);
    v.settings = crate::hello_settings::HelloSettings::new(
        PathBuf::new(),
        crate::env::Env::empty(),
        0,
        Arc::new(crate::flags::Flags::fixed(if on {
            &["setup-check-command"]
        } else {
            &[]
        })),
    );
    v
}

#[tokio::test]
async fn hello_has_no_checks_until_the_first_run_then_carries_them_with_node() {
    let v = view_with_checks_flag(true);
    for id in ["", "a"] {
        assert!(v.hello(id).await.get("checks").is_none());
    }
    v.checks.run_now().await;
    for id in ["", "a"] {
        let checks = &v.hello(id).await["checks"];
        assert_eq!(checks["node"]["ok"], true);
        assert!(checks["ghInstalled"].is_object());
        assert!(checks["claudeSignedIn"].is_object());
    }
}

#[tokio::test]
async fn hello_has_no_checks_while_the_setup_check_command_flag_is_off() {
    let v = view_with_checks_flag(false);
    v.checks.run_now().await;
    for id in ["", "a"] {
        assert!(v.hello(id).await.get("checks").is_none());
    }
}

#[test]
fn snapshot_before_setup_says_setup() {
    let snap = view(vec![]).snapshot("", Path::new("."));
    assert_eq!(
        snap["state"]["orchestrator"],
        json!({ "status": "setup", "machine": "pc" })
    );
    assert!(snap["state"].get("galleries").is_none());
}

#[test]
fn snapshot_reports_the_trial_plan_from_the_projects_state_file() {
    use crate::trial_plan::TrialRecord;
    let dir = tempfile::tempdir().unwrap();
    let v = view(vec![project("a", "o/a")]);
    let trial = |v: &AgentView| v.snapshot("a", dir.path())["state"]["trial"].clone();
    assert_eq!(trial(&v), json!({ "state": "not_started" }));
    let mut store = Store::load(&paths::project_dir(dir.path(), "a")).unwrap();
    store.set_trial(TrialRecord::started()).unwrap();
    assert_eq!(trial(&v), json!({ "state": "running" }));
    store.set_trial(TrialRecord::opened("https://x/pr/1")).unwrap();
    assert_eq!(trial(&v), json!({ "state": "open", "prUrl": "https://x/pr/1" }));
    store.set_trial(TrialRecord::dropped()).unwrap();
    assert_eq!(trial(&v), json!({ "state": "failed" }));
    store.set_trial(TrialRecord::skip()).unwrap();
    assert_eq!(trial(&v), json!({ "state": "not_started" }));
    assert!(v.snapshot("", dir.path())["state"].get("trial").is_none());
}

#[test]
fn snapshot_reports_the_pause_reason_or_the_default_one() {
    let v = view(vec![project("a", "o/a")]);
    let orch = |v: &AgentView| v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
    assert_eq!(orch(&v)["status"], "paused");
    assert_eq!(orch(&v)["pausedReason"], NO_WORK_YET);
    v.paused.set("a", Some("Paused from the site".into()));
    assert_eq!(orch(&v)["pausedReason"], "Paused from the site");
}

fn reporting_view(on: bool) -> AgentView {
    reporting_view_in(on, PathBuf::new())
}

fn reporting_view_in(on: bool, dir: PathBuf) -> AgentView {
    let mut v = view(vec![project("a", "o/a")]);
    v.settings = HelloSettings::new(
        dir,
        crate::env::Env::empty(),
        4700,
        Arc::new(crate::flags::Flags::fixed(if on {
            &[SNAPSHOT_REPORTING_FLAG]
        } else {
            &[]
        })),
    );
    v
}

fn lanes_of(v: &AgentView) -> Value {
    v.snapshot("a", Path::new("."))["state"]["lanes"].clone()
}

#[test]
fn snapshot_reporting_shows_each_lanes_item_branch_and_game() {
    use crate::orchestrator::live::HeldLane;
    let held = |lane, playing| HeldLane {
        lane,
        item_id: format!("i{lane}"),
        title: None,
        branch: Some(format!("retaskable/i{lane}-s1")),
        playing,
    };
    let v = reporting_view(true);
    v.live.register("a");
    v.live.ticked(
        "a",
        false,
        "t".into(),
        vec![held(1, false), held(2, true), held(5, false)],
        "d".into(),
    );
    let lanes = lanes_of(&v);
    let lanes = lanes.as_array().unwrap();
    // The view has 3 lanes; lane 5 is held past that and still listed.
    assert_eq!(
        lanes.iter().map(|l| l["k"].as_u64().unwrap()).collect::<Vec<_>>(),
        [1, 2, 3, 5]
    );
    assert_eq!(
        (&lanes[0]["itemId"], &lanes[0]["branch"], &lanes[0]["playing"]),
        (&json!("i1"), &json!("retaskable/i1-s1"), &json!(false))
    );
    assert_eq!(lanes[1]["playing"], true);
    assert!(lanes[2].get("itemId").is_none() && lanes[2]["playing"] == false);
    assert_eq!(lanes[3]["itemId"], "i5");
    // Flag Off: as before.
    let off = reporting_view(false);
    off.live.register("a");
    off.live.ticked("a", false, "t".into(), vec![held(1, true)], "d".into());
    assert_eq!(lanes_of(&off)[0], json!({ "k": 1, "diskGb": 0.0, "playing": false }));
}

#[test]
fn snapshot_reporting_lists_the_items_held_and_the_chores_milestone() {
    use crate::orchestrator::live::HeldItem;
    let held = |id: &str, chore| HeldItem {
        id: id.into(),
        title: format!("T {id}"),
        state: "building".into(),
        lane: Some(2),
        branch: format!("retaskable/{id}-s1"),
        pr_url: None,
        attempt: 1,
        chore,
    };
    let of = |v: &AgentView| {
        let s = v.snapshot("a", Path::new("."))["state"].clone();
        (s["items"].clone(), s["milestones"].clone())
    };
    let v = reporting_view(true);
    v.live.register("a");
    assert_eq!(of(&v), (json!([]), json!([])));
    v.live.set_items("a", vec![held("x", false)]);
    let (items, milestones) = of(&v);
    assert_eq!(
        (&items[0]["id"], &items[0]["state"], &items[0]["lane"]),
        (&json!("x"), &json!("building"), &json!(2))
    );
    assert_eq!(items[0]["chore"], false);
    assert_eq!(milestones, json!([]));
    v.live.set_items("a", vec![held("x", false), held("chore-y", true)]);
    assert_eq!(of(&v).1, json!([{ "id": "chores", "title": "Chores" }]));
    v.live.set_items("a", vec![]);
    assert_eq!(of(&v), (json!([]), json!([])));
    let off = reporting_view(false);
    off.live.set_items("a", vec![held("x", true)]);
    assert_eq!(of(&off), (json!([]), json!([])));
}

#[test]
fn main_playing_is_true_while_play_main_runs() {
    let root = tempfile::tempdir().unwrap();
    let mut p = project("a", "o/a");
    p.repo_path = crate::test_support::repo_with_origin(root.path());
    p.worktrees_dir = root.path().join("worktrees");
    p.play_command = if cfg!(windows) {
        "ping -n 60 127.0.0.1 >nul"
    } else {
        "sleep 60"
    }
    .into();
    let v = reporting_view(true);
    let main_playing = |v: &AgentView| v.snapshot("a", Path::new("."))["state"]["mainPlaying"].clone();
    assert_eq!(main_playing(&v), false);
    crate::play::start("a", &p).unwrap();
    assert_eq!(main_playing(&v), true);
    assert_eq!(main_playing(&reporting_view(false)), false);
    crate::play::stop("a").unwrap();
    assert_eq!(main_playing(&v), false);
}

#[test]
fn snapshot_reporting_gives_the_orchestrator_status_from_the_loop() {
    let v = reporting_view(true);
    let orch = |v: &AgentView| v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
    assert_eq!(orch(&v)["pausedReason"], NO_WORK_YET, "no loop running");
    assert_eq!(orch(&v)["status"], "paused");
    v.live.register("a");
    v.live.set_concurrency(3);
    assert_eq!(
        orch(&v),
        json!({ "status": "running", "machine": "pc", "concurrency": 3, "spendTodayUsd": 0.0, "dailyUsd": 0.0 })
    );
    v.live.set_limit(
        "a",
        Some(std::time::Instant::now() + std::time::Duration::from_secs(60)),
    );
    let o = orch(&v);
    assert_eq!(
        (&o["status"], o["resetsAt"].is_string()),
        (&json!("waiting_for_limit"), true)
    );
    v.paused.set("a", Some("Paused from the site".into()));
    let o = orch(&v);
    assert_eq!(
        (&o["status"], &o["pausedReason"]),
        (&json!("paused"), &json!("Paused from the site"))
    );
    v.paused.set("a", None);
    v.live.set_limit("a", None);
    v.item_commands.set_auto(Some((2, "On pace".into())));
    assert_eq!(
        orch(&v)["concurrency"],
        2,
        "auto concurrency's number once it has stepped"
    );
    assert_eq!(
        v.snapshot("", Path::new("."))["state"]["orchestrator"]["status"],
        "setup"
    );
}

#[test]
fn snapshot_reporting_reports_todays_spend_and_the_daily_budget() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.json"),
        r#"{ "version": 2, "budget": { "dailyUsd": 20 } }"#,
    )
    .unwrap();
    let mut machine = crate::store::MachineStore::load(dir.path()).unwrap();
    machine.add_spend("a", 1.25).unwrap();
    for on in [true, false] {
        let v = reporting_view_in(on, dir.path().to_path_buf());
        let o = v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
        assert_eq!(o.get("spendTodayUsd").is_some(), on);
        assert_eq!(o.get("dailyUsd").is_some(), on);
        if on {
            assert_eq!((&o["spendTodayUsd"], &o["dailyUsd"]), (&json!(1.25), &json!(20.0)));
        }
    }
}

#[test]
fn snapshot_reporting_adds_the_usage_windows_from_the_projects_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::load(&paths::project_dir(dir.path(), "a")).unwrap();
    let soon = crate::telemetry::now_secs() + 3600;
    store.set_usage(json!({"utilization": 0.25, "status": "allowed", "resetsAt": soon}));
    store.set_week_usage(json!({"rateLimitType": "seven_day", "utilization": 0.4, "resetsAt": soon}));
    store.save().unwrap();
    let usage = |on| reporting_view(on).snapshot("a", dir.path())["state"]["usage"].clone();
    let u = usage(true);
    assert_eq!(
        (&u["leftPct"], &u["status"], &u["weekUsedPct"]),
        (&json!(75.0), &json!("allowed"), &json!(40.0))
    );
    assert_eq!(u["tier"], "opus");
    assert!(u["resetsAt"].is_string() && u["weekResetsAt"].is_string());
    assert_eq!(usage(false), json!({}), "flag Off");
}

#[test]
fn snapshot_reporting_off_leaves_the_orchestrator_block_as_it_was() {
    let v = reporting_view(false);
    v.live.register("a");
    v.live.set_concurrency(3);
    let orch = v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
    assert_eq!(
        orch,
        json!({ "status": "paused", "machine": "pc", "pausedReason": NO_WORK_YET })
    );
}

fn open(id: &str, since_ms: i64, text: &str, details: Value) -> crate::telemetry::debounce::OpenAnomaly {
    crate::telemetry::debounce::OpenAnomaly {
        id: id.into(),
        rule: "lease_idle",
        item: Some("R-1".into()),
        text: text.into(),
        details,
        since_ms,
    }
}

#[test]
fn snapshot_sends_open_anomalies_oldest_first_with_their_project_and_drops_ended_ones() {
    let v = reporting_view(true);
    let orch = |v: &AgentView| v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
    v.live.register("a");
    assert!(orch(&v).get("anomalies").is_none(), "none open: the key is left out");
    let mut rx = v.live.subscribe();
    rx.mark_unchanged();
    v.live
        .set_anomalies("a", vec![open("x", 5_000, "stuck", json!({ "n": 1 }))]);
    assert!(rx.has_changed().unwrap(), "a change in the list signals the connector");
    assert_eq!(
        orch(&v)["anomalies"],
        json!([{ "id": "x", "kind": "lease_idle", "item": "R-1", "since": "1970-01-01T00:00:05.000Z",
                 "text": "stuck", "details": { "n": 1 }, "project": "a" }])
    );
    v.live.set_anomalies("a", vec![]);
    assert!(orch(&v).get("anomalies").is_none(), "gone the minute it ends");
}

#[test]
fn snapshot_caps_anomalies_at_twenty_and_cleans_their_text_and_details() {
    let v = reporting_view(true);
    v.live.register("a");
    let many = (0..25).map(|i| open(&format!("id{i:02}"), i, "t", json!({}))).collect();
    v.live.set_anomalies("a", many);
    let o = v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
    assert_eq!(o["anomalies"].as_array().unwrap().len(), 20);
    assert_eq!(o["anomalies"][0]["id"], "id00");
    assert_eq!(o["anomaliesMore"], 5);

    let big: serde_json::Map<String, Value> = (0..40).map(|i| (format!("k{i:02}"), json!("x".repeat(100)))).collect();
    v.live.set_anomalies(
        "a",
        vec![open(
            "s",
            0,
            &format!("password=hunter2 {}", "y".repeat(400)),
            Value::Object(big),
        )],
    );
    let o = v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
    let text = o["anomalies"][0]["text"].as_str().unwrap();
    assert!(!text.contains("hunter2") && text.chars().count() <= 300, "{text}");
    let details = &o["anomalies"][0]["details"];
    assert!(details.to_string().len() <= 1024 && details["k00"].is_string());
    assert!(o.get("anomaliesMore").is_none());
}

#[test]
fn snapshot_reporting_off_sends_no_anomalies() {
    let v = reporting_view(false);
    v.live.register("a");
    v.live.set_anomalies("a", vec![open("x", 0, "stuck", json!({}))]);
    assert!(
        v.snapshot("a", Path::new("."))["state"]["orchestrator"]
            .get("anomalies")
            .is_none()
    );
}

#[test]
fn snapshot_reports_cursor_tokens_only_when_there_are_some_and_the_flag_is_on() {
    let dir = tempfile::tempdir().unwrap();
    let mk = |on: bool| reporting_view_in(on, dir.path().to_path_buf());
    let cursor = |v: &AgentView| v.snapshot("", dir.path())["state"]["usage"].get("cursor").cloned();
    assert_eq!(cursor(&mk(true)), None, "no tokens yet");
    let mut m = crate::store::MachineStore::load(dir.path()).unwrap();
    m.add_cursor_tokens(30, 4).unwrap();
    m.add_cursor_tokens(1, 2).unwrap();
    m.stamp_cursor_limit_at().unwrap();
    let got = cursor(&mk(true)).unwrap();
    assert_eq!(
        (&got["todayTokens"], &got["weekInputTokens"], &got["weekOutputTokens"]),
        (&json!(37), &json!(31), &json!(6))
    );
    assert!(got["limitHitAt"].is_string());
    assert_eq!(cursor(&mk(false)), None, "flag Off");
}

#[test]
fn snapshot_lists_every_lane_with_its_size_and_notes_only_the_chore_lane() {
    let v = view(vec![project("a", "o/a")]);
    v.lane_sizes.set("a", [(2, 1.5)].into_iter().collect());
    let lanes = v.snapshot("a", Path::new("."))["state"]["lanes"].clone();
    assert_eq!(
        lanes,
        json!([
            { "k": 1, "diskGb": 0.0, "playing": false },
            { "k": 2, "diskGb": 1.5, "playing": false },
            { "k": 3, "diskGb": 0.0, "playing": false, "note": CHORE_LANE_NOTE },
        ])
    );
}

#[test]
fn project_view_derives_its_id_from_the_config() {
    let cfg = ProjectConfig {
        id: Some("custom".into()),
        ..ProjectConfig::default()
    };
    assert_eq!(ProjectView::from(&cfg).id, "custom");
}

#[test]
fn snapshot_carries_commands_waiting_only_while_one_waits_and_the_flag_is_on() {
    use crate::backlog::settings::StoreSettings;
    let dir = tempfile::tempdir().unwrap();
    let v = view(vec![]);
    let waiting = || v.snapshot("p", dir.path())["state"].get("commandsWaiting").cloned();
    let store = |version: u64, play: &str| -> StoreSettings {
        serde_json::from_value(json!({
            "version": version, "commandsVersion": version, "updatedBy": "ann",
            "values": { "playCommand": play },
        }))
        .unwrap()
    };
    let file = crate::config::ProjectConfig::default();
    let gates = v.item_commands.gates();
    gates.open(dir.path(), "p");
    gates.consider("p", Some(&store(1, "run it")), &file).unwrap();
    assert_eq!(waiting(), None, "flag Off");
    v.item_commands.set_flag("p", crate::command_gate::FLAG, true);
    let w = waiting().expect("waiting");
    assert_eq!((w["version"].clone(), w["by"].clone()), (json!(1), json!("ann")));
    assert!(w["text"].as_str().unwrap().contains("run it"), "{w}");
    assert!(w.get("values").is_none());
    gates.approve("p", 1).unwrap();
    assert_eq!(waiting(), None, "approved");
}

fn pause_block(live: bool, obey: bool, store: StorePause, computer: bool) -> Value {
    let v = reporting_view(true);
    if live {
        v.live.register("a");
    }
    v.item_commands.set_flag("a", OBEY_SHARED_PAUSE_FLAG, obey);
    v.store_paused.set("a", store);
    v.paused
        .set("a", computer.then(|| "Paused from retaskable.com by sam".to_string()));
    v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone()
}

#[test]
fn obey_shared_pause_reports_which_pause_is_on_in_both_blocks() {
    let words = || StorePause::Paused("Paused by @james for everyone".into());
    for live in [true, false] {
        let reason = |b: Value| b["pausedReason"].clone();
        assert_eq!(
            reason(pause_block(live, true, words(), false)),
            "Paused by @james for everyone"
        );
        assert_eq!(
            reason(pause_block(live, true, StorePause::NotPaused, true)),
            "This computer is paused"
        );
        assert_eq!(
            reason(pause_block(live, true, words(), true)),
            "Paused by @james for everyone"
        );
        assert_eq!(
            reason(pause_block(live, true, StorePause::Unknown, true)),
            "This computer is paused"
        );
        let neither = pause_block(live, true, StorePause::NotPaused, false);
        let unknown = pause_block(live, true, StorePause::Unknown, false);
        assert_eq!(neither, unknown, "an unknown store pause alone isn't paused");
        if live {
            assert_eq!(neither["status"], "running");
        } else {
            assert_eq!(reason(neither), NO_WORK_YET);
        }
        // Off: the old output, the store's pause ignored and this computer's own reason kept.
        assert_eq!(
            reason(pause_block(live, false, words(), true)),
            "Paused from retaskable.com by sam"
        );
        assert_eq!(
            pause_block(live, false, words(), false).get("pausedReason").is_some(),
            !live
        );
    }
}

fn item_play(project: &str, lane: u32) -> ItemCommand {
    ItemCommand::Play {
        project: project.into(),
        lane,
    }
}

fn item_skip(project: &str, item: &str) -> ItemCommand {
    ItemCommand::Skip {
        project: project.into(),
        item: item.into(),
    }
}

#[test]
fn take_and_take_lane_split_a_projects_queue_and_leave_other_projects_alone() {
    let q = ItemCommands::default();
    q.push(item_skip("one", "a"));
    q.push(item_play("one", 2));
    q.push(item_skip("two", "x"));
    q.push(ItemCommand::Stop {
        project: "one".into(),
        lane: 2,
    });
    q.push(item_skip("one", "b"));
    assert_eq!(q.take("one"), vec![item_skip("one", "a"), item_skip("one", "b")]);
    assert!(q.take("one").is_empty());
    assert_eq!(
        q.take_lane("one"),
        vec![
            item_play("one", 2),
            ItemCommand::Stop {
                project: "one".into(),
                lane: 2
            }
        ]
    );
    assert_eq!(q.take("two"), vec![item_skip("two", "x")]);
    assert!(q.take_lane("two").is_empty());
}

#[test]
fn take_drafts_ideas_and_feedback_return_only_that_projects_in_order() {
    let q = ItemCommands::default();
    let draft = |p: &str, d: &str| DraftCommand::Discard {
        project: p.into(),
        draft: d.into(),
    };
    q.push_draft(draft("one", "1"));
    q.push_draft(draft("two", "2"));
    q.push_draft(DraftCommand::Merge {
        project: "one".into(),
        draft: "3".into(),
    });
    assert_eq!(
        q.take_drafts("one"),
        vec![
            draft("one", "1"),
            DraftCommand::Merge {
                project: "one".into(),
                draft: "3".into()
            }
        ]
    );
    assert_eq!(q.take_drafts("two"), vec![draft("two", "2")]);

    let idea = |p: &str, id: &str| IdeaCommand {
        project: p.into(),
        command_id: id.into(),
        idea: "i".into(),
        by: "me".into(),
    };
    q.push_idea(idea("one", "a"));
    q.push_idea(idea("two", "b"));
    q.push_idea(idea("one", "c"));
    let ids: Vec<_> = q.take_ideas("one").into_iter().map(|i| i.command_id).collect();
    assert_eq!(ids, ["a", "c"]);
    assert_eq!(q.take_ideas("two").len(), 1);
}

#[test]
fn flags_are_per_project_and_per_flag_and_can_be_turned_off() {
    let q = ItemCommands::default();
    assert!(!q.flag_on("one", "some-flag"));
    q.set_flag("one", "some-flag", true);
    assert!(q.flag_on("one", "some-flag"));
    assert!(!q.flag_on("two", "some-flag"));
    assert!(!q.flag_on("one", "other-flag"));
    assert!(!q.ask_on("one"));
    q.set_ask_on("one", true);
    assert!(q.ask_on("one"));
    q.set_flag("one", "some-flag", false);
    assert!(!q.flag_on("one", "some-flag"));
    assert!(q.ask_on("one"));
}

#[test]
fn answer_goes_up_the_projects_connection_and_is_dropped_once_it_is_gone() {
    let q = ItemCommands::default();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    q.answer("one", json!({"n": 0})); // no connection yet: dropped, no panic
    q.set_out("one", Some(tx));
    q.answer("one", json!({"n": 1}));
    q.answer("two", json!({"n": 2}));
    assert_eq!(rx.try_recv().unwrap(), json!({"n": 1}));
    assert!(rx.try_recv().is_err());
    q.set_out("one", None);
    q.answer("one", json!({"n": 3}));
    assert!(rx.try_recv().is_err());
}
