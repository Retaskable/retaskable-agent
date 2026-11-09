use serde_json::json;

use super::*;

fn record(version: u64, play: &str) -> SettingsRecord {
    SettingsRecord {
        commands_version: version,
        values: json!({"playCommand": play, "checks": [{"name": "t", "command": "npm test"}], "port": 1}),
        updated_by: Some("sam".into()),
        history: vec![],
    }
}

fn cfg() -> ProjectConfig {
    serde_json::from_value(json!({
        "playCommand": "old", "checks": [{"name": "t", "command": "npm test"}],
        "deploy": {"staging": {"branch": "s", "smoke": "curl x"}}
    }))
    .unwrap()
}

fn allowing(dir: &Path) -> CommandAllowing {
    CommandAllowing::new(AllowedCommands::file(dir, "p"))
}

#[test]
fn commands_of_picks_only_command_fields_and_drops_machine() {
    let v = json!({
        "checks": [], "playCommand": "x", "promptNotes": "n", "ciCommand": "c", "galleries": [], "phone": {"buildCommand": "b"},
        "engines": {"allow": []}, "port": 3, "appsScript": {"sheetId": "s"}, "nfrs": {"x": 1},
        "deploy": {"staging": {"smoke": "s", "url": "u"}, "production": {"rollout": {"machine": "m", "traffic": "t"}, "branch": "b"}}
    });
    let out = commands_of(&v);
    let mut keys: Vec<&str> = out.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "checks",
            "ciCommand",
            "deploy.production.rollout",
            "deploy.staging.smoke",
            "engines",
            "galleries",
            "phone",
            "playCommand",
            "promptNotes"
        ]
    );
    assert_eq!(out["deploy.production.rollout"], json!({"traffic": "t"}));
}

#[test]
fn with_allowed_empties_with_a_record_and_nothing_allowed() {
    let out = with_allowed(&cfg(), None, true);
    assert!(out.checks.is_empty() && out.play_command.is_empty() && out.galleries.is_empty());
    assert!(out.phone.is_none() && out.engines.is_none());
    assert_eq!(out.deploy.staging.unwrap().smoke, None);
    assert_eq!(with_allowed(&cfg(), None, false), cfg());
}

#[test]
fn a_changed_command_waits_while_the_old_one_runs() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = allowing(dir.path());
    let running = commands_of(&record(1, "old").values);
    a.allowed = Some(AllowedCommands {
        commands_version: 1,
        values: running.clone(),
        at: 0,
    });
    assert!(a.consider(Some(&record(2, "new")), &running, &Map::new()));
    let w = a.waiting().unwrap();
    assert_eq!((w.version, w.by.as_deref()), (2, Some("sam")));
    assert_eq!(w.text, "the play command: `old` → `new`");
    assert_eq!(with_allowed(&cfg(), a.allowed.as_ref(), true).play_command, "old");
    // The same version again changes nothing.
    assert!(!a.consider(Some(&record(2, "new")), &running, &Map::new()));
}

#[test]
fn a_version_that_changes_nothing_is_taken() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = allowing(dir.path());
    let running = commands_of(&record(1, "old").values);
    a.allowed = Some(AllowedCommands {
        commands_version: 1,
        values: running.clone(),
        at: 0,
    });
    assert!(!a.consider(Some(&record(2, "old")), &running, &Map::new()));
    assert!(a.waiting().is_none());
    assert_eq!(a.allowed.as_ref().unwrap().commands_version, 2);
}

#[test]
fn a_newer_version_replaces_the_waiting_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = allowing(dir.path());
    let running = commands_of(&record(1, "old").values);
    a.allowed = Some(AllowedCommands {
        commands_version: 1,
        values: running.clone(),
        at: 0,
    });
    a.consider(Some(&record(2, "b")), &running, &Map::new());
    assert!(a.consider(Some(&record(3, "c")), &running, &Map::new()));
    assert_eq!(a.waiting().unwrap().version, 3);
    assert_eq!(a.approve(2), Err(NOT_WAITING.to_string()));
}

#[test]
fn approve_saves_and_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = allowing(dir.path());
    let running = Map::new();
    assert_eq!(a.approve(1), Err(NOT_WAITING.to_string()));
    a.consider(Some(&record(1, "new")), &running, &Map::new());
    assert_eq!(a.approve(2), Err(NOT_WAITING.to_string()));
    assert_eq!(a.approve(1), Ok(true));
    assert!(a.waiting().is_none());
    assert_eq!(with_allowed(&cfg(), a.allowed.as_ref(), true).play_command, "new");
    let again = allowing(dir.path());
    assert_eq!(again.allowed, a.allowed);
}

#[test]
fn unreadable_file_reads_as_none_and_text_is_cut() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x.json");
    std::fs::write(&file, "{nope").unwrap();
    assert_eq!(AllowedCommands::read(&file), None);
    let big = "x".repeat(3000);
    let c = CommandChange {
        field: "checks".into(),
        before: json!([{"name": "a", "command": "npm test"}]),
        after: json!([{"name": "a", "command": "npm run e2e"}]),
    };
    assert_eq!(
        commands_change_text(std::slice::from_ref(&c)),
        "checks: a: `npm test` → a: `npm run e2e`"
    );
    let long = CommandChange {
        field: "playCommand".into(),
        before: json!(big),
        after: json!("y"),
    };
    assert_eq!(commands_change_text(&[long]).chars().count(), 2000);
}
