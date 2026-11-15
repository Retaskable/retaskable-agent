use super::*;

fn mode(args: &[&str], env: &[(&str, &str)], os: OsKind, helper: bool) -> TrayMode {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    tray_mode(
        &args,
        &Env::from_pairs(env.iter().copied()),
        os,
        helper.then(|| "/h/tray".into()),
    )
}

fn headless(m: TrayMode) -> bool {
    matches!(m, TrayMode::Headless(_))
}

#[test]
fn a_desktop_with_a_helper_gets_the_tray() {
    assert_eq!(
        mode(&[], &[("DISPLAY", ":0")], OsKind::Linux, true),
        TrayMode::Tray("/h/tray".into())
    );
    assert_eq!(mode(&[], &[], OsKind::Windows, true), TrayMode::Tray("/h/tray".into()));
    assert_eq!(mode(&[], &[], OsKind::Mac, true), TrayMode::Tray("/h/tray".into()));
}

#[test]
fn no_tray_flag_and_variable_run_headless() {
    assert!(headless(mode(&["--no-tray"], &[], OsKind::Windows, true)));
    assert!(headless(mode(
        &[],
        &[("RETASKABLE_NO_TRAY", "1")],
        OsKind::Windows,
        true
    )));
    assert!(!headless(mode(
        &[],
        &[("RETASKABLE_NO_TRAY", "0")],
        OsKind::Windows,
        true
    )));
}

#[test]
fn linux_needs_a_display() {
    assert!(headless(mode(&[], &[], OsKind::Linux, true)));
    assert!(!headless(mode(
        &[],
        &[("WAYLAND_DISPLAY", "wayland-0")],
        OsKind::Linux,
        true
    )));
    assert!(!headless(mode(&[], &[("DISPLAY", ":0")], OsKind::Linux, true)));
}

#[test]
fn no_helper_runs_headless() {
    assert!(headless(mode(&[], &[("DISPLAY", ":0")], OsKind::Linux, false)));
    assert!(headless(mode(&[], &[], OsKind::Other, false)));
}
