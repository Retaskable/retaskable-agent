use super::*;
use crate::release::Environment;

fn build(environment: Environment) -> Build {
    Build {
        os: "linux-x64".to_string(),
        environment,
        version: "1.4.2".to_string(),
        commit: "abc1234".to_string(),
    }
}

#[test]
fn desktop_exec_escapes_as_the_desktop_entry_spec_asks() {
    assert_eq!(desktop_exec("/tmp/.mount_x/retaskable"), "\"/tmp/.mount_x/retaskable\"");
    assert_eq!(
        desktop_exec("/home/a/Apps/Retaskable-Staging \"x\" 100%.AppImage"),
        "\"/home/a/Apps/Retaskable-Staging \\\"x\\\" 100%%.AppImage\""
    );
}

#[test]
fn login_entry_names_production_and_staging_apart() {
    let (file, text) = login_entry(
        &build(Environment::Staging),
        None,
        None,
        "/home/a",
        "/tmp/.mount_x/retaskable",
    );
    assert_eq!(
        file,
        PathBuf::from("/home/a/.config/autostart/retaskable-staging.desktop")
    );
    assert!(text.starts_with("[Desktop Entry]\n"));
    assert!(text.contains("\nName=Retaskable Staging\n"));
    assert!(text.contains("\nTerminal=false\n"));
    assert!(
        text.contains("\nExec=\"/tmp/.mount_x/retaskable\"\n"),
        "a loose folder: its executable, {text}"
    );

    let (file, text) = login_entry(
        &build(Environment::Production),
        Some("/cfg"),
        None,
        "/home/a",
        "/tmp/.mount_x/retaskable",
    );
    assert_eq!(file, PathBuf::from("/cfg/autostart/retaskable.desktop"));
    assert!(text.contains("\nName=Retaskable\n"));
}

#[test]
fn login_entry_names_the_appimage_not_its_mount() {
    let (_, text) = login_entry(
        &build(Environment::Staging),
        None,
        Some("/home/a/Apps/Retaskable-Staging \"x\" 100%.AppImage"),
        "/home/a",
        "/tmp/.mount_x/retaskable",
    );
    assert!(
        text.contains("\nExec=\"/home/a/Apps/Retaskable-Staging \\\"x\\\" 100%%.AppImage\"\n"),
        "the AppImage, not its mount, quoted: {text}"
    );
}

#[test]
fn a_file_entry_is_added_and_removed_by_the_tick_and_on_by_default_on_the_first_run_only() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("autostart").join("retaskable-staging.desktop");
    let data = dir.path().join("data");

    let login = StartAtLogin::new(file.clone(), "[Desktop Entry]\n".to_string());
    login.at_start(&data).unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "[Desktop Entry]\n",
        "first run: on"
    );
    assert!(login.is_on());

    login.set(false).unwrap();
    assert!(!file.exists());
    assert!(!login.is_on());

    StartAtLogin::new(file.clone(), "x".to_string())
        .at_start(&data)
        .unwrap();
    assert!(!file.exists(), "unticked stays unticked after a restart");

    login.set(true).unwrap();
    let moved = StartAtLogin::new(file.clone(), "moved\n".to_string());
    moved.at_start(&data).unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "moved\n",
        "on: written again with where this copy is now"
    );
}
