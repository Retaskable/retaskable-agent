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
    assert!(desktop.contains("Exec=retaskable"), "{desktop}");
    assert!(desktop.contains("Name=Retaskable Staging"), "{desktop}");
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
