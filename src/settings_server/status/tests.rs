use super::*;
use crate::config::{LaneEngine, RoleEngines};
use crate::engines::EngineId;
use crate::engines::choose::Role;
use crate::orchestrator::live::sessions::SessionInfo;
use crate::orchestrator::live::{HeldLane, ProjectLive, StoreFailure};

fn check(ok: Option<bool>, detail: &str) -> SetupCheck {
    SetupCheck {
        ok,
        detail: detail.to_string(),
        fix: None,
    }
}

fn checks(claude_signed_in: Option<bool>) -> SetupChecks {
    SetupChecks {
        git: check(Some(true), "git"),
        gh_installed: check(Some(true), "gh"),
        gh: check(Some(true), "gh"),
        claude: check(Some(true), "Claude Code 2.1"),
        claude_signed_in: check(claude_signed_in, "signed in"),
        cursor: check(Some(false), "Cursor's agent isn't installed"),
        pi: None,
        codex: Some(check(Some(true), "codex-cli 0.5.0")),
        docker: None,
    }
}

fn source(projects: &[&str]) -> StatusSource {
    StatusSource {
        view: crate::test_support::agent_view(
            vec![],
            projects
                .iter()
                .map(|id| crate::test_support::project_view(id))
                .collect(),
            "1.2.3",
            3,
        ),
        started: Instant::now(),
        checks: Arc::new(Mutex::new(None)),
        live: LiveHandle::default(),
    }
}

fn settled(choice: EngineChoiceConfig, lanes: u32) -> Settled {
    Settled {
        choice,
        local: LocalModelConfig::default(),
        lanes,
        codex_tokens: Some(42),
        cursor_tokens: None,
        live_sessions: false,
    }
}

fn row<'a>(s: &'a ComputerStatus, id: &str) -> &'a EngineRow {
    s.engines.iter().find(|e| e.id == id).unwrap()
}

#[test]
fn lanes_are_free_with_their_own_engine_and_the_chore_lane_note() {
    let choice = EngineChoiceConfig {
        lanes: vec![LaneEngine::Codex, LaneEngine::Default],
        ..EngineChoiceConfig::default()
    };
    let s = build(&settled(choice, 3), &source(&["a"]), None, &LiveState::default());
    assert_eq!(s.header.lanes_total, 3);
    assert_eq!(s.header.lanes_busy, 0);
    assert_eq!((s.lanes[0].engine.as_str(), s.lanes[0].own), ("codex", true));
    assert_eq!((s.lanes[1].engine.as_str(), s.lanes[1].own), ("claude", false));
    assert_eq!(s.lanes[1].note, "Nothing on it");
    assert!(s.lanes[2].note.starts_with("chore lane"));
    assert!(s.lanes.iter().all(|l| l.state == "free" && l.state_text == "Free"));
    assert!(s.sessions.is_empty());
}

#[test]
fn engine_rows_say_what_each_is_used_for_and_what_was_found() {
    let choice = EngineChoiceConfig {
        roles: RoleEngines {
            review: Some(Engine::Codex),
            ..RoleEngines::default()
        },
        lanes: vec![LaneEngine::Default, LaneEngine::Codex],
        ..EngineChoiceConfig::default()
    };
    let c = checks(Some(true));
    let s = build(&settled(choice, 3), &source(&["a"]), Some(&c), &LiveState::default());
    let ids: Vec<&str> = s.engines.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["claude", "cursor", "pi", "codex", "local"]);
    let codex = row(&s, "codex");
    assert_eq!(codex.used_for, vec!["Review", "Lane 2"]);
    assert_eq!(
        (codex.in_use, codex.found, codex.state_text),
        (true, Some(true), "Ready")
    );
    assert_eq!(codex.version.as_deref(), Some("codex-cli 0.5.0"));
    assert_eq!(codex.today_tokens, Some(42));
    assert_eq!(codex.limit.as_ref().unwrap().label, "Not reported");
    let claude = row(&s, "claude");
    assert_eq!(claude.state_text, "Signed in");
    assert_eq!(
        claude.used_for,
        vec!["Planning", "Building", "Drafting specs", "Chat", "Writing"]
    );
    // Not installed and not used: off. pi was not found.
    assert_eq!(
        (row(&s, "cursor").state, row(&s, "cursor").state_text),
        ("off", "Not installed")
    );
    assert_eq!(row(&s, "pi").found, Some(false));
    assert_eq!(row(&s, "local").state_text, "Off");
    assert!(row(&s, "local").limit.is_none());
}

