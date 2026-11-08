//! The terminal menu (tray/04-the-tray-agent.md section 4.7), following `agent/src/terminalMenu.ts`: a
//! [`TrayView`] drawing the tray's menu full screen in the terminal's alternate screen, turning keys into the
//! tray's own actions, keeping the console's output in a log pane meanwhile, and putting the terminal back exactly
//! once however the agent ends.
#[cfg(test)]
mod tests;

use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use anyhow::Result;

use super::agent::{Frame, Opener, Quit, SystemOpener, TrayAgent, TrayView};
use crate::terminal_menu::console::ConsoleCapture;
use crate::terminal_menu::{Cursor, Key, Outcome, QUIT_HINT, backlog_notice, handle_key, render_screen};

const ENTER: &str = "\x1b[?1049h\x1b[?25l";
const LEAVE: &str = "\x1b[?25h\x1b[?1049l";
const BELL: &str = "\x07";

/// The terminal: raw mode, size and writes. The real one is `term::StdTerm`.
pub trait Term: Send + Sync {
    fn enter(&self) -> io::Result<()>;
    fn leave(&self);
    /// Columns and rows.
    fn size(&self) -> (usize, usize);
    fn write(&self, s: &str);
}

/// The OS's opener, saying the address in the log first, for a machine with no browser. A desktop notification
/// is not sent: the menu shows the wait itself.
pub struct TuiOpener;

impl Opener for TuiOpener {
    fn open_url(&self, url: &str) -> Result<()> {
        tracing::info!("Open {url}");
        SystemOpener.open_url(url)
    }
    fn open_file(&self, file: &Path) -> Result<()> {
        SystemOpener.open_file(file)
    }
    fn notify(&self, _: &str) {}
}

/// The bytes the terminal sent, as keys. An unknown escape sequence is skipped.
pub fn parse_keys(bytes: &[u8]) -> Vec<Key> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = vec![];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(match c {
            '\x1b' => match chars.peek() {
                Some('[') | Some('O') => {
                    chars.next();
                    let mut seq = String::new();
                    while let Some(&d) = chars.peek() {
                        chars.next();
                        seq.push(d);
                        if d.is_ascii_alphabetic() || d == '~' {
                            break;
                        }
                    }
                    match seq.as_str() {
                        "A" => Key::Up,
                        "B" => Key::Down,
                        "C" => Key::Right,
                        "D" => Key::Left,
                        "H" | "1~" | "7~" => Key::Home,
                        "F" | "4~" | "8~" => Key::End,
                        _ => continue,
                    }
                }
                _ => Key::Esc,
            },
            '\r' | '\n' => Key::Enter,
            '\x03' => Key::CtrlC,
            '\x7f' | '\x08' => Key::Backspace,
            c => Key::Char(c),
        });
    }
    out
}

struct Screen {
    frame: Option<Frame>,
    cursor: Cursor,
    notice: Option<String>,
    /// The backlog's wait has been announced (and rung) already.
    announced: bool,
    size: (usize, usize),
}

pub struct TerminalMenu {
    term: Arc<dyn Term>,
    log: Arc<ConsoleCapture>,
    quit: Arc<dyn Quit>,
    agent: OnceLock<Weak<TrayAgent>>,
    screen: Mutex<Screen>,
    restored: AtomicBool,
}

impl TerminalMenu {
    /// Puts the terminal in raw mode on the alternate screen and starts keeping the console's output. An error
    /// means the terminal is as it was and the agent carries on with the plain console.
    pub fn start(term: Arc<dyn Term>, log: Arc<ConsoleCapture>, quit: Arc<dyn Quit>) -> io::Result<Arc<Self>> {
        term.enter()?;
        log.start();
        term.write(ENTER);
        let size = term.size();
        Ok(Arc::new(Self {
            term,
            log,
            quit,
            agent: OnceLock::new(),
            screen: Mutex::new(Screen {
                frame: None,
                cursor: Cursor::default(),
                notice: None,
                announced: false,
                size,
            }),
            restored: AtomicBool::new(false),
        }))
    }

