use super::*;

fn decode(png: &[u8]) -> image::DynamicImage {
    image::load_from_memory_with_format(png, image::ImageFormat::Png).unwrap()
}

#[test]
fn every_state_and_environment_is_a_32_pixel_png() {
    for environment in [Environment::Production, Environment::Staging] {
        for state in ICON_STATES {
            for os in [OsKind::Linux, OsKind::Mac] {
                let img = decode(&tray_icon(state, environment, os));
                assert_eq!((img.width(), img.height()), (32, 32), "{state:?} {environment:?}");
            }
        }
    }
}

#[test]
fn windows_wraps_the_png_in_an_ico() {
    for state in ICON_STATES {
        let png = tray_icon(state, Environment::Production, OsKind::Linux);
        let ico = tray_icon(state, Environment::Production, OsKind::Windows);
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 1, 0]);
        assert_eq!((ico[6], ico[7]), (32, 32));
        assert_eq!(u32::from_le_bytes(ico[14..18].try_into().unwrap()) as usize, png.len());
        let start = u32::from_le_bytes(ico[18..22].try_into().unwrap()) as usize;
        assert_eq!(&ico[start..], &png[..]);
    }
}

#[test]
fn staging_and_states_differ() {
    for state in ICON_STATES {
        assert_ne!(
            tray_icon(state, Environment::Staging, OsKind::Linux),
            tray_icon(state, Environment::Production, OsKind::Linux)
        );
    }
    let all: std::collections::HashSet<_> = ICON_STATES
        .iter()
        .map(|s| tray_icon(*s, Environment::Production, OsKind::Linux))
        .collect();
    assert_eq!(all.len(), 4);
}

#[test]
fn a_release_build_reads_icons_beside_it_and_a_checkout_uses_the_embedded() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("retaskable");
    let build = Build {
        os: "linux-x64".into(),
        environment: Environment::Production,
        version: "1.0.0".into(),
        commit: "abc".into(),
    };
    std::fs::create_dir(dir.path().join("icons")).unwrap();
    std::fs::write(dir.path().join("icons/connected.png"), b"custom").unwrap();
    assert_eq!(
        load_tray_icon(TrayState::Connected, Some(&build), &exe, OsKind::Linux),
        b"custom"
    );
    // A state with no file falls back to the embedded one; a checkout never reads the folder.
    assert_eq!(
        load_tray_icon(TrayState::Offline, Some(&build), &exe, OsKind::Linux),
        tray_icon(TrayState::Offline, Environment::Production, OsKind::Linux)
    );
    assert_ne!(
        load_tray_icon(TrayState::Connected, None, &exe, OsKind::Linux),
        b"custom"
    );
}

#[test]
fn bundle_dir_is_beside_the_exe_or_a_apps_resources() {
    use crate::release::bundle_dir;
    assert_eq!(bundle_dir(Path::new("/a/b/retaskable")), Path::new("/a/b"));
    assert_eq!(
        bundle_dir(Path::new("/x/Retaskable.app/Contents/MacOS/retaskable")),
        Path::new("/x/Retaskable.app/Contents/Resources")
    );
}
