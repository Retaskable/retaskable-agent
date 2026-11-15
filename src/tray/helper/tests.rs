use super::*;

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"x").unwrap();
}

fn build() -> Build {
    Build {
        os: "linux-x64".into(),
        environment: crate::release::Environment::Production,
        version: "1.0.0".into(),
        commit: "abc".into(),
    }
}

#[test]
fn a_release_build_finds_the_helper_beside_the_executable() {
    for (os, name) in [
        (OsKind::Windows, "tray_windows_release.exe"),
        (OsKind::Mac, "tray_darwin_release"),
        (OsKind::Linux, "tray_linux_release"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("retaskable");
        touch(&dir.path().join(name));
        assert_eq!(
            helper_path(Some(&build()), &exe, &Env::empty(), os),
            Some(dir.path().join(name))
        );
    }
}

#[test]
fn a_mac_app_keeps_it_in_contents_resources() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("Retaskable.app/Contents/MacOS/retaskable");
    let helper = dir.path().join("Retaskable.app/Contents/Resources/tray_darwin_release");
    touch(&helper);
    assert_eq!(
        helper_path(Some(&build()), &exe, &Env::empty(), OsKind::Mac),
        Some(helper)
    );
}

#[test]
fn a_checkout_uses_the_env_override_then_the_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("retaskable");
    let own = dir.path().join("mine/helper");
    touch(&own);
    let env = Env::from_pairs([
        ("RETASKABLE_TRAY_HELPER", own.to_str().unwrap()),
        ("RETASKABLE_DATA_DIR", dir.path().join("data").to_str().unwrap()),
    ]);
    assert_eq!(helper_path(None, &exe, &env, OsKind::Linux), Some(own));

    let fetched = dir.path().join("data/tray/tray_linux_release");
    touch(&fetched);
    let env = Env::from_pairs([("RETASKABLE_DATA_DIR", dir.path().join("data").to_str().unwrap())]);
    assert_eq!(helper_path(None, &exe, &env, OsKind::Linux), Some(fetched));
}

#[test]
fn none_when_missing_or_without_a_helper_for_the_os() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("retaskable");
    let env = Env::from_pairs([("RETASKABLE_DATA_DIR", dir.path().to_str().unwrap())]);
    assert_eq!(helper_path(Some(&build()), &exe, &env, OsKind::Linux), None);
    assert_eq!(helper_path(None, &exe, &env, OsKind::Windows), None);
    touch(&dir.path().join("tray/anything"));
    assert_eq!(helper_path(None, &exe, &env, OsKind::Other), None);
}

fn tarball(name: &str, body: &[u8]) -> Vec<u8> {
    let mut tar_bytes = Vec::new();
    {
        let mut b = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        b.append_data(&mut header, format!("package/traybin/{name}"), body)
            .unwrap();
        b.finish().unwrap();
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
    gz.finish().unwrap()
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn unpacks_the_os_helper_when_the_hash_matches() {
    let tgz = tarball("tray_linux_release", b"ELF");
    assert_eq!(unpack_helper(&tgz, &sha(&tgz), OsKind::Linux).unwrap(), b"ELF");
    let err = unpack_helper(&tgz, &sha(&tgz), OsKind::Mac).unwrap_err();
    assert!(err.contains("tray_darwin_release"), "{err}");
}

#[test]
fn a_tarball_with_the_wrong_hash_is_refused() {
    let tgz = tarball("tray_linux_release", b"ELF");
    let err = unpack_helper(&tgz, SYSTRAY2_SHA256, OsKind::Linux).unwrap_err();
    assert!(err.contains("not the pinned"), "{err}");
}

#[cfg(unix)]
#[test]
fn the_helper_is_placed_executable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = place_helper(dir.path(), b"ELF", OsKind::Linux).unwrap();
    assert_eq!(path, dir.path().join("tray_linux_release"));
    assert_eq!(std::fs::read(&path).unwrap(), b"ELF");
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o755);
}

#[test]
fn an_os_without_a_helper_has_no_file_name_and_can_neither_unpack_nor_place_one() {
    assert_eq!(helper_file_name(OsKind::Other), None);
    let tgz = tarball("tray_linux_release", b"ELF");
    let err = unpack_helper(&tgz, &sha(&tgz), OsKind::Other).unwrap_err();
    assert!(err.contains("no tray helper"), "{err}");
    let dir = tempfile::tempdir().unwrap();
    let err = place_helper(dir.path(), b"ELF", OsKind::Other).unwrap_err();
    assert!(err.contains("no tray helper"), "{err}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing is written");
}

#[test]
fn the_pinned_hash_matches_whatever_its_letter_case() {
    let tgz = tarball("tray_linux_release", b"ELF");
    assert_eq!(
        unpack_helper(&tgz, &sha(&tgz).to_uppercase(), OsKind::Linux).unwrap(),
        b"ELF"
    );
}

#[test]
fn a_tarball_without_the_helper_is_refused_naming_where_it_looked() {
    let tgz = tarball("tray_darwin_release", b"MACH");
    let err = unpack_helper(&tgz, &sha(&tgz), OsKind::Linux).unwrap_err();
    assert!(err.contains("has no") && err.contains("tray_linux_release"), "{err}");
}

#[test]
fn bytes_that_are_not_a_tarball_are_refused_once_the_hash_matches() {
    let junk = b"not a tarball";
    let err = unpack_helper(junk, &sha(junk), OsKind::Linux).unwrap_err();
    assert!(err.contains("systray2 tarball"), "{err}");
}

#[test]
fn the_helper_comes_from_the_pinned_systray2_version() {
    assert_eq!(
        tarball_url(),
        format!("https://registry.npmjs.org/systray2/-/systray2-{SYSTRAY2_VERSION}.tgz")
    );
    assert_eq!(helper_file_name(OsKind::Windows), Some("tray_windows_release.exe"));
}
