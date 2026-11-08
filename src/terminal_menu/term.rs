//! The terminal control the terminal menu draws on (tray/04-the-tray-agent.md section 4.7): raw mode, the
//! alternate screen and plain ANSI, with no dependency beyond `libc` / `windows-sys`. `restore` is process-wide and
//! idempotent, so a panic hook, a signal path and an exit path can all call it.
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

const ENTER: &str = "\x1b[?1049h\x1b[?25l";
const RESTORE: &str = "\x1b[?25h\x1b[?1049l";
const HOME: &str = "\x1b[H";
const CLEAR_LINE: &str = "\x1b[K";
const CLEAR_BELOW: &str = "\x1b[J";

/// Set by `enter`, cleared by the first `restore`.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static SAVED: Mutex<Option<Saved>> = Mutex::new(None);

#[cfg(unix)]
type Saved = libc::termios;
#[cfg(windows)]
type Saved = (u32, u32); // stdin mode, stdout mode
#[cfg(not(any(unix, windows)))]
type Saved = ();

/// Stdin and stdout are both terminals.
pub fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// The screen for `lines`: home, each line cleared to its end, then the rest of the screen cleared. One string, so
/// one write.
pub fn screen(lines: &[String]) -> String {
    let mut s = String::from(HOME);
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            s.push_str("\r\n");
        }
        s.push_str(line);
        s.push_str(CLEAR_LINE);
    }
    s.push_str(CLEAR_BELOW);
    s
}

fn write_out(out: &mut dyn Write, s: &str) -> io::Result<()> {
    out.write_all(s.as_bytes())?;
    out.flush()
}

/// Marks the terminal active and writes the enter sequence; `put_raw` has already saved the mode.
fn enter_with(active: &AtomicBool, out: &mut dyn Write) -> io::Result<()> {
    active.store(true, Ordering::SeqCst);
    write_out(out, ENTER)
}

/// Writes the restore sequence and runs `put_back` once; does nothing if not active.
fn restore_with(active: &AtomicBool, out: &mut dyn Write, put_back: impl FnOnce()) {
    if active.swap(false, Ordering::SeqCst) {
        let _ = write_out(out, RESTORE);
        put_back();
    }
}

/// Puts the terminal back, if `Terminal::enter` ran and this hasn't already.
pub fn restore() {
    restore_with(&ACTIVE, &mut io::stdout(), || {
        let saved = SAVED.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(saved) = saved {
            put_back(&saved);
        }
    });
}

/// The terminal in raw mode on the alternate screen. Dropping it restores the terminal.
pub struct Terminal(());

impl Terminal {
    pub fn enter() -> io::Result<Terminal> {
        if ACTIVE.load(Ordering::SeqCst) {
            return Err(io::Error::other("the terminal is already in raw mode"));
        }
        let saved = put_raw()?;
        *SAVED.lock().unwrap_or_else(|e| e.into_inner()) = Some(saved);
        if let Err(e) = enter_with(&ACTIVE, &mut io::stdout()) {
            restore();
            return Err(e);
        }
        Ok(Terminal(()))
    }

    /// (columns, rows); 80x24 when unknown.
    pub fn size(&self) -> (u16, u16) {
        size().unwrap_or((80, 24))
    }

    /// Draws `lines` over the whole screen in one write.
    pub fn draw(&self, lines: &[String]) {
        let _ = write_out(&mut io::stdout(), &screen(lines));
    }

    pub fn bell(&self) {
        let _ = write_out(&mut io::stdout(), "\x07");
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        restore();
    }
}

#[cfg(unix)]
fn put_raw() -> io::Result<Saved> {
    // SAFETY: tcgetattr/tcsetattr on stdin with a zeroed termios that tcgetattr fills in.
    unsafe {
        let mut saved: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(libc::STDIN_FILENO, &mut saved) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = saved;
        libc::cfmakeraw(&mut raw);
        if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &raw) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(saved)
    }
}

