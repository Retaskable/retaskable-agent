use std::sync::atomic::Ordering;

use super::*;
use crate::terminal_menu::console::ConsoleWriter;
use crate::tray::agent::tests::rig;
use std::io::Write as _;

#[derive(Default)]
struct FakeTerm {
    out: Mutex<Vec<String>>,
    entered: Mutex<usize>,
    left: Mutex<usize>,
    size: Mutex<(usize, usize)>,
}

impl FakeTerm {
    fn new() -> Arc<Self> {
        let t = Self::default();
        *t.size.lock().unwrap() = (60, 20);
        Arc::new(t)
    }
    fn text(&self) -> String {
        self.out.lock().unwrap().join("")
    }
}

impl Term for FakeTerm {
    fn enter(&self) -> io::Result<()> {
        *self.entered.lock().unwrap() += 1;
        Ok(())
    }
    fn leave(&self) {
        *self.left.lock().unwrap() += 1;
    }
    fn size(&self) -> (usize, usize) {
        *self.size.lock().unwrap()
    }
    fn write(&self, s: &str) {
        self.out.lock().unwrap().push(s.to_string());
    }
}

struct Setup {
    term: Arc<FakeTerm>,
    log: Arc<ConsoleCapture>,
    menu: Arc<TerminalMenu>,
    rig: crate::tray::agent::tests::Rig,
}

fn setup() -> Setup {
    let rig = rig(false);
    let (term, log) = (FakeTerm::new(), Arc::new(ConsoleCapture::new()));
    let menu = TerminalMenu::start(term.clone(), log.clone(), rig.quit.clone()).unwrap();
    menu.attach(&rig.agent);
    let _ = rig.view.forward.set(menu.clone());
    menu.draw(&rig.agent.frame()).unwrap();
    Setup { term, log, menu, rig }
}

fn press(s: &Setup, keys: &[Key]) {
    for k in keys {
        s.menu.key(*k);
    }
}

#[test]
fn starting_takes_the_alternate_screen_and_draws_the_menu() {
    let s = setup();
    let text = s.term.text();
    assert!(text.starts_with(ENTER));
    assert!(text.contains("Pause this computer"));
    assert_eq!(*s.term.entered.lock().unwrap(), 1);
}

#[test]
fn a_choice_runs_the_trays_handler() {
    let s = setup();
    let paused = || {
        s.rig
            .agent_view
            .projects
            .iter()
            .all(|p| s.rig.agent_view.paused.reason(&p.id).is_some())
    };
    // Move down to Pause this computer, then choose it.
    let pause = loop {
        let before = s.term.text().len();
        press(&s, &[Key::Down]);
        if s.term.text()[before..].contains("› Pause this computer") {
            break true;
        }
        if s.term.text()[before..].contains("› Quit") {
            break false;
        }
    };
    assert!(pause);
    press(&s, &[Key::Enter]);
    assert!(paused());
}

#[test]
fn choosing_quit_quits_now_and_puts_the_terminal_back() {
    let s = setup();
    press(&s, &[Key::End, Key::Enter]);
    assert_eq!(s.rig.quit.now.load(Ordering::SeqCst), 1);
    assert_eq!(*s.term.left.lock().unwrap(), 1);
    assert_eq!(s.term.text().matches(LEAVE).count(), 1);
}

#[test]
fn q_says_how_to_stop_and_stops_nothing() {
    let s = setup();
    press(&s, &[Key::Char('q')]);
    assert!(s.term.text().contains("Choose Quit or Quit when idle"));
    assert_eq!(s.rig.quit.now.load(Ordering::SeqCst), 0);
    let before = s.term.text().len();
    press(&s, &[Key::Down]);
    assert!(
        !s.term.text()[before..].contains("Choose Quit"),
        "gone with the next key"
    );
}

#[test]
fn ctrl_c_restores_once_then_stops_as_sigint_does() {
    let s = setup();
    press(&s, &[Key::CtrlC, Key::CtrlC]);
    assert_eq!(s.rig.quit.now.load(Ordering::SeqCst), 1);
    assert_eq!(*s.term.left.lock().unwrap(), 1);
    s.menu.close();
    s.menu.restore();
    assert_eq!(
        *s.term.left.lock().unwrap(),
        1,
        "exit, stop and a panic after it do nothing more"
    );
    assert_eq!(s.term.text().matches(LEAVE).count(), 1);
}

