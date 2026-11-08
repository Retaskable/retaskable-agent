use super::*;
use crate::release::Environment;

fn build(os: &str, environment: Environment) -> Build {
    Build {
        os: os.to_string(),
        environment,
        version: "1.4.2".to_string(),
        commit: "abc1234".to_string(),
    }
}

#[test]
fn package_name_is_the_appimage_for_linux() {
    assert_eq!(
        package_name(&build("linux-x64", Environment::Production)).unwrap(),
        "Retaskable-x86_64.AppImage"
    );
    assert_eq!(
        package_name(&build("linux-x64", Environment::Staging)).unwrap(),
        "Retaskable-Staging-x86_64.AppImage"
    );
}

#[test]
fn package_name_refuses_anything_but_linux() {
    for os in ["win-x64", "mac-arm64", "mac-x64"] {
        let err = package_name(&build(os, Environment::Production))
            .unwrap_err()
            .to_string();
        assert!(err.contains(os), "{err}");
    }
}

#[test]
fn copy_dir_copies_nested_files() {
    let from = tempfile::tempdir().unwrap();
    std::fs::write(from.path().join("retaskable"), b"exe").unwrap();
    std::fs::create_dir(from.path().join("sub")).unwrap();
    std::fs::write(from.path().join("sub").join("build.json"), b"{}").unwrap();

    let to = tempfile::tempdir().unwrap();
    copy_dir(from.path(), to.path()).unwrap();

    assert_eq!(std::fs::read(to.path().join("retaskable")).unwrap(), b"exe");
    assert_eq!(std::fs::read(to.path().join("sub").join("build.json")).unwrap(), b"{}");
}

#[test]
fn cached_runtime_returns_a_cache_hit_whose_hash_matches_with_no_download() {
    let cache = tempfile::tempdir().unwrap();
    let sha256 = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    std::fs::write(
        cache.path().join(format!("appimage-runtime-{}", &sha256[..12])),
        b"hello",
    )
    .unwrap();

    let data = cached_runtime_for(cache.path(), "http://example.invalid/runtime", sha256).unwrap();
    assert_eq!(data, b"hello");
}

#[test]
fn cached_runtime_refuses_a_cache_hit_whose_hash_does_not_match() {
    let cache = tempfile::tempdir().unwrap();
    let sha256 = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    std::fs::write(
        cache.path().join(format!("appimage-runtime-{}", &sha256[..12])),
        b"wrong-bytes",
    )
    .unwrap();

    let err = cached_runtime_for(cache.path(), "http://example.invalid/runtime", sha256)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("7c1d387f892b3c965dfc1951e2a92a2149cd103cef25c8ba5d0cc30a3a21063"),
        "{err}"
    );
    assert!(err.contains(sha256), "{err}");
}

#[cfg(unix)]
#[test]
fn build_app_dir_makes_an_app_run_symlink_and_a_desktop_entry() {
    let bundle = tempfile::tempdir().unwrap();
    std::fs::write(bundle.path().join("retaskable"), b"exe").unwrap();
    std::fs::write(bundle.path().join("build.json"), b"{}").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let app_dir = dir.path().join("AppDir");
    let build = build("linux-x64", Environment::Staging);
    build_app_dir(bundle.path(), &build, &app_dir).unwrap();

    assert_eq!(std::fs::read(app_dir.join("retaskable")).unwrap(), b"exe");
    assert_eq!(
        std::fs::read_link(app_dir.join("AppRun")).unwrap(),
        Path::new("retaskable")
    );
    let desktop = std::fs::read_to_string(app_dir.join("retaskable-staging.desktop")).unwrap();
    assert_eq!(desktop, desktop_entry(&build, "retaskable-staging", "retaskable"));
    assert!(desktop.contains("Terminal=false"), "{desktop}");
    assert!(desktop.contains("Icon=retaskable-staging\n"), "{desktop}");
    assert!(desktop.contains("Name=Retaskable Staging"), "{desktop}");
    assert_eq!(
        std::fs::read(app_dir.join("retaskable-staging.png")).unwrap(),
        include_bytes!("../../assets/retaskable-staging.png")
    );
    assert_eq!(
        std::fs::read_link(app_dir.join(".DirIcon")).unwrap(),
        Path::new("retaskable-staging.png")
    );
    let mut names: Vec<String> = std::fs::read_dir(&app_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            ".DirIcon",
            "AppRun",
            "build.json",
            "retaskable",
            "retaskable-staging.desktop",
            "retaskable-staging.png"
        ]
    );
}

#[test]
fn production_has_its_own_desktop_file_and_icon() {
    let build = build("linux-x64", Environment::Production);
    assert_eq!(app_id(&build), "retaskable");
    assert_eq!(icon_bytes(&build), include_bytes!("../../assets/retaskable.png"));
    assert_ne!(
        icon_bytes(&build),
        icon_bytes(&self::build("linux-x64", Environment::Staging))
    );
}

#[test]
fn the_icons_are_256_pixel_pngs() {
    for environment in [Environment::Production, Environment::Staging] {
        let png = icon_bytes(&build("linux-x64", environment));
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[16..24], &[0, 0, 1, 0, 0, 0, 1, 0]);
    }
}

#[test]
fn the_squashfs_is_made_when_mksquashfs_is_installed() {
    if Command::new("mksquashfs").arg("-version").output().is_err() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("retaskable"), b"exe").unwrap();
    let out = tempfile::tempdir().unwrap();
    let squashfs = out.path().join("app.squashfs");
    make_squashfs(dir.path(), &squashfs).unwrap();
    assert_eq!(&std::fs::read(&squashfs).unwrap()[..4], b"hsqs");
}

#[cfg(not(unix))]
#[test]
fn build_app_dir_refuses_to_run_on_windows() {
    let bundle = tempfile::tempdir().unwrap();
    std::fs::write(bundle.path().join("retaskable"), b"exe").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let build = build("linux-x64", Environment::Staging);
    let err = build_app_dir(bundle.path(), &build, &dir.path().join("AppDir"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("Linux or macOS"), "{err}");
}

#[test]
fn write_icons_puts_the_four_states_in_icons() {
    for (os, ext) in [("win-x64", "ico"), ("linux-x64", "png"), ("mac-arm64", "png")] {
        let dir = tempfile::tempdir().unwrap();
        write_icons(dir.path(), &build(os, Environment::Staging)).unwrap();
        for state in ["not_linked", "waiting", "offline", "connected"] {
            assert!(
                dir.path().join("icons").join(format!("{state}.{ext}")).is_file(),
                "{os} {state}"
            );
        }
    }
}