#[test]
fn before_the_checks_finish_found_is_not_known() {
    let s = build(
        &settled(EngineChoiceConfig::default(), 1),
        &source(&[]),
        None,
        &LiveState::default(),
    );
    assert_eq!(row(&s, "claude").found, None);
    assert_eq!(row(&s, "codex").found, None);
    assert_eq!(s.header.state, "running");
    assert_eq!(s.header.projects, 0);
}

#[test]
fn signed_out_claude_and_paused_projects_are_alerts() {
    let src = source(&["a", "b"]);
    src.view.paused.set("a", Some("Paused from the website".into()));
    let c = checks(Some(false));
    let s = build(
        &settled(EngineChoiceConfig::default(), 1),
        &src,
        Some(&c),
        &LiveState::default(),
    );
    let titles: Vec<&str> = s.alerts.iter().map(|a| a.title.as_str()).collect();
    assert_eq!(titles, ["Claude Code is signed out", "o/a is paused"]);
    assert_eq!(s.alerts[0].tone, "bad");
    assert_eq!(row(&s, "claude").state_text, "Signed out");
    assert_eq!(s.header.state, "running");
    src.view.paused.set("b", Some("later".into()));
    let s = build(
        &settled(EngineChoiceConfig::default(), 1),
        &src,
        None,
        &LiveState::default(),
    );
    assert_eq!((s.header.state, s.header.state_text), ("paused", "Paused"));
}

#[test]
fn the_status_serialises_with_the_typescript_agents_field_names() {
    let s = build(
        &settled(EngineChoiceConfig::default(), 1),
        &source(&["a"]),
        None,
        &LiveState::default(),
    );
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["header"]["stateText"], "Running");
    assert_eq!(v["header"]["lanesTotal"], 1);
    assert_eq!(v["lanes"][0]["stateText"], "Free");
    assert_eq!(v["engines"][0]["usedFor"][0], "Planning");
    assert_eq!(v["engines"][0]["limit"]["label"], "Not reported");
    assert!(v["engines"][0]["limit"]["usedPct"].is_null());
    assert!(v["engines"][0]["signedIn"].is_null());
    assert!(v["engines"][1].get("signedIn").is_none());
}

fn live(mode: OrchestratorMode, projects: &[(&str, ProjectLive)]) -> LiveState {
    LiveState {
        mode,
        run_kinds: Vec::new(),
        projects: projects.iter().map(|(id, p)| (id.to_string(), p.clone())).collect(),
        ci: None,
        concurrency: 0,
    }
}

#[test]
fn the_ci_header_carries_slots_running_docker_and_discovery_reasons_only_when_the_loop_runs() {
    let mut l = LiveState::default();
    assert!(serde_json::to_value(status_with(&l)).unwrap().get("ci").is_none());
    l.ci = Some(crate::agent_ci::CiLive {
        slots: 1,
        docker: "stopped",
        docker_reason: Some("Docker isn't running".into()),
        reasons: [("a".to_string(), "No .agent-ci/image on origin/main".to_string())].into(),
        ..Default::default()
    });
    let v = serde_json::to_value(status_with(&l)).unwrap();
    assert_eq!(
        v["ci"],
        serde_json::json!({
            "slots": 1, "running": 0, "docker": "stopped", "dockerReason": "Docker isn't running",
            "discovery": [{ "project": "a", "reason": "No .agent-ci/image on origin/main" }]
        })
    );
}