#[test]
fn the_terminal_is_put_back_once_on_stop_exit_and_panic() {
    for how in ["close", "restore"] {
        let s = setup();
        if how == "close" {
            s.menu.close();
        } else {
            s.menu.restore();
        }
        s.menu.restore();
        s.menu.close();
        assert_eq!(*s.term.left.lock().unwrap(), 1);
        assert_eq!(s.term.text().matches(LEAVE).count(), 1);
    }
}

#[test]
fn a_panic_restores_before_its_message() {
    let s = setup();
    let previous = std::panic::take_hook();
    let order = Arc::new(Mutex::new(vec![]));
    let seen = order.clone();
    let (term, menu) = (s.term.clone(), s.menu.clone());
    let _ = menu;
    std::panic::set_hook(Box::new(move |_| {
        seen.lock().unwrap().push(*term.left.lock().unwrap());
    }));
    s.menu.restore_on_panic();
    let _ = std::panic::catch_unwind(|| panic!("boom"));
    std::panic::set_hook(previous);
    assert_eq!(*order.lock().unwrap(), [1], "restored when the message hook ran");
}

#[test]
fn the_console_is_kept_and_printed_again_when_it_closes() {
    let s = setup();
    // Through the writer `main` installs.
    use tracing_subscriber::fmt::MakeWriter;
    ConsoleWriter(s.log.clone())
        .make_writer()
        .write_all(b"\x1b[32mINFO\x1b[0m first\nsecond\n")
        .unwrap();
    s.menu.draw(&s.rig.agent.frame()).unwrap();
    assert!(s.term.text().contains("INFO first"));
    s.menu.close();
    let text = s.term.text();
    assert!(text.ends_with("INFO first\nsecond\n"), "{text:?}");
    let mut later = Vec::new();
    s.log.write_to(b"later\n", &mut later).unwrap();
    assert_eq!(later, b"later\n", "after it closes the console prints as before");
}

#[test]
fn the_backlog_waiting_rings_once_and_is_highlighted() {
    let s = setup();
    *s.rig.links.backlog.lock().unwrap() = Some(crate::link::pending::BacklogWaiting {
        page_url: "https://retaskable.com/link?code=X".into(),
        label: "Waiting".into(),
    });
    let before = s.term.text().len();
    s.menu.draw(&s.rig.agent.frame()).unwrap();
    let new = s.term.text()[before..].to_string();
    assert!(new.contains(BELL));
    assert!(new.contains("! Retaskable: approve"), "{new}");
    s.menu.draw(&s.rig.agent.frame()).unwrap();
    assert_eq!(s.term.text()[before..].matches(BELL).count(), 1);
}

#[test]
fn a_resize_draws_again() {
    let s = setup();
    s.menu.repaint_if_resized();
    let before = s.term.out.lock().unwrap().len();
    *s.term.size.lock().unwrap() = (40, 10);
    s.menu.repaint_if_resized();
    assert_eq!(s.term.out.lock().unwrap().len(), before + 1);
    s.menu.repaint_if_resized();
    assert_eq!(s.term.out.lock().unwrap().len(), before + 1);
}

#[test]
fn keys_are_read_from_the_terminals_bytes() {
    assert_eq!(
        parse_keys(b"\x1b[A\x1b[B\x1b[C\x1b[D\x1b[H\x1b[F\r \x03\x7f\x1bq"),
        [
            Key::Up,
            Key::Down,
            Key::Right,
            Key::Left,
            Key::Home,
            Key::End,
            Key::Enter,
            Key::Char(' '),
            Key::CtrlC,
            Key::Backspace,
            Key::Esc,
            Key::Char('q'),
        ]
    );
    assert_eq!(parse_keys(b"\x1b"), [Key::Esc]);
    assert_eq!(
        parse_keys(b"\x1b[5~j"),
        [Key::Char('j')],
        "an unknown sequence is skipped"
    );
}
