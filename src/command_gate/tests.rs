use super::*;
use crate::view::ItemCommands;

fn view(play: &str) -> ProjectView {
    ProjectView {
        play_command: play.into(),
        ..crate::test_support::project_view("p")
    }
}

fn settings(version: u64, play: &str) -> StoreSettings {
    serde_json::from_value(json!({ "version": version, "commandsVersion": version, "values": { "playCommand": play } }))
        .unwrap()
}

fn file(play: &str) -> ProjectConfig {
    ProjectConfig {
        play_command: play.into(),
        ..ProjectConfig::default()
    }
}

#[test]
fn play_main_refuses_with_nothing_allowed_and_runs_the_allowed_command() {
    let dir = tempfile::tempdir().unwrap();
    let commands = ItemCommands::default();
    commands.gates().open(dir.path(), "p");
    commands
        .gates()
        .consider("p", Some(&settings(1, "store play")), &file("file play"))
        .unwrap();
    let held = commands.allowed_view(&view("file play"));
    assert_eq!(held.play_command, "");
    assert_eq!(
        crate::play::start("p", &held),
        Err("No play command set for this project.".to_string())
    );
    commands.gates().approve("p", 1).unwrap();
    assert_eq!(commands.allowed_view(&view("file play")).play_command, "store play");
    // A changed store command waits; the allowed one still runs.
    commands
        .gates()
        .consider("p", Some(&settings(2, "newer")), &file("file play"))
        .unwrap();
    assert_eq!(commands.allowed_view(&view("file play")).play_command, "store play");
}

#[test]
fn a_restart_keeps_what_was_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let first = CommandGates::default();
    first.open(dir.path(), "p");
    first
        .consider("p", Some(&settings(1, "store play")), &file(""))
        .unwrap();
    first.approve("p", 1).unwrap();
    let second = CommandGates::default();
    second.open(dir.path(), "p");
    assert_eq!(second.config("p", &file("file play")).play_command, "store play");
}

#[test]
fn with_no_settings_in_the_store_config_json_runs() {
    let dir = tempfile::tempdir().unwrap();
    let gates = CommandGates::default();
    gates.open(dir.path(), "p");
    assert_eq!(gates.config("p", &file("file play")).play_command, "");
    gates.consider("p", None, &file("file play")).unwrap();
    assert_eq!(gates.config("p", &file("file play")).play_command, "file play");
}

#[test]
fn a_project_the_loop_never_opened_runs_nothing_and_has_nothing_waiting() {
    let gates = CommandGates::default();
    assert_eq!(gates.config("ghost", &file("file play")).play_command, "");
    assert_eq!(gates.view(&view("file play")).play_command, "");
    assert!(gates.waiting("ghost").is_none());
    assert!(gates.allowed("ghost").is_none());
    // Considering settings for an unopened project moves nothing.
    let moved = gates
        .consider("ghost", Some(&settings(1, "x")), &file("file play"))
        .unwrap();
    assert_eq!(moved, Moved::default());
}

#[test]
fn approving_with_nothing_waiting_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let gates = CommandGates::default();
    assert_eq!(
        gates.approve("ghost", 1),
        Err(crate::allowed_commands::NOT_WAITING.to_string())
    );
    gates.open(dir.path(), "p");
    assert_eq!(
        gates.approve("p", 1),
        Err(crate::allowed_commands::NOT_WAITING.to_string())
    );
}

#[test]
fn a_waiting_version_wakes_subscribers_and_shows_in_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let gates = CommandGates::default();
    gates.open(dir.path(), "p");
    let rx = gates.subscribe();
    assert!(!rx.has_changed().unwrap());
    let moved = gates
        .consider("p", Some(&settings(1, "store play")), &file("file play"))
        .unwrap();
    assert!(moved.waiting);
    assert!(rx.has_changed().unwrap());
    assert_eq!(gates.waiting("p").map(|w| w.version), Some(1));
}

#[test]
fn opening_a_project_twice_keeps_the_first_gate() {
    let dir = tempfile::tempdir().unwrap();
    let gates = CommandGates::default();
    gates.open(dir.path(), "p");
    gates
        .consider("p", Some(&settings(1, "store play")), &file(""))
        .unwrap();
    gates.open(dir.path(), "p");
    assert_eq!(gates.waiting("p").map(|w| w.version), Some(1));
}

#[test]
fn rollout_commands_follow_the_releases_answer() {
    let dir = tempfile::tempdir().unwrap();
    let rollout = |v: u64, traffic: &str| -> StoreSettings {
        serde_json::from_value(json!({ "version": v, "commandsVersion": v, "values": {
            "deploy": {"production": {"rollout": {"traffic": traffic, "deploy": "d"}}} } }))
        .unwrap()
    };
    let on = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gates = CommandGates::default();
    let flag = on.clone();
    gates.set_releases(move |p| p == "p" && flag.load(std::sync::atomic::Ordering::SeqCst));
    gates.open(dir.path(), "p");
    gates.consider("p", Some(&rollout(1, "t1")), &file("")).unwrap();
    gates.approve("p", 1).unwrap();
    gates.consider("p", Some(&rollout(2, "t2")), &file("")).unwrap();
    assert_eq!(gates.waiting("p").unwrap().version, 2);
    on.store(true, std::sync::atomic::Ordering::SeqCst);
    gates.consider("p", Some(&rollout(2, "t2")), &file("")).unwrap();
    assert!(gates.waiting("p").is_none());
    assert_eq!(gates.allowed("p").unwrap().commands_version, 2);
}
