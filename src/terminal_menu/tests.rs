use super::*;
use crate::tray::menu::tray_menu;
use crate::tray::state::{BacklogWait, CloudInput, LinkState, TrayInput};

fn cloud(name: &str, link: LinkState) -> CloudInput {
    let host = format!("{}.example", name.to_lowercase());
    CloudInput {
        name: name.into(),
        url: format!("https://{host}"),
        link_page: None,
        host,
        link,
    }
}

fn input() -> TrayInput {
    let mut i = crate::tray::state::tests::input(vec![
        cloud("Production", LinkState::Connected),
        cloud("Staging", LinkState::NotLinked),
    ]);
    i.machine = "pc4".into();
    i
}

fn menu() -> Menu {
    tray_menu(&input())
}

fn cursor(on: &str) -> Cursor {
    Cursor {
        path: vec![],
        on: Some(on.into()),
    }
}

fn screen(m: &Menu, c: &Cursor, w: usize, h: usize) -> Vec<String> {
    render_screen(m, c, w, h, None, &[])
}

fn plain(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            let mut out = String::new();
            let mut skip = false;
            for ch in l.chars() {
                match (skip, ch) {
                    (_, '\x1b') => skip = true,
                    (true, 'm') => skip = false,
                    (false, c) => out.push(c),
                    _ => {}
                }
            }
            out
        })
        .collect()
}

fn press(m: &Menu, c: &Cursor, k: Key) -> (Cursor, Option<Outcome>) {
    handle_key(m, c, k)
}

#[test]
fn header_tooltip_rules_and_hidden_items() {
    let m = menu();
    let lines = plain(&screen(&m, &cursor("pause"), 80, 40));
    assert!(lines[0].starts_with("Retaskable"));
    assert!(lines[1].contains("pc4"));
    assert!(lines.iter().any(|l| l.starts_with("───")));
    assert!(lines.iter().any(|l| l.contains("› Pause this computer")));
    assert!(lines.iter().any(|l| l.contains("Link to ▸")));
    // Hidden: release-only items, Waiting for you, the log, Quit when idle.
    for gone in [
        "Open the log",
        "Start at login",
        "Waiting for you",
        "Quit when idle",
        "<SEPARATOR>",
    ] {
        assert!(!lines.iter().any(|l| l.contains(gone)), "{gone}");
    }
}

#[test]
fn greyed_dimmed_and_ticked() {
    let mut i = input();
    i.is_release = true;
    i.start_at_login = true;
    let m = tray_menu(&i);
    let raw = screen(&m, &cursor("settings"), 80, 40);
    assert!(raw.iter().any(|l| l.contains("✓ Start at login")));
    let mut j = input();
    j.has_project = false;
    let raw = screen(&tray_menu(&j), &cursor("settings"), 80, 40);
    assert!(
        raw.iter()
            .any(|l| l.starts_with("\x1b[2m") && l.contains("Pause this computer"))
    );
}

#[test]
fn inline_list_is_a_submenu_and_backlog_is_highlighted() {
    let mut i = input();
    i.clouds[1].link = LinkState::Connected;
    i.backlog = Some(BacklogWait {
        host: "store.example".into(),
        link_page: Some("https://s/link".into()),
        unlinked: false,
    });
    let m = tray_menu(&i);
    let raw = screen(&m, &cursor("pause"), 80, 40);
    let find = |t: &str| raw.iter().find(|l| l.contains(t)).unwrap().clone();
    assert!(find("Waiting for you").starts_with("\x1b[1;33m"));
    assert!(find("Approve the shared backlog ▸").starts_with("\x1b[1;33m"));
    assert!(find("Open on retaskable.com ▸").contains("▸"));
    let notice = backlog_notice(&m).unwrap();
    assert!(notice.starts_with("! Retaskable: approve the shared backlog: "));
    let with = render_screen(&m, &cursor("pause"), 200, 40, Some(&notice), &[]);
    assert!(with[2].starts_with("\x1b[1;33m! Retaskable"));
    assert_eq!(backlog_notice(&menu()), None);
}

#[test]
fn submenu_opens_and_goes_back() {
    let m = menu();
    let (c, out) = press(&m, &cursor("link"), Key::Enter);
    assert_eq!(out, None);
    assert_eq!(c.path, vec!["link"]);
    assert_eq!(c.on.as_deref(), Some("link.0.do"));
    let lines = plain(&screen(&m, &c, 80, 40));
    assert!(lines.iter().any(|l| l == "‹ Link to"));
    assert!(!lines.iter().any(|l| l.contains("Settings")));
    assert!(lines.iter().any(|l| l.contains("› Forget Production")));
    let (c, _) = press(&m, &c, Key::Esc);
    assert_eq!(c, cursor("link"));
}

