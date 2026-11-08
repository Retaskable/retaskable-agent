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

fn tui(args: &[&str], env: &[(&str, &str)], os: OsKind, terminals: bool) -> bool {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    use_tui(&args, &Env::from_pairs(env.iter().copied()), os, terminals)
}

#[test]
fn the_terminal_menu_is_asked_for_by_flag_or_variable() {
    let desktop = [("DISPLAY", ":0")];
    assert!(tui(&["--tui"], &desktop, OsKind::Linux, true));
    assert!(tui(
        &[],
        &[("DISPLAY", ":0"), ("RETASKABLE_TUI", "1")],
        OsKind::Linux,
        true
    ));
    assert!(!tui(&[], &desktop, OsKind::Linux, true), "a desktop gets the tray");
}

#[test]
fn no_tui_flag_and_variable_keep_the_plain_console() {
    assert!(!tui(&["--no-tui"], &[], OsKind::Linux, true));
    assert!(!tui(&["--no-tui", "--tui"], &[], OsKind::Windows, true));
    assert!(!tui(&[], &[("RETASKABLE_TUI", "0")], OsKind::Linux, true));
    assert!(!tui(&["--no-tray"], &[("RETASKABLE_TUI", "0")], OsKind::Windows, true));
}

#[test]
fn no_tray_from_the_start_draws_it_only_with_terminals() {
    assert!(tui(&[], &[], OsKind::Linux, true), "no display");
    assert!(tui(&["--no-tray"], &[], OsKind::Windows, true));
    assert!(tui(&[], &[("RETASKABLE_NO_TRAY", "1")], OsKind::Mac, true));
    assert!(!tui(&[], &[], OsKind::Linux, false));
    assert!(!tui(&["--tui"], &[], OsKind::Linux, false));
    assert!(!tui(&["--no-tray"], &[], OsKind::Windows, false));
}

#[test]
fn a_missing_helper_alone_does_not_draw_it() {
    // The helper is looked for after this decision; a failure later leaves the plain console.
    assert!(!tui(&[], &[], OsKind::Windows, true));
}