fn status_with(live: &LiveState) -> ComputerStatus {
    build(&settled(EngineChoiceConfig::default(), 3), &source(&["a"]), None, live)
}

#[test]
fn off_mode_looks_as_before_with_a_quiet_note() {
    let s = status_with(&LiveState::default());
    assert!(s.alerts.is_empty(), "off is not an error");
    assert_eq!(s.header.lanes_busy, 0);
    assert!(s.lanes.iter().all(|l| l.state == "free" && l.item.is_none()));
    assert_eq!(
        (s.orchestrator.mode, s.orchestrator.note.as_str()),
        ("off", "Orchestrator off")
    );
    assert!(s.orchestrator.projects.is_empty());
}

#[test]
fn dry_mode_says_it_claims_nothing_and_a_store_error_is_an_alert() {
    let failing = ProjectLive {
        store_error: Some(StoreFailure {
            since: "2026-10-29T14:03:05Z".into(),
            message: "connection refused".into(),
        }),
        ..ProjectLive::default()
    };
    let s = status_with(&live(OrchestratorMode::Dry, &[("a", failing)]));
    assert!(s.orchestrator.note.contains("claims nothing"));
    assert_eq!(s.alerts.len(), 1);
    assert_eq!(s.alerts[0].tone, "warn");
    assert_eq!(
        s.alerts[0].title,
        "Can't reach the backlog for a since 2026-10-29 14:03 UTC"
    );
    assert!(s.alerts[0].detail.contains("connection refused"));
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["orchestrator"]["mode"], "dry");
    assert_eq!(v["orchestrator"]["projects"][0]["project"], "a");
}

#[test]
fn a_held_lane_is_busy_with_its_holder() {
    let ticking = ProjectLive {
        last_tick: Some("2026-10-29T14:04:00Z".into()),
        held: vec![
            HeldLane {
                lane: 2,
                item_id: "i1".into(),
                title: Some("Add a thing".into()),
                branch: None,
                playing: false,
            },
            HeldLane {
                lane: 9,
                item_id: "gone".into(),
                title: None,
                branch: None,
                playing: false,
            },
        ],
        decision: Some("would ask for work".into()),
        ..ProjectLive::default()
    };
    let s = status_with(&live(OrchestratorMode::Dry, &[("a", ticking)]));
    assert_eq!(s.header.lanes_busy, 1, "a lane past the count is ignored");
    let lane = &s.lanes[1];
    assert_eq!((lane.state, lane.state_text), ("busy", "Busy"));
    let item = lane.item.as_ref().unwrap();
    assert_eq!(
        (item.id.as_str(), item.title.as_str(), item.project.as_str()),
        ("i1", "Add a thing", "a")
    );
    assert_eq!(s.lanes[0].state, "free");
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["lanes"][1]["item"]["title"], "Add a thing");
    assert!(v["lanes"][0].get("item").is_none());
    assert_eq!(v["orchestrator"]["projects"][0]["decision"], "would ask for work");
}

#[test]
fn a_paused_project_is_paused_in_the_alerts_and_the_orchestrator_block() {
    let src = source(&["a"]);
    src.view.paused.set("a", Some("Paused from the website".into()));
    let paused = ProjectLive {
        paused: true,
        decision: Some("paused: would do nothing".into()),
        ..ProjectLive::default()
    };
    let s = build(
        &settled(EngineChoiceConfig::default(), 3),
        &src,
        None,
        &live(OrchestratorMode::Dry, &[("a", paused)]),
    );
    assert_eq!(s.header.state, "paused");
    assert_eq!(s.alerts.len(), 1);
    assert!(s.alerts[0].title.ends_with("is paused"));
    assert!(s.orchestrator.projects[0].paused);
    assert_eq!(s.header.lanes_busy, 0);
}

