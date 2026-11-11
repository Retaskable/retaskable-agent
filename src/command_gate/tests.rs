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
