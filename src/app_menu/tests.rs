use std::path::Path;

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
fn only_an_appimage_has_an_entry_production_and_staging_apart() {
    let appimage = "/home/a/Apps/Retaskable-Staging \"x\" 100%.AppImage";
    let exec_path = Path::new("/opt/retaskable/retaskable");

    let staging = menu_entry(
        &build(Environment::Staging),
        Some(appimage),
        Some("/tmp/.mount_x"),
        None,
        "/home/a",
        exec_path,
    )
    .unwrap();
    assert_eq!(
        staging.file,
        PathBuf::from("/home/a/.local/share/applications/retaskable-staging.desktop")
    );
    assert!(staging.text.starts_with("[Desktop Entry]\n"));
    assert!(staging.text.contains("\nName=Retaskable Staging\n"));
    assert!(
        staging
            .text
            .contains("\nExec=\"/home/a/Apps/Retaskable-Staging \\\"x\\\" 100%%.AppImage\"\n"),
        "the AppImage, not its mount, quoted: {}",
        staging.text
    );
    assert!(staging.text.contains("\nIcon=retaskable-staging\n"));
    assert!(staging.text.contains("\nTerminal=false\n"));
    assert_eq!(staging.icon_from, PathBuf::from("/tmp/.mount_x/retaskable-staging.png"));
    assert_eq!(
        staging.icon_to,
        PathBuf::from("/home/a/.local/share/icons/hicolor/256x256/apps/retaskable-staging.png")
    );

    let production = menu_entry(
        &build(Environment::Production),
        Some(appimage),
        Some("/tmp/.mount_x"),
        Some("/data"),
        "/home/a",
        exec_path,
    )
    .unwrap();
    assert_eq!(production.file, PathBuf::from("/data/applications/retaskable.desktop"));
    assert!(production.text.contains("\nName=Retaskable\n"));

    assert!(
        menu_entry(
            &build(Environment::Staging),
            None,
            Some("/tmp/.mount_x"),
            None,
            "/home/a",
            exec_path
        )
        .is_none(),
        "a loose folder"
    );
}

#[test]
fn icon_falls_back_to_beside_the_executable_without_appdir() {
    let entry = menu_entry(
        &build(Environment::Staging),
        Some("/home/a/Apps/Retaskable-Staging.AppImage"),
        None,
        None,
        "/home/a",
        Path::new("/opt/retaskable/retaskable"),
    )
    .unwrap();
    assert_eq!(entry.icon_from, PathBuf::from("/opt/retaskable/retaskable-staging.png"));
}

#[test]
fn added_on_the_first_run_with_its_icon_written_again_while_there_and_left_out_once_removed() {
    let dir = tempfile::tempdir().unwrap();
    let mount = dir.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    std::fs::write(mount.join("retaskable-staging.png"), "png").unwrap();
    let data = dir.path().join("data");
    let entry = MenuEntry {
        file: dir
            .path()
            .join("share")
            .join("applications")
            .join("retaskable-staging.desktop"),
        text: "first".to_string(),
        icon_from: mount.join("retaskable-staging.png"),
        icon_to: dir.path().join("share").join("icons").join("retaskable-staging.png"),
    };

    assert!(add_to_app_menu(&entry, &data).unwrap());
    assert_eq!(std::fs::read_to_string(&entry.file).unwrap(), "first");
    assert_eq!(std::fs::read_to_string(&entry.icon_to).unwrap(), "png");
    assert!(data.join("app-menu").exists());

    let moved = MenuEntry {
        file: entry.file.clone(),
        text: "moved".to_string(),
        icon_from: entry.icon_from.clone(),
        icon_to: entry.icon_to.clone(),
    };
    assert!(
        add_to_app_menu(&moved, &data).unwrap(),
        "names where the AppImage is now"
    );
    assert_eq!(std::fs::read_to_string(&entry.file).unwrap(), "moved");

    std::fs::remove_file(&entry.file).unwrap();
    assert!(
        !add_to_app_menu(&entry, &data).unwrap(),
        "removed by the user, so it stays removed"
    );
    assert!(!entry.file.exists());

    let no_icon = tempfile::tempdir().unwrap();
    let bare = MenuEntry {
        file: no_icon.path().join("a.desktop"),
        text: "x".to_string(),
        icon_from: no_icon.path().join("none.png"),
        icon_to: no_icon.path().join("icon.png"),
    };
    assert!(
        add_to_app_menu(&bare, &no_icon.path().join("data")).unwrap(),
        "an entry without its icon still goes in"
    );
    assert!(!bare.icon_to.exists());
}