#[test]
fn runs_mode_says_it_runs_on_runs_and_which_kinds() {
    let mut l = live(OrchestratorMode::Runs, &[]);
    let s = status_with(&l);
    assert_eq!(s.orchestrator.mode, "runs");
    assert!(s.orchestrator.note.contains("running on runs"));
    assert!(s.orchestrator.note.contains("no kinds"), "{}", s.orchestrator.note);
    l.run_kinds = vec!["build".into(), "watch".into()];
    let s = status_with(&l);
    assert_eq!(
        s.orchestrator.note,
        "Orchestrator running on runs: it can take build, watch runs"
    );
}

#[test]
fn the_chore_count_shows_only_once_the_store_has_answered_one() {
    let none = status_with(&live(OrchestratorMode::Runs, &[("a", ProjectLive::default())]));
    assert_eq!(none.orchestrator.projects[0].chores, None);
    let counted = ProjectLive {
        chores: Some(crate::backlog::runs::ChoreCount { today: 2, limit: 3 }),
        ..Default::default()
    };
    let s = status_with(&live(OrchestratorMode::Runs, &[("a", counted)]));
    assert_eq!(
        s.orchestrator.projects[0].chores.as_deref(),
        Some("Chores today: 2 of 3")
    );
}

#[test]
fn the_fix_main_note_is_reported_with_the_computers_state() {
    let found = ProjectLive {
        fix_main: Some("Fixing main found nothing to fix at abc1234. It tries again when main moves.".into()),
        ..Default::default()
    };
    let s = status_with(&live(OrchestratorMode::Runs, &[("a", found)]));
    assert_eq!(
        s.orchestrator.projects[0].fix_main.as_deref(),
        Some("Fixing main found nothing to fix at abc1234. It tries again when main moves.")
    );
}

fn vet_of(counts: &[(&str, usize)]) -> Option<String> {
    let p = ProjectLive {
        vet: Some(counts.iter().map(|(k, n)| (k.to_string(), *n)).collect()),
        ..Default::default()
    };
    status_with(&live(OrchestratorMode::Runs, &[("a", p)]))
        .orchestrator
        .projects[0]
        .vet
        .clone()
}

#[test]
fn the_vet_line_names_each_verdict_leaves_out_zeros_and_is_absent_before_a_run() {
    assert_eq!(
        vet_of(&[("promising", 2), ("duplicate", 1), ("not_promising", 9)]).as_deref(),
        Some("12 suggestions: 2 promising, 1 duplicate, 9 not promising")
    );
    assert_eq!(
        vet_of(&[("promising", 0), ("duplicate", 2), ("spam", 3)]).as_deref(),
        Some("5 suggestions: 2 duplicates, 3 spam")
    );
    assert_eq!(vet_of(&[("spam", 1)]).as_deref(), Some("1 suggestion: 1 spam"));
    let none = status_with(&live(OrchestratorMode::Runs, &[("a", ProjectLive::default())]));
    assert_eq!(none.orchestrator.projects[0].vet, None);
}

fn info(item: &str, role: Role, lane: Option<u32>) -> SessionInfo {
    SessionInfo {
        project: "a".into(),
        item_id: item.into(),
        title: Some(format!("Title {item}")),
        role,
        lane,
        engine: EngineId::Codex,
        model: "gpt-x".into(),
    }
}

fn live_status(on: bool, src: &StatusSource) -> ComputerStatus {
    let mut s = settled(EngineChoiceConfig::default(), 3);
    s.live_sessions = on;
    build(&s, src, None, &src.live.snapshot())
}

