use super::*;
use crate::config::DeployTarget;
use serde_json::json;

fn record(version: u64, test: &str) -> StoreRecord {
    StoreRecord {
        commands_version: version,
        values: json!({
            "checks": [{"name": "test", "command": test}],
            "playCommand": "npm start",
            "branch": "ignored",
            "deploy": {"staging": {"smoke": "curl x"}, "production": {"rollout": {"traffic": "t", "machine": "m"}}},
            "appsScript": {"sheetId": "s"},
        }),
        updated_by: Some("owner".into()),
        history: vec![SettingsChange {
            by: "pat".into(),
            fields: vec!["checks".into()],
        }],
    }
}

fn cfg(test: &str) -> ProjectConfig {
    ProjectConfig {
        checks: serde_json::from_value(json!([{"name": "test", "command": test}])).unwrap(),
        play_command: "old play".into(),
        deploy: crate::config::DeployConfig {
            staging: Some(DeployTarget {
                rollout: None,
                branch: "main".into(),
                url: None,
                smoke: Some("old".into()),
                version_path: None,
            }),
            production: None,
        },
        ..ProjectConfig::default()
    }
}

fn file() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let f = allowed_commands_file(dir.path(), "p");
    (dir, f)
}

#[test]
fn commands_of_picks_command_fields_and_drops_machine() {
    let m = commands_of(&record(1, "npm test").values);
    let mut keys: Vec<_> = m.keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "checks",
            "deploy.production.rollout",
            "deploy.staging.smoke",
            "playCommand"
        ]
    );
    assert_eq!(m["deploy.production.rollout"], json!({"traffic": "t"}));
}

#[test]
fn with_allowed_empties_without_allowed_and_keeps_without_record() {
    let c = cfg("npm test");
    let e = with_allowed(&c, None, true);
    assert!(e.checks.is_empty() && e.play_command.is_empty());
    assert_eq!(e.deploy.staging.unwrap().smoke, None);
    assert_eq!(with_allowed(&c, None, false), c);
}

#[test]
fn changed_command_waits_until_approved_and_survives_restart() {
    let (_d, f) = file();
    let c = cfg("npm test");
    let mut a = CommandAllowing::new(f.clone());
    // First read: nothing running matches, so it waits; the config is empty meanwhile.
    let m = a
        .consider(Some(&record(1, "npm test")), &Map::new(), &Map::new())
        .unwrap();
    assert!(m.waiting);
    assert!(with_allowed(&c, a.allowed.as_ref(), true).checks.is_empty());
    assert_eq!(a.waiting().unwrap().by.as_deref(), Some("pat"));
    a.approve(1).unwrap();
    assert_eq!(with_allowed(&c, a.allowed.as_ref(), true).checks[0].command, "npm test");

    // A changed command waits while the old one still shows.
    let run = a.allowed.as_ref().unwrap().values.clone();
    a.consider(Some(&record(2, "npm run e2e")), &run, &Map::new()).unwrap();
    assert_eq!(
        a.waiting().unwrap().text,
        "checks: test: `npm test` → test: `npm run e2e`"
    );
    assert_eq!(with_allowed(&c, a.allowed.as_ref(), true).checks[0].command, "npm test");

    // A newer version replaces it; the old one is refused.
    a.consider(Some(&record(3, "npm run lint")), &run, &Map::new()).unwrap();
    assert_eq!(a.approve(2), Err(NOT_WAITING.to_string()));
    let m = a.approve(3).unwrap();
    assert!(m.allowed && m.waiting);
    assert_eq!(a.approve(3), Err(NOT_WAITING.to_string()));
    assert_eq!(
        with_allowed(&c, a.allowed.as_ref(), true).checks[0].command,
        "npm run lint"
    );

    let again = CommandAllowing::new(f);
    assert_eq!(again.allowed, a.allowed);
}

#[test]
fn version_that_changes_nothing_is_taken() {
    let (_d, f) = file();
    let mut a = CommandAllowing::new(f);
    let store = commands_of(&record(5, "npm test").values);
    let m = a.consider(Some(&record(5, "npm test")), &store, &Map::new()).unwrap();
    assert!(!m.waiting && a.waiting().is_none());
    assert_eq!(a.allowed.as_ref().unwrap().commands_version, 5);
}

#[test]
fn nothing_waiting_is_not_waiting() {
    let (_d, f) = file();
    let mut a = CommandAllowing::new(f);
    assert_eq!(a.approve(1), Err(NOT_WAITING.to_string()));
    a.close();
}

#[test]
fn unreadable_file_reads_as_none() {
    let (_d, f) = file();
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "nope").unwrap();
    assert_eq!(AllowedCommands::read(&f), None);
}

#[test]
fn change_text_is_cut() {
    let big = "x".repeat(3000);
    let ch = [CommandChange {
        field: "playCommand".into(),
        before: None,
        after: Some(json!(big)),
    }];
    let t = commands_change_text(&ch);
    assert_eq!(t.chars().count(), 2000);
    assert!(t.starts_with("the play command: nothing → `x"));
}

#[test]
fn first_save_of_an_empty_set_moves_the_allowed_commands() {
    let (_d, f) = file();
    let mut a = CommandAllowing::new(f);
    let rec = StoreRecord {
        commands_version: 1,
        values: json!({"branch": "x"}),
        ..StoreRecord::default()
    };
    let m = a.consider(Some(&rec), &Map::new(), &Map::new()).unwrap();
    assert!(m.allowed);
    assert_eq!(
        with_allowed(&cfg("npm test"), a.allowed.as_ref(), true).checks,
        cfg("npm test").checks
    );
}
