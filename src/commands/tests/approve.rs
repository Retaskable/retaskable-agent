//! `approve_commands` : the owner approves the waiting commands of a project.
use super::*;
use crate::allowed_commands::{NOT_WAITING, allowed_commands_file};
use crate::backlog::settings::StoreSettings;
use crate::config::ProjectConfig;

fn store(version: u64, command: &str) -> StoreSettings {
    serde_json::from_value(json!({
        "version": version,
        "commandsVersion": version,
        "values": { "checks": [{ "name": "t", "command": command }] },
    }))
    .unwrap()
}

/// A view with the gate open for "one", the owner "jc", and 1's `old` allowed; version 2 (`new`) waits.
fn gated(dir: &Path) -> AgentView {
    let mut v = view();
    v.item_commands = Default::default();
    v.owner.set("jc".into()).unwrap();
    let gates = v.item_commands.gates();
    gates.open(dir, "one");
    let file = ProjectConfig::default();
    gates.consider("one", Some(&store(1, "old")), &file).unwrap();
    gates.approve("one", 1).unwrap();
    gates.consider("one", Some(&store(2, "new")), &file).unwrap();
    v
}

fn checks(v: &AgentView) -> Vec<String> {
    let cfg = v.item_commands.allowed_config("one", &ProjectConfig::default());
    cfg.checks.iter().map(|c| c.command.clone()).collect()
}

fn approve(v: &AgentView, by: &str, args: Value) -> Value {
    let mut c = cmd("approve_commands", args);
    c.issued_by = by.into();
    run(&c, "one", &mut Seen::default(), v, &|_| {}, &data_dir())
}

fn nacked(answer: &Value) -> String {
    assert_eq!(answer["type"], "nack", "{answer}");
    answer["reason"].as_str().unwrap_or_default().to_string()
}

#[test]
fn the_owner_approves_a_waiting_change_and_the_new_command_runs() {
    let dir = tempfile::tempdir().unwrap();
    let v = gated(dir.path());
    assert_eq!(checks(&v), vec!["old"]);
    assert_eq!(
        approve(&v, "jc", json!({ "version": 2 })),
        json!({ "type": "ack", "id": "c1" })
    );
    assert_eq!(checks(&v), vec!["new"]);
}

#[test]
fn a_stale_version_or_nothing_waiting_is_a_nack() {
    let dir = tempfile::tempdir().unwrap();
    let v = gated(dir.path());
    assert_eq!(nacked(&approve(&v, "jc", json!({ "version": 1 }))), NOT_WAITING);
    assert_eq!(checks(&v), vec!["old"]);
    approve(&v, "jc", json!({ "version": 2 }));
    assert_eq!(nacked(&approve(&v, "jc", json!({ "version": 2 }))), NOT_WAITING);
}

#[test]
fn a_missing_or_non_numeric_version_is_a_nack() {
    let dir = tempfile::tempdir().unwrap();
    let v = gated(dir.path());
    nacked(&approve(&v, "jc", json!({})));
    nacked(&approve(&v, "jc", json!({ "version": "2" })));
    assert_eq!(checks(&v), vec!["old"]);
}

#[test]
fn someone_other_than_the_owner_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let v = gated(dir.path());
    nacked(&approve(&v, "mallory", json!({ "version": 2 })));
    assert_eq!(checks(&v), vec!["old"]);
}

#[test]
fn a_restart_keeps_the_approved_commands() {
    let dir = tempfile::tempdir().unwrap();
    let v = gated(dir.path());
    approve(&v, "jc", json!({ "version": 2 }));
    let again = crate::allowed_commands::CommandAllowing::new(allowed_commands_file(dir.path(), "one"));
    assert_eq!(again.allowed.unwrap().commands_version, 2);
}
