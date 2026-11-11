use super::*;

fn check(command: &str) -> CheckConfig {
    CheckConfig {
        command: command.into(),
        name: String::new(),
        blocking: false,
        timeout_sec: 0,
    }
}

#[test]
fn program_skips_assignments_and_quotes() {
    assert_eq!(command_program("npm run typecheck").as_deref(), Some("npm"));
    assert_eq!(command_program("CI=1 cargo test").as_deref(), Some("cargo"));
    assert_eq!(command_program("\"./gradlew\" build").as_deref(), Some("./gradlew"));
    assert_eq!(
        command_program("cd shots && npx playwright test").as_deref(),
        Some("cd")
    );
    assert_eq!(command_program(""), None);
}

#[test]
fn shells_once_each_in_order_without_denied_or_launchers() {
    let checks = [
        check("npm run typecheck"),
        check("CI=1 cargo test"),
        check("bash scripts/ci.sh"),
        check("npm test"),
        check(""),
        check("/bin/bash x"),
        check("bash.exe x"),
        check("PowerShell -f x"),
        check("rm -rf x"),
        check("make shots"),
    ];
    assert_eq!(command_shells(&checks), ["Shell(npm)", "Shell(cargo)", "Shell(make)"]);
}

#[test]
fn refusals_have_their_reasons() {
    let every = |e: &str| Some(format!("{e} would allow every command"));
    assert_eq!(
        allow_entry_problem("Shell(*)").as_deref(),
        Some("Shell(*) would allow every command")
    );
    assert_eq!(allow_entry_problem("Shell(/bin/bash)"), every("Shell(/bin/bash)"));
    assert_eq!(allow_entry_problem("Shell(PowerShell)"), every("Shell(PowerShell)"));
    for w in ["Write(docs/../../etc)", "Write(/etc)", "Write(C:\\x)"] {
        assert_eq!(
            allow_entry_problem(w),
            Some(format!("{w} writes outside the workspace"))
        );
    }
    assert_eq!(allow_entry_problem("Shell(RM)").as_deref(), Some("RM is always denied"));
    assert_eq!(
        allow_entry_problem("Shell(/bin/rm)").as_deref(),
        Some("/bin/rm is always denied")
    );
    for e in ["Shell()", "Read()", "Write()"] {
        assert_eq!(
            allow_entry_problem(e),
            Some(format!("{e} isn't Shell(…), Read(…) or Write(…)"))
        );
    }
    assert_eq!(
        allow_entry_problem("make").as_deref(),
        Some("make isn't Shell(…), Read(…) or Write(…)")
    );
}

#[test]
fn valid_entries_pass() {
    for e in ["Shell(make)", "Read(docs/**)", "Write(src/**)"] {
        assert_eq!(allow_entry_problem(e), None);
    }
}

#[test]
fn entries_split_on_commas_and_lines() {
    assert_eq!(
        split_allow_entries("Shell(make), Read(a)\n\n Write(b) ,"),
        ["Shell(make)", "Read(a)", "Write(b)"]
    );
}