#[test]
fn live_sessions_join_their_lane_and_the_rest_are_listed_apart() {
    let src = source(&["a"]);
    let reg = src.live.sessions();
    let _g1 = reg.register(info("i1", Role::Implementer, Some(2)));
    let _g2 = reg.register(info("ask1", Role::Assistant, None));
    let _g3 = reg.register(info("d1", Role::Drafter, None));
    let _g4 = reg.register(info("s1", Role::Scout, None));
    let s = live_status(true, &src);
    assert_eq!(s.header.lanes_busy, 1);
    let lane = &s.lanes[1];
    assert_eq!(lane.state, "busy");
    let ses = lane.session.as_ref().unwrap();
    assert_eq!(
        (ses.role.as_deref(), ses.engine.as_deref(), ses.model.as_deref()),
        (Some("implementer"), Some("codex"), Some("gpt-x"))
    );
    assert_eq!((ses.run_sec, ses.quiet_sec), (Some(0), Some(0)));
    assert_eq!(s.lanes[0].state, "free");
    let ids: Vec<&str> = s.sessions.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["ask1", "d1", "s1"]);
}

#[test]
fn the_json_has_names_and_times_but_no_prompt_or_transcript() {
    let src = source(&["a"]);
    let _g = src.live.sessions().register(info("i1", Role::Implementer, Some(1)));
    let v = serde_json::to_value(live_status(true, &src)).unwrap();
    let ses = &v["lanes"][0]["session"];
    assert_eq!(ses["runSec"], 0);
    assert_eq!(ses["quietSec"], 0);
    let mut keys: Vec<&String> = ses.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "engine", "id", "model", "project", "quietSec", "role", "runSec", "title"
        ]
    );
}

#[test]
fn with_the_flag_off_the_registry_is_ignored_and_the_old_view_stays() {
    let src = source(&["a"]);
    let _g = src.live.sessions().register(info("i1", Role::Implementer, Some(1)));
    let s = live_status(false, &src);
    assert_eq!(s.header.lanes_busy, 0);
    assert!(s.lanes.iter().all(|l| l.session.is_none() && l.state == "free"));
    assert!(s.sessions.is_empty());
    let v = serde_json::to_value(&s).unwrap();
    assert!(v["lanes"][0].get("session").is_none());
}

#[test]
fn the_page_renders_lane_sessions_and_the_list_without_a_lane() {
    let html = include_str!("../../../assets/settings.html");
    for needle in [
        "id=\"sessions-list\"",
        "renderSessions(st)",
        "run \" + secs(x.runSec)",
        "l.session",
    ] {
        assert!(html.contains(needle), "{needle}");
    }
}

#[tokio::test]
async fn the_token_lines_show_what_ended_sessions_added() {
    use crate::engines::engine::{EngineId, SessionResult, Tokens};
    let dir = tempfile::tempdir().unwrap();
    let sessions = crate::orchestrator::live::sessions::Sessions::default().with_machine_dir(dir.path().to_path_buf());
    sessions.limits().record_any_role(true);
    let result = |input, output| SessionResult {
        ok: true,
        subtype: String::new(),
        structured: None,
        error: None,
        cost_usd: None,
        session_id: String::new(),
        limit_hit: false,
        signed_out: false,
        context_tokens: None,
        usage: None,
        tokens: Some(Tokens { input, output }),
    };
    for (engine, r) in [(EngineId::Cursor, result(10, 3)), (EngineId::Codex, result(4, 1))] {
        let info = crate::orchestrator::live::sessions::SessionInfo {
            project: "p".into(),
            item_id: "i".into(),
            title: None,
            role: crate::engines::choose::Role::Implementer,
            lane: None,
            engine,
            model: String::new(),
        };
        sessions
            .track(info, async {
                crate::orchestrator::live::sessions::record_session_end(engine, &r)
            })
            .await;
    }
    let machine = crate::store::MachineStore::load(dir.path()).unwrap();
    let mut s = settled(EngineChoiceConfig::default(), 1);
    s.codex_tokens = machine.codex_today_tokens();
    s.cursor_tokens = machine.cursor_today_tokens();
    let status = build(&s, &source(&["a"]), None, &LiveState::default());
    assert_eq!(row(&status, "cursor").today_tokens, Some(13));
    assert_eq!(row(&status, "codex").today_tokens, Some(5));
}
