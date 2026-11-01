use std::collections::BTreeMap;
use std::path::PathBuf;

use super::*;

fn project(id: &str, github_repo: &str) -> ProjectView {
    ProjectView {
        id: id.to_string(),
        github_repo: github_repo.to_string(),
        backlog_path: "backlog.json".to_string(),
        repo_path: PathBuf::from("repo"),
        worktrees_dir: PathBuf::from("worktrees"),
        base_branch: "main".to_string(),
        galleries: Vec::new(),
    }
}

fn agent(projects: Vec<ProjectView>) -> AgentView {
    AgentView {
        machine: "my-machine".to_string(),
        tags: vec!["windows".to_string()],
        projects,
        version: "1.2.3".to_string(),
        lanes: 2,
        chore_lane: None,
        lane_sizes: LaneSizes::default(),
        paused: PauseState::default(),
    }
}

#[test]
fn keys_is_the_agent_hub_before_any_project_exists() {
    let view = agent(Vec::new());
    assert_eq!(view.keys(), vec![String::new()]);
}

#[test]
fn keys_is_each_projects_id_once_projects_exist() {
    let view = agent(vec![project("alpha", ""), project("beta", "")]);
    assert_eq!(view.keys(), vec!["alpha".to_string(), "beta".to_string()]);
}

#[test]
fn hello_reports_the_matched_projects_repo_and_backlog() {
    let view = agent(vec![project("alpha", "org/repo")]);

    let hello = view.hello("alpha");

    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["project"]["githubRepo"], "org/repo");
    assert_eq!(hello["project"]["backlogPath"], "backlog.json");
}

#[test]
fn hello_project_is_null_before_the_project_has_a_repo() {
    let view = agent(vec![project("alpha", "")]);

    assert_eq!(view.hello("alpha")["project"], serde_json::Value::Null);
}

#[test]
fn hello_project_is_null_for_an_unknown_project() {
    let view = agent(vec![project("alpha", "org/repo")]);

    assert_eq!(view.hello("beta")["project"], serde_json::Value::Null);
}

#[test]
fn hello_truncates_a_long_version_and_machine_name() {
    let mut view = agent(Vec::new());
    view.version = "v".repeat(60);
    view.machine = "m".repeat(250);

    let hello = view.hello("");

    assert_eq!(hello["agentVersion"].as_str().unwrap().chars().count(), 50);
    assert_eq!(hello["machine"].as_str().unwrap().chars().count(), 200);
}

#[test]
fn lanes_list_marks_only_the_chore_lane() {
    let mut view = agent(Vec::new());
    view.lanes = 3;
    view.chore_lane = Some(2);

    let lanes = view.lanes_list("");

    assert_eq!(lanes.len(), 3);
    assert_eq!(lanes[0]["k"], 1);
    assert!(lanes[0].get("note").is_none());
    assert_eq!(lanes[1]["note"], CHORE_LANE_NOTE);
    assert!(lanes[2].get("note").is_none());
}

#[test]
fn lanes_list_reports_each_lanes_disk_use() {
    let mut view = agent(Vec::new());
    view.lanes = 2;
    view.lane_sizes.set("proj", BTreeMap::from([(1, 2.5)]));

    let lanes = view.lanes_list("proj");

    assert_eq!(lanes[0]["diskGb"], 2.5);
    assert_eq!(lanes[1]["diskGb"], 0.0);
}

#[test]
fn pause_state_starts_unpaused() {
    let state = PauseState::default();
    assert_eq!(state.reason("proj"), None);
}

#[test]
fn pause_state_set_some_pauses_with_that_reason() {
    let state = PauseState::default();
    state.set("proj", Some("stuck".to_string()));
    assert_eq!(state.reason("proj"), Some("stuck".to_string()));
}

#[test]
fn pause_state_set_none_resumes() {
    let state = PauseState::default();
    state.set("proj", Some("stuck".to_string()));
    state.set("proj", None);
    assert_eq!(state.reason("proj"), None);
}

#[test]
fn pause_state_is_kept_per_project() {
    let state = PauseState::default();
    state.set("a", Some("stuck".to_string()));
    assert_eq!(state.reason("b"), None);
}

#[test]
fn pause_state_set_same_reason_does_not_notify_subscribers() {
    let state = PauseState::default();
    let mut rx = state.subscribe();
    state.set("proj", Some("stuck".to_string()));
    assert!(rx.has_changed().unwrap());
    rx.mark_unchanged();

    state.set("proj", Some("stuck".to_string()));

    assert!(!rx.has_changed().unwrap(), "same reason: nothing to send");
}

#[test]
fn pause_state_set_a_different_reason_notifies_subscribers() {
    let state = PauseState::default();
    state.set("proj", Some("stuck".to_string()));
    let mut rx = state.subscribe();
    rx.mark_unchanged();

    state.set("proj", Some("still stuck".to_string()));

    assert!(rx.has_changed().unwrap());
}