#[test]
fn quoted_runs_stay_one_word() {
    assert_eq!(command_program(r#"FOO="a b" cargo test"#).as_deref(), Some("cargo"));
    assert_eq!(command_program(r#""./my tool" build"#).as_deref(), Some("./my tool"));
    assert_eq!(command_program("1A=x make").as_deref(), Some("make"));
}

// with_project: the session's options from the project's config, as the lane's written .cursor/cli.json shows them

fn project(repo: &std::path::Path, remote: Option<&str>, engines: serde_json::Value) -> ProjectConfig {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
    };
    git(&["init"]);
    if let Some(url) = remote {
        git(&["remote", "add", "origin", url]);
    }
    serde_json::from_value(serde_json::json!({
        "repoPath": repo,
        "checks": [{"command": "npm run typecheck"}, {"command": "CI=1 cargo test"}],
        "engines": engines,
    }))
    .unwrap()
}

/// The `permissions.allow` of the `.cursor/cli.json` a session on `project` gets in a fresh lane.
fn written(project: &ProjectConfig, readonly: bool) -> Vec<String> {
    written_with(project, readonly, true)
}

fn written_with(project: &ProjectConfig, readonly: bool, on: bool) -> Vec<String> {
    use crate::engines::engine::Engine;
    let lane = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .arg("init")
        .current_dir(lane.path())
        .output()
        .unwrap();
    crate::tiers::set_cursor_allow_derive_for_test(on);
    let mut spec = super::super::base_spec(lane.path());
    spec.readonly = readonly;
    spec.cursor = Some(with_project(project, CursorOptions::default()));
    super::super::CursorEngine.prepare(&spec).unwrap();
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(lane.path().join(".cursor/cli.json")).unwrap()).unwrap();
    file["permissions"]["allow"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_writing_lane_gets_the_derived_programs_and_a_read_only_one_does_not() {
    let repo = tempfile::tempdir().unwrap();
    let p = project(repo.path(), Some("https://github.com/o/r.git"), serde_json::Value::Null);
    let w = written(&p, false);
    for want in ["Shell(npm)", "Shell(cargo)", "Shell(gh)"] {
        assert!(w.iter().any(|e| e == want), "{want}");
    }
    assert!(!w.iter().any(|e| e == "Shell(bash)"));
    let r = written(&p, true);
    assert!(r.iter().any(|e| e == "Shell(gh)"));
    assert!(!r.iter().any(|e| e == "Shell(npm)" || e == "Shell(cargo)"));
}

#[test]
fn the_host_picks_gh_glab_or_neither() {
    let repo = tempfile::tempdir().unwrap();
    let w = written(
        &project(repo.path(), Some("git@gitlab.com:o/r.git"), serde_json::Value::Null),
        false,
    );
    assert!(w.iter().any(|e| e == "Shell(glab)") && !w.iter().any(|e| e == "Shell(gh)"));
    let repo = tempfile::tempdir().unwrap();
    let w = written(&project(repo.path(), None, serde_json::Value::Null), false);
    assert!(!w.iter().any(|e| e == "Shell(glab)" || e == "Shell(gh)"));
}

#[test]
fn a_bad_typed_entry_is_left_out_and_a_good_one_written_once() {
    let repo = tempfile::tempdir().unwrap();
    let engines = serde_json::json!({"allow": ["Shell(*)", "Shell(make)", "Shell(make)", "Shell(npm)", 7]});
    let w = written(&project(repo.path(), None, engines), false);
    assert!(!w.iter().any(|e| e == "Shell(*)"));
    assert_eq!(w.iter().filter(|e| *e == "Shell(make)").count(), 1);
    assert_eq!(w.iter().filter(|e| *e == "Shell(npm)").count(), 1);
}

#[test]
fn a_config_change_between_sessions_shows_in_the_second() {
    let repo = tempfile::tempdir().unwrap();
    let mut p = project(repo.path(), None, serde_json::Value::Null);
    assert!(!written(&p, false).iter().any(|e| e == "Shell(make)"));
    p.engines = Some(serde_json::json!({"allow": ["Shell(make)"]}));
    assert!(written(&p, false).iter().any(|e| e == "Shell(make)"));
}

#[test]
fn with_the_flag_off_the_options_are_todays() {
    let repo = tempfile::tempdir().unwrap();
    let p = project(
        repo.path(),
        Some("git@gitlab.com:o/r.git"),
        serde_json::json!({"allow": ["Shell(make)"]}),
    );
    let w = written_with(&p, false, false);
    assert!(
        !w.iter()
            .any(|e| ["Shell(npm)", "Shell(cargo)", "Shell(make)", "Shell(glab)"].contains(&e.as_str()))
    );
    assert!(!w.iter().any(|e| e == "Shell(gh)"), "placed options carry no cli");
    // The implementer and trial sessions keep gh-by-repo.
    crate::tiers::set_cursor_allow_derive_for_test(false);
    let mut named = p.clone();
    named.github_repo = "o/r".into();
    assert_eq!(
        with_project_or_gh(&named, CursorOptions::default()).cli.as_deref(),
        Some("gh")
    );
    assert_eq!(with_project_or_gh(&p, CursorOptions::default()).cli, None);
    assert!(with_project_or_gh(&named, CursorOptions::default()).allow.is_empty());
}
