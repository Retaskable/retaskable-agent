use std::path::PathBuf;

use super::*;
use crate::config::{LaneEngine, RoleEngines};
use crate::connector::{PauseState, ProjectView};
use crate::lane_pool::LaneSizes;

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
    }
}

fn source(projects: &[&str]) -> StatusSource {
    StatusSource {
        view: AgentView {
            machine: "pc".into(),
            tags: vec![],
            projects: projects
                .iter()
                .map(|id| ProjectView {
                    id: id.to_string(),
                    github_repo: format!("o/{id}"),
                    backlog_path: "backlog".into(),
                    repo_path: PathBuf::new(),
                    worktrees_dir: PathBuf::new(),
                    base_branch: String::new(),
                    galleries: vec![],
                })
                .collect(),
            version: "1.2.3".into(),
            lanes: 3,
            chore_lane: Some(3),
            lane_sizes: LaneSizes::default(),
            paused: PauseState::default(),
        },
        started: Instant::now(),
        checks: Arc::new(Mutex::new(None)),
    }
}

fn settled(choice: EngineChoiceConfig, lanes: u32) -> Settled {
    Settled {
        choice,
        local: LocalModelConfig::default(),
        lanes,
        codex_tokens: Some(42),
        cursor_tokens: None,
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
    let s = build(&settled(choice, 3), &source(&["a"]), None);
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
    let s = build(&settled(choice, 3), &source(&["a"]), Some(&c));
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
    let s = build(&settled(EngineChoiceConfig::default(), 1), &source(&[]), None);
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
    let s = build(&settled(EngineChoiceConfig::default(), 1), &src, Some(&c));
    let titles: Vec<&str> = s.alerts.iter().map(|a| a.title.as_str()).collect();
    assert_eq!(titles, ["Claude Code is signed out", "o/a is paused"]);
    assert_eq!(s.alerts[0].tone, "bad");
    assert_eq!(row(&s, "claude").state_text, "Signed out");
    assert_eq!(s.header.state, "running");
    src.view.paused.set("b", Some("later".into()));
    let s = build(&settled(EngineChoiceConfig::default(), 1), &src, None);
    assert_eq!((s.header.state, s.header.state_text), ("paused", "Paused"));
}

#[test]
fn the_status_serialises_with_the_typescript_agents_field_names() {
    let s = build(&settled(EngineChoiceConfig::default(), 1), &source(&["a"]), None);
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
