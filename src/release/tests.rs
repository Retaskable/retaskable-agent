use std::io::Write;

use super::*;

fn exe_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("retaskable")
}

#[test]
fn a_checkout_has_no_build_json() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(load_build(&exe_path(dir.path())), Ok(None));
}

#[test]
fn a_release_build_s_build_json_is_read_beside_the_executable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("build.json"),
        r#"{"os": "win-x64", "environment": "staging", "version": "1.4.2", "commit": "abc1234"}"#,
    )
    .unwrap();
    assert_eq!(
        load_build(&exe_path(dir.path())),
        Ok(Some(Build {
            os: "win-x64".into(),
            environment: Environment::Staging,
            version: "1.4.2".into(),
            commit: "abc1234".into(),
        }))
    );
}

#[test]
fn a_build_json_with_an_unknown_environment_is_a_problem_naming_the_executable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("build.json"),
        r#"{"os": "win-x64", "environment": "electron", "version": "1.4.2", "commit": "abc1234"}"#,
    )
    .unwrap();
    let err = load_build(&exe_path(dir.path())).unwrap_err();
    assert!(err.contains(&exe_path(dir.path()).display().to_string()), "{err}");
    assert!(err.starts_with("its build.json, beside "), "{err}");
}

#[test]
fn a_build_json_missing_a_field_is_a_problem() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("build.json"), r#"{"os": "win-x64"}"#).unwrap();
    assert!(load_build(&exe_path(dir.path())).is_err());
}

#[test]
fn a_build_json_that_isn_t_json_is_a_problem() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("build.json"), "not json").unwrap();
    assert!(load_build(&exe_path(dir.path())).is_err());
}

fn build(environment: Environment) -> Build {
    Build {
        os: "win-x64".into(),
        environment,
        version: "1.4.2".into(),
        commit: "abc1234".into(),
    }
}

#[test]
#[cfg(windows)]
fn windows_keeps_data_and_logs_under_local_app_data() {
    let env = |k: &str| (k == "LOCALAPPDATA").then(|| "C:/Users/j/AppData/Local".to_string());
    assert_eq!(
        data_dir(&build(Environment::Production), env),
        std::path::PathBuf::from("C:/Users/j/AppData/Local/Retaskable")
    );
    assert_eq!(
        logs_dir(&build(Environment::Staging), env),
        std::path::PathBuf::from("C:/Users/j/AppData/Local/Retaskable Staging/logs")
    );
}

#[test]
#[cfg(target_os = "macos")]
fn macos_keeps_data_under_application_support_and_logs_under_logs() {
    let env = |k: &str| (k == "HOME").then(|| "/Users/j".to_string());
    assert_eq!(
        data_dir(&build(Environment::Production), env),
        std::path::PathBuf::from("/Users/j/Library/Application Support/Retaskable")
    );
    assert_eq!(
        logs_dir(&build(Environment::Staging), env),
        std::path::PathBuf::from("/Users/j/Library/Logs/Retaskable Staging")
    );
}

#[test]
#[cfg(target_os = "linux")]
fn linux_uses_xdg_dirs_or_falls_back_under_home() {
    let xdg = |k: &str| match k {
        "XDG_DATA_HOME" => Some("/x/data".to_string()),
        "XDG_STATE_HOME" => Some("/x/state".to_string()),
        _ => None,
    };
    assert_eq!(
        data_dir(&build(Environment::Staging), xdg),
        std::path::PathBuf::from("/x/data/retaskable-staging")
    );
    assert_eq!(
        logs_dir(&build(Environment::Production), xdg),
        std::path::PathBuf::from("/x/state/retaskable/logs")
    );
    let home = |k: &str| (k == "HOME").then(|| "/home/j".to_string());
    assert_eq!(
        data_dir(&build(Environment::Production), home),
        std::path::PathBuf::from("/home/j/.local/share/retaskable")
    );
    assert_eq!(
        logs_dir(&build(Environment::Production), home),
        std::path::PathBuf::from("/home/j/.local/state/retaskable/logs")
    );
}

#[test]
fn the_build_line_names_the_build() {
    assert_eq!(
        build_line(&build(Environment::Staging)),
        "Retaskable Staging 1.4.2 (abc1234) on win-x64"
    );
    assert_eq!(
        build_line(&build(Environment::Production)),
        "Retaskable 1.4.2 (abc1234) on win-x64"
    );
}

#[test]
fn a_rotating_log_appends_across_opens() {
    let dir = tempfile::tempdir().unwrap();
    let log = RotatingLog::open(dir.path()).unwrap();
    log.make_writer().write_all(b"one\n").unwrap();
    drop(log);
    let log = RotatingLog::open(dir.path()).unwrap();
    log.make_writer().write_all(b"two\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("retaskable.log")).unwrap(),
        "one\ntwo\n"
    );
}

#[test]
fn a_rotating_log_rotates_past_its_limit_and_keeps_three_old_ones() {
    let dir = tempfile::tempdir().unwrap();
    let log = RotatingLog::open_with_max(dir.path(), 10).unwrap();
    for n in 0..5 {
        log.make_writer()
            .write_all(format!("{n}123456789\n").as_bytes())
            .unwrap();
    }
    let read = |n: u32| std::fs::read_to_string(numbered(&dir.path().join("retaskable.log"), n)).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("retaskable.log")).unwrap(),
        "4123456789\n"
    );
    assert_eq!(read(1), "3123456789\n");
    assert_eq!(read(2), "2123456789\n");
    assert_eq!(read(3), "1123456789\n");
    assert!(!numbered(&dir.path().join("retaskable.log"), 4).exists());
}

#[test]
fn target_slug_names_the_four_built_platforms_and_nothing_else() {
    assert_eq!(target_slug("windows", "x86_64"), Some("win-x64"));
    assert_eq!(target_slug("macos", "aarch64"), Some("mac-arm64"));
    assert_eq!(target_slug("macos", "x86_64"), Some("mac-x64"));
    assert_eq!(target_slug("linux", "x86_64"), Some("linux-x64"));
    assert_eq!(target_slug("linux", "aarch64"), None);
    assert_eq!(target_slug("freebsd", "x86_64"), None);
}

#[test]
fn target_triple_names_a_cargo_target_for_each_slug_and_nothing_else() {
    assert_eq!(target_triple("win-x64"), Some("x86_64-pc-windows-msvc"));
    assert_eq!(target_triple("mac-arm64"), Some("aarch64-apple-darwin"));
    assert_eq!(target_triple("mac-x64"), Some("x86_64-apple-darwin"));
    assert_eq!(target_triple("linux-x64"), Some("x86_64-unknown-linux-gnu"));
    assert_eq!(target_triple("linux-arm64"), None);
}
