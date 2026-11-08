use super::*;
use UiMode::*;

fn mode(args: &[&str], env: &[(&str, &str)], os: &str, helper: bool, tty: bool) -> UiMode {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    ui_mode(&args, &Env::from_pairs(env.iter().copied()), os, helper, tty)
}

fn reason(args: &[&str], env: &[(&str, &str)], os: &str, helper: bool) -> Option<String> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    no_tray_reason(&args, &Env::from_pairs(env.iter().copied()), os, helper)
}

#[test]
fn the_default_is_the_tray() {
    assert_eq!(mode(&[], &[], "windows", true, true), Tray);
    assert_eq!(mode(&[], &[], "macos", true, false), Tray);
    assert_eq!(mode(&[], &[("DISPLAY", ":0")], "linux", true, true), Tray);
    assert_eq!(mode(&[], &[("WAYLAND_DISPLAY", "w0")], "linux", true, false), Tray);
}

#[test]
fn tui_asks_for_the_menu_only_with_a_terminal() {
    assert_eq!(mode(&["--tui"], &[], "windows", true, true), TerminalMenu);
    assert_eq!(mode(&["--tui"], &[], "windows", true, false), Console);
    assert_eq!(mode(&[], &[("RETASKABLE_TUI", "1")], "macos", true, true), TerminalMenu);
    assert_eq!(mode(&[], &[("RETASKABLE_TUI", "1")], "macos", true, false), Console);
}

#[test]
fn no_tui_beats_everything() {
    for env in [&[][..], &[("RETASKABLE_TUI", "1")][..]] {
        assert_eq!(
            mode(&["--no-tui", "--tui", "--no-tray"], env, "linux", false, true),
            Console
        );
    }
    assert_eq!(
        mode(
            &["--tui", "--no-tray"],
            &[("RETASKABLE_TUI", "0")],
            "windows",
            true,
            true
        ),
        Console
    );
    assert_eq!(
        mode(
            &[],
            &[("RETASKABLE_TUI", "0"), ("RETASKABLE_NO_TRAY", "1")],
            "windows",
            true,
            true
        ),
        Console
    );
}

#[test]
fn no_tray_from_the_start_draws_the_menu_in_a_terminal() {
    assert_eq!(mode(&["--no-tray"], &[], "windows", true, true), TerminalMenu);
    assert_eq!(mode(&["--no-tray"], &[], "windows", true, false), Console);
    assert_eq!(
        mode(&[], &[("RETASKABLE_NO_TRAY", "1")], "macos", true, true),
        TerminalMenu
    );
    assert_eq!(mode(&[], &[("RETASKABLE_NO_TRAY", "1")], "macos", true, false), Console);
    assert_eq!(mode(&[], &[("RETASKABLE_NO_TRAY", "0")], "macos", true, true), Tray);
}

#[test]
fn a_linux_with_no_display_has_no_tray() {
    assert_eq!(mode(&[], &[], "linux", true, true), TerminalMenu);
    assert_eq!(mode(&[], &[], "linux", true, false), Console);
    assert_eq!(mode(&[], &[("DISPLAY", ":0")], "linux", true, true), Tray);
    assert_eq!(mode(&[], &[("WAYLAND_DISPLAY", "w0")], "linux", true, true), Tray);
}

#[test]
fn windows_and_macos_ignore_display() {
    assert_eq!(mode(&[], &[], "windows", true, true), Tray);
    assert_eq!(mode(&[], &[], "macos", true, true), Tray);
}

#[test]
fn no_helper_means_no_tray() {
    assert_eq!(mode(&[], &[], "windows", false, true), TerminalMenu);
    assert_eq!(mode(&[], &[], "windows", false, false), Console);
}

#[test]
fn output_that_is_not_a_terminal_never_gives_the_menu() {
    for args in [&[][..], &["--tui"][..], &["--no-tray"][..]] {
        for os in ["linux", "windows", "macos"] {
            for helper in [true, false] {
                assert_ne!(mode(args, &[], os, helper, false), TerminalMenu);
            }
        }
    }
}

#[test]
fn the_reason_says_why_there_is_no_tray() {
    assert_eq!(
        reason(&["--no-tray"], &[], "windows", true).unwrap(),
        "No tray: --no-tray"
    );
    assert_eq!(
        reason(&[], &[("RETASKABLE_NO_TRAY", "1")], "windows", true).unwrap(),
        "No tray: RETASKABLE_NO_TRAY is set"
    );
    assert_eq!(
        reason(&[], &[], "linux", true).unwrap(),
        "No tray: no DISPLAY or WAYLAND_DISPLAY"
    );
    assert_eq!(
        reason(&[], &[], "windows", false).unwrap(),
        "No tray: no tray helper for this OS"
    );
    assert_eq!(reason(&[], &[], "windows", true), None);
}
