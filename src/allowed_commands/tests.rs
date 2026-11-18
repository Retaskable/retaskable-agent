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
            ..Default::default()
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
        .consider(Some(&record(1, "npm test")), &Map::new(), &Map::new(), false)
        .unwrap();
    assert!(m.waiting);
    assert!(with_allowed(&c, a.allowed.as_ref(), true).checks.is_empty());
    assert_eq!(a.waiting().unwrap().by.as_deref(), Some("pat"));
    a.approve(1).unwrap();
    assert_eq!(with_allowed(&c, a.allowed.as_ref(), true).checks[0].command, "npm test");

    // A changed command waits while the old one still shows.
    let run = a.allowed.as_ref().unwrap().values.clone();
    a.consider(Some(&record(2, "npm run e2e")), &run, &Map::new(), false)
        .unwrap();
    assert_eq!(
        a.waiting().unwrap().text,
        "checks: test: `npm test` → test: `npm run e2e`"
    );
    assert_eq!(with_allowed(&c, a.allowed.as_ref(), true).checks[0].command, "npm test");

    // A newer version replaces it; the old one is refused.
    a.consider(Some(&record(3, "npm run lint")), &run, &Map::new(), false)
        .unwrap();
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
    let m = a
        .consider(Some(&record(5, "npm test")), &store, &Map::new(), false)
        .unwrap();
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
    let m = a.consider(Some(&rec), &Map::new(), &Map::new(), false).unwrap();
    assert!(m.allowed);
    assert_eq!(
        with_allowed(&cfg("npm test"), a.allowed.as_ref(), true).checks,
        cfg("npm test").checks
    );
}

#[test]
fn the_rollout_approval_text_has_no_schedule_and_no_site() {
    let rollout = |v: Value| {
        let change = CommandChange {
            field: "deploy.production.rollout".into(),
            before: None,
            after: Some(v),
        };
        commands_change_text(&[change])
    };
    let text = rollout(json!({"traffic": "t", "health": "h", "site": true}));
    assert!(text.contains("traffic `t`, health `h`"), "{text}");
    assert!(!text.contains("minutes") && !text.contains("retaskable.com"), "{text}");
}

fn with_rollout(version: u64, rollout: serde_json::Value, play: &str) -> StoreRecord {
    StoreRecord {
        commands_version: version,
        values: json!({ "playCommand": play, "deploy": {"production": {"rollout": rollout}} }),
        updated_by: None,
        history: Vec::new(),
    }
}

/// Approves version 1 (a rollout with a `traffic` command) and gives what then runs.
fn approved_rollout(a: &mut CommandAllowing) -> Map {
    let first = with_rollout(1, json!({"traffic": "t1", "health": "h1", "deploy": "d"}), "go");
    a.consider(Some(&first), &Map::new(), &Map::new(), false).unwrap();
    a.approve(1).unwrap();
    a.allowed.as_ref().unwrap().values.clone()
}

#[test]
fn rollout_commands_run_unasked_only_while_releases_is_on() {
    let (_d, f) = file();
    let mut a = CommandAllowing::new(f);
    let run = approved_rollout(&mut a);
    let next = with_rollout(2, json!({"traffic": "t2", "health": "h2", "deploy": "d"}), "go");
    // Off: it waits for Approve.
    a.consider(Some(&next), &run, &Map::new(), false).unwrap();
    assert_eq!(a.waiting().unwrap().version, 2);
    // On: the same version is taken, and what waited is gone.
    let m = a.consider(Some(&next), &run, &Map::new(), true).unwrap();
    assert!(m.waiting && m.allowed);
    assert!(a.waiting().is_none());
    assert_eq!(a.allowed.as_ref().unwrap().commands_version, 2);
    assert_eq!(
        a.allowed.as_ref().unwrap().values["deploy.production.rollout"]["traffic"],
        "t2"
    );
}