    /// The agent a choice runs on (a click, as the tray's).
    pub fn attach(&self, agent: &Arc<TrayAgent>) {
        let _ = self.agent.set(Arc::downgrade(agent));
    }

    fn paint(&self, s: &mut Screen, bell: bool) {
        let Some(frame) = &s.frame else { return };
        s.size = self.term.size();
        let (w, h) = s.size;
        let lines = render_screen(
            &frame.menu,
            &s.cursor,
            w,
            h,
            &frame.tooltip,
            s.notice.as_deref(),
            &self.log.tail(h),
        );
        let mut out = String::from(if bell { BELL } else { "" });
        out.push_str("\x1b[H");
        for (n, line) in lines.iter().enumerate() {
            if n > 0 {
                out.push_str("\r\n");
            }
            out.push_str(line);
            out.push_str("\x1b[K");
        }
        out.push_str("\x1b[J");
        self.term.write(&out);
    }

    /// Draws again when the terminal's size has changed.
    pub fn repaint_if_resized(&self) {
        if self.restored.load(Ordering::SeqCst) {
            return;
        }
        let mut s = self.screen.lock().unwrap();
        if self.term.size() != s.size {
            self.paint(&mut s, false);
        }
    }

    /// One key: moves the cursor, or runs the chosen item as the tray's click would, or stops the agent.
    pub fn key(&self, key: Key) {
        if self.restored.load(Ordering::SeqCst) {
            return;
        }
        let outcome = {
            let mut s = self.screen.lock().unwrap();
            let Some(menu) = s.frame.as_ref().map(|f| f.menu.clone()) else {
                return;
            };
            s.notice = None;
            let (cursor, outcome) = handle_key(&menu, &s.cursor, key);
            s.cursor = cursor;
            if outcome == Some(Outcome::Hint) {
                s.notice = Some(QUIT_HINT.into());
            }
            self.paint(&mut s, false);
            outcome
        };
        match outcome {
            Some(Outcome::Run(id)) => {
                if let Some(agent) = self.agent.get().and_then(Weak::upgrade) {
                    agent.click(&id);
                }
            }
            Some(Outcome::Stop) => {
                self.restore();
                self.quit.now();
            }
            _ => {}
        }
    }

    /// Puts the terminal back (cursor shown, raw mode off, the normal screen) and prints what the console said
    /// meanwhile. Only the first call does anything.
    pub fn restore(&self) {
        if self.restored.swap(true, Ordering::SeqCst) {
            return;
        }
        self.term.write(LEAVE);
        self.term.leave();
        let mut printed = Vec::new();
        let _ = self.log.finish(&mut printed);
        if !printed.is_empty() {
            self.term.write(&String::from_utf8_lossy(&printed));
        }
    }

    /// Reads the keys on stdin and watches the size, on threads of their own. Not for tests.
    pub fn spawn_input(self: &Arc<Self>) {
        let menu = self.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let mut stdin = io::stdin().lock();
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 {
                    break;
                }
                for key in parse_keys(&buf[..n]) {
                    menu.key(key);
                }
            }
        });
        let menu = self.clone();
        std::thread::spawn(move || {
            while !menu.restored.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(250));
                menu.repaint_if_resized();
            }
        });
    }

    /// A panic puts the terminal back before its message is printed.
    pub fn restore_on_panic(self: &Arc<Self>) {
        let menu = self.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            menu.restore();
            previous(info);
        }));
    }
}

impl TrayView for TerminalMenu {
    fn draw(&self, frame: &Frame) -> Result<()> {
        if self.restored.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut s = self.screen.lock().unwrap();
        let mut bell = false;
        match backlog_notice(&frame.menu) {
            Some(n) if !s.announced => {
                s.announced = true;
                bell = true;
                tracing::info!("{n}");
                s.notice = Some(n);
            }
            None => s.announced = false,
            _ => {}
        }
        s.frame = Some(frame.clone());
        self.paint(&mut s, bell);
        Ok(())
    }

    fn close(&self) {
        self.restore();
    }
}