#[test]
fn keys_move_choose_and_back() {
    let m = menu();
    let (c, _) = press(&m, &Cursor::default(), Key::Home);
    let first = c.on.clone().unwrap();
    let (c2, _) = press(&m, &c, Key::Char('j'));
    assert_ne!(c2.on.unwrap(), first);
    let (c3, _) = press(&m, &c, Key::Char('k'));
    assert_eq!(c3.on.unwrap(), first);
    assert_eq!(press(&m, &c, Key::Up).0.on.unwrap(), first, "stops at the top");
    assert_eq!(press(&m, &c, Key::End).0.on.as_deref(), Some("quit"));
    assert_eq!(
        press(&m, &c, Key::End).0,
        press(&m, &press(&m, &c, Key::End).0, Key::Down).0
    );
    assert_eq!(press(&m, &cursor("quit"), Key::Home).0.on, Some(first));
    for k in [Key::Enter, Key::Char(' ')] {
        assert_eq!(press(&m, &cursor("pause"), k).1, Some(Outcome::Run("pause".into())));
    }
    // → and l only open a submenu.
    assert_eq!(press(&m, &cursor("pause"), Key::Right).1, None);
    assert_eq!(press(&m, &cursor("pause"), Key::Char('l')).0, cursor("pause"));
    assert_eq!(press(&m, &cursor("link"), Key::Right).0.path, vec!["link"]);
    assert_eq!(press(&m, &cursor("link"), Key::Char('l')).0.path, vec!["link"]);
    for k in [Key::Left, Key::Esc, Key::Char('h'), Key::Backspace] {
        let (open, _) = press(&m, &cursor("link"), Key::Enter);
        assert_eq!(press(&m, &open, k).0, cursor("link"));
        assert_eq!(
            press(&m, &cursor("pause"), k).0,
            cursor("pause"),
            "nothing to go back to"
        );
    }
    // A greyed-out item is skipped and never run.
    let mut i = input();
    i.has_project = false;
    let g = tray_menu(&i);
    assert_ne!(press(&g, &cursor("open"), Key::Down).0.on.as_deref(), Some("pause"));
    assert_eq!(press(&g, &cursor("pause"), Key::Enter).1, None);
}

#[test]
fn q_hints_and_ctrl_c_stops() {
    let m = menu();
    let (c, out) = press(&m, &cursor("pause"), Key::Char('q'));
    assert_eq!((c, out), (cursor("pause"), Some(Outcome::Hint)));
    assert!(QUIT_HINT.contains("Quit") && QUIT_HINT.contains("Ctrl+C"));
    assert_eq!(press(&m, &cursor("pause"), Key::CtrlC).1, Some(Outcome::Stop));
    assert_eq!(press(&m, &cursor("pause"), Key::Char('x')).1, None);
}

#[test]
fn lines_are_cut_with_an_ellipsis() {
    let m = menu();
    let lines = plain(&screen(&m, &cursor("pause"), 12, 40));
    assert!(lines.iter().all(|l| l.chars().count() <= 12));
    assert!(lines.iter().any(|l| l.ends_with('…')));
    assert_eq!(cut("abc", 3), "abc");
    assert_eq!(cut("abcd", 3), "ab…");
    assert_eq!(cut("abcd", 0), "");
}

#[test]
fn scrolls_to_keep_the_cursor_in_sight() {
    let m = menu();
    for on in ["pause", "quit"] {
        let lines = plain(&screen(&m, &cursor(on), 80, 6));
        assert_eq!(lines.len(), 6);
        assert!(lines.iter().any(|l| l.starts_with('›')), "{on}");
    }
    let first = plain(&screen(&m, &cursor("link"), 80, 6));
    assert!(first.iter().any(|l| l.contains("Link to")));
    assert!(screen(&m, &cursor("quit"), 80, 0).is_empty());
}

#[test]
fn log_lines_fill_the_rest() {
    let m = menu();
    let log: Vec<String> = (1..=50).map(|n| format!("line {n}")).collect();
    let out = plain(&render_screen(&m, &cursor("quit"), 80, 40, None, &log));
    assert_eq!(out.len(), 40);
    assert_eq!(out.last().unwrap(), "line 50");
    assert!(out.iter().any(|l| l == "line 49"));
    // No room under the menu: no log.
    let short = plain(&render_screen(&m, &cursor("quit"), 80, 8, None, &log));
    assert!(!short.iter().any(|l| l.starts_with("line")));
}

#[test]
fn cursor_stays_on_its_item_as_the_menu_changes() {
    let before = menu();
    let mut i = input();
    i.backlog = Some(BacklogWait {
        host: "s".into(),
        link_page: None,
        unlinked: false,
    });
    i.sessions = 2;
    let after = tray_menu(&i);
    let c = cursor("settings");
    let rows = |m: &Menu| {
        plain(&screen(m, &c, 80, 40))
            .into_iter()
            .position(|l| l.starts_with("› Settings"))
    };
    assert!(rows(&before).is_some() && rows(&after).is_some());
    assert_ne!(rows(&before), rows(&after));
    assert_eq!(press(&after, &c, Key::Down).0.on.as_deref(), Some("quit_when_idle"));
    // The item goes: the first choosable takes over. The open submenu goes: back to the list.
    let gone = Cursor {
        path: vec!["open.many".into()],
        on: Some("open.0".into()),
    };
    assert_eq!(fit(&before, &gone).0.path, Vec::<String>::new());
    assert_eq!(fit(&before, &cursor("nope")).0.on.as_deref(), Some("link"));
}