#[test]
fn releases_on_still_asks_for_anything_but_the_rollout_commands() {
    let (_d, f) = file();
    let mut a = CommandAllowing::new(f);
    let run = approved_rollout(&mut a);
    // Another command changed too.
    let both = with_rollout(2, json!({"traffic": "t2", "health": "h1", "deploy": "d"}), "other");
    a.consider(Some(&both), &run, &Map::new(), true).unwrap();
    assert_eq!(a.waiting().unwrap().version, 2);
    // The rollout's own deploy command changed.
    let deploy = with_rollout(3, json!({"traffic": "t1", "health": "h1", "deploy": "evil"}), "go");
    a.consider(Some(&deploy), &run, &Map::new(), true).unwrap();
    assert_eq!(a.waiting().unwrap().version, 3);
    // A rollout that did not exist before (no rollout yet) is not "the rollout already exists".
    let (_d2, f2) = file();
    let mut fresh = CommandAllowing::new(f2);
    let none = Map::new();
    fresh
        .consider(Some(&with_rollout(1, json!({"traffic": "t"}), "")), &none, &none, true)
        .unwrap();
    assert_eq!(fresh.waiting().unwrap().version, 1);
}

/// Live 2026-11-18: Retaskable.com added on the Rust agent with only `"deploy": {"mergesTo": "production"}` in config.json
/// never rolled out, though its rollout was approved: the approved commands had no production target to land on, and
/// the store's branch and URL were never taken. With the store's `deploy` laid under them, the rollout runs.
#[test]
fn an_approved_rollout_runs_on_a_project_whose_config_names_no_production_target() {
    let file = ProjectConfig {
        deploy: serde_json::from_value(json!({"mergesTo": "production"})).unwrap(),
        ..Default::default()
    };
    let store = json!({
        "staging": {"branch": "main", "url": "https://staging.example", "smoke": "npm run smoke"},
        "production": {"branch": "production", "url": "https://example", "rollout": {"traffic": "npm run t --", "health": "npm run h"}},
        "mergesTo": "production",
    });
    let allowed = AllowedCommands {
        at: 1,
        commands_version: 5,
        values: commands_of(&json!({"deploy": store.clone()})),
    };
    let laid = with_allowed(
        &ProjectConfig {
            deploy: file.deploy.with_store(&store),
            ..file.clone()
        },
        Some(&allowed),
        true,
    );
    assert_eq!(
        crate::rollout::uses_store(&laid),
        Ok(("production".to_string(), Some("main".to_string())))
    );
    let production = laid.deploy.production.as_ref().unwrap();
    assert_eq!(production.url.as_deref(), Some("https://example"));
    assert_eq!(
        laid.deploy.staging.as_ref().and_then(|s| s.smoke.as_deref()),
        Some("npm run smoke")
    );
    // The store's own commands never run unapproved: its deploy alone gives targets, not commands.
    let unapproved = with_allowed(
        &ProjectConfig {
            deploy: file.deploy.with_store(&store),
            ..file
        },
        None,
        true,
    );
    assert!(unapproved.deploy.production.as_ref().unwrap().rollout.is_none());
    assert!(crate::rollout::uses_store(&unapproved).is_err());
}

#[test]
fn an_approved_smoke_or_rollout_with_no_target_at_all_makes_one() {
    let allowed = AllowedCommands {
        at: 1,
        commands_version: 2,
        values: commands_of(
            &json!({"deploy": {"staging": {"smoke": "s"}, "production": {"rollout": {"traffic": "t", "health": "h"}}}}),
        ),
    };
    let laid = with_allowed(&ProjectConfig::default(), Some(&allowed), true);
    assert_eq!(laid.deploy.staging.unwrap().smoke.as_deref(), Some("s"));
    assert!(laid.deploy.production.unwrap().rollout.is_some());
}

/// TS `effectiveDeploy`: Retaskable.com merges straight to production, so the rollout reports main's commits since what
/// production serves, not the stale `production` branch (live 2026-11-18: 1,097 commits, refused by the store).
#[test]
fn production_only_has_no_staging_and_production_is_the_default_branch() {
    let deploy: crate::config::DeployConfig = serde_json::from_value(json!({
        "mergesTo": "production",
        "staging": {"branch": "main"},
        "production": {"branch": "production", "url": "https://example"},
    }))
    .unwrap();
    let e = deploy.effective("main");
    assert!(e.staging.is_none());
    assert_eq!(e.production.as_ref().map(|p| p.branch.as_str()), Some("main"));
    assert_eq!(e.production.unwrap().url.as_deref(), Some("https://example"));
    let staged: crate::config::DeployConfig =
        serde_json::from_value(json!({"staging": {"branch": "main"}, "production": {"branch": "production"}})).unwrap();
    assert_eq!(staged.effective("main"), staged, "staging then production: unchanged");
}