#[cfg(unix)]
fn put_back(saved: &Saved) {
    // SAFETY: restores a termios that tcgetattr produced.
    unsafe {
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, saved);
    }
}

#[cfg(unix)]
fn size() -> Option<(u16, u16)> {
    // SAFETY: TIOCGWINSZ fills a winsize.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        let ok = libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0;
        (ok && ws.ws_col > 0 && ws.ws_row > 0).then_some((ws.ws_col, ws.ws_row))
    }
}

#[cfg(windows)]
mod win {
    pub use windows_sys::Win32::System::Console::*;
}

#[cfg(windows)]
fn put_raw() -> io::Result<Saved> {
    use win::*;
    // SAFETY: console handle and mode calls with valid out pointers.
    unsafe {
        let (hin, hout) = (GetStdHandle(STD_INPUT_HANDLE), GetStdHandle(STD_OUTPUT_HANDLE));
        let (mut min, mut mout) = (0u32, 0u32);
        if GetConsoleMode(hin, &mut min) == 0 || GetConsoleMode(hout, &mut mout) == 0 {
            return Err(io::Error::last_os_error());
        }
        let raw_in =
            (min & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT)) | ENABLE_VIRTUAL_TERMINAL_INPUT;
        if SetConsoleMode(hin, raw_in) == 0 {
            return Err(io::Error::last_os_error());
        }
        if SetConsoleMode(hout, mout | ENABLE_VIRTUAL_TERMINAL_PROCESSING) == 0 {
            let e = io::Error::last_os_error();
            SetConsoleMode(hin, min);
            return Err(e);
        }
        Ok((min, mout))
    }
}

#[cfg(windows)]
fn put_back(saved: &Saved) {
    use win::*;
    // SAFETY: restores modes read by GetConsoleMode.
    unsafe {
        SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), saved.0);
        SetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), saved.1);
    }
}

#[cfg(windows)]
fn size() -> Option<(u16, u16)> {
    use win::*;
    // SAFETY: GetConsoleScreenBufferInfo fills the zeroed struct.
    unsafe {
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) == 0 {
            return None;
        }
        let w = info.srWindow;
        let (cols, rows) = (w.Right - w.Left + 1, w.Bottom - w.Top + 1);
        (cols > 0 && rows > 0).then_some((cols as u16, rows as u16))
    }
}

#[cfg(not(any(unix, windows)))]
fn put_raw() -> io::Result<Saved> {
    Err(io::Error::other("no terminal control on this OS"))
}
#[cfg(not(any(unix, windows)))]
fn put_back(_: &Saved) {}
#[cfg(not(any(unix, windows)))]
fn size() -> Option<(u16, u16)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(b: Vec<u8>) -> String {
        String::from_utf8(b).unwrap()
    }

    #[test]
    fn draw_homes_writes_each_line_and_clears() {
        let s = screen(&["a".into(), "b".into()]);
        assert_eq!(s, "\x1b[Ha\x1b[K\r\nb\x1b[K\x1b[J");
    }

    #[test]
    fn enter_and_restore_write_their_sequences() {
        let active = AtomicBool::new(false);
        let mut out = Vec::new();
        enter_with(&active, &mut out).unwrap();
        assert_eq!(text(out), "\x1b[?1049h\x1b[?25l");
        let mut out = Vec::new();
        restore_with(&active, &mut out, || {});
        assert_eq!(text(out), "\x1b[?25h\x1b[?1049l");
    }

    #[test]
    fn restore_twice_writes_once() {
        let active = AtomicBool::new(true);
        let (mut out, mut puts) = (Vec::new(), 0);
        restore_with(&active, &mut out, || puts += 1);
        restore_with(&active, &mut out, || puts += 1);
        assert_eq!(text(out), RESTORE);
        assert_eq!(puts, 1);
    }

    #[test]
    fn restore_before_enter_writes_nothing() {
        let active = AtomicBool::new(false);
        let mut out = Vec::new();
        restore_with(&active, &mut out, || panic!("nothing to put back"));
        assert!(out.is_empty());
    }
}
