//! The real terminal for the terminal menu (tray/04-the-tray-agent.md section 4.7): raw mode through termios on
//! Unix and the console API on Windows, its size, and writes to stdout.
use std::io::{self, Write};
use std::sync::Mutex;

use super::tui::Term;

/// Raw mode is left exactly as it was found.
#[derive(Default)]
pub struct StdTerm {
    saved: Mutex<Option<Saved>>,
}

#[cfg(unix)]
type Saved = libc::termios;
#[cfg(windows)]
type Saved = (u32, u32);

impl Term for StdTerm {
    #[cfg(unix)]
    fn enter(&self) -> io::Result<()> {
        // SAFETY: plain termios calls on stdin with a zeroed struct that tcgetattr fills.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return Err(io::Error::last_os_error());
            }
            *self.saved.lock().unwrap() = Some(t);
            libc::cfmakeraw(&mut t);
            t.c_oflag |= libc::OPOST; // lines the log prints later still end properly
            if libc::tcsetattr(0, libc::TCSADRAIN, &t) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn leave(&self) {
        if let Some(t) = self.saved.lock().unwrap().take() {
            // SAFETY: restores the settings tcgetattr returned.
            unsafe { libc::tcsetattr(0, libc::TCSADRAIN, &t) };
        }
    }

    #[cfg(unix)]
    fn size(&self) -> (usize, usize) {
        // SAFETY: TIOCGWINSZ fills a winsize.
        let ws = unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            (libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0).then_some(ws)
        };
        match ws {
            Some(ws) if ws.ws_col > 0 && ws.ws_row > 0 => (ws.ws_col as usize, ws.ws_row as usize),
            _ => (80, 24),
        }
    }

    #[cfg(windows)]
    fn enter(&self) -> io::Result<()> {
        use windows_sys::Win32::System::Console::*;
        // SAFETY: console mode calls on the process's own standard handles.
        unsafe {
            let (i, o) = (GetStdHandle(STD_INPUT_HANDLE), GetStdHandle(STD_OUTPUT_HANDLE));
            let (mut im, mut om) = (0u32, 0u32);
            if GetConsoleMode(i, &mut im) == 0 || GetConsoleMode(o, &mut om) == 0 {
                return Err(io::Error::last_os_error());
            }
            *self.saved.lock().unwrap() = Some((im, om));
            let raw = (im & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
                | ENABLE_VIRTUAL_TERMINAL_INPUT;
            if SetConsoleMode(i, raw) == 0 || SetConsoleMode(o, om | ENABLE_VIRTUAL_TERMINAL_PROCESSING) == 0 {
                let e = io::Error::last_os_error();
                self.leave();
                return Err(e);
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    fn leave(&self) {
        use windows_sys::Win32::System::Console::*;
        if let Some((im, om)) = self.saved.lock().unwrap().take() {
            // SAFETY: restores the modes GetConsoleMode returned.
            unsafe {
                SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), im);
                SetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), om);
            }
        }
    }

    #[cfg(windows)]
    fn size(&self) -> (usize, usize) {
        use windows_sys::Win32::System::Console::*;
        // SAFETY: fills a CONSOLE_SCREEN_BUFFER_INFO for the output handle.
        unsafe {
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
            if GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) == 0 {
                return (80, 24);
            }
            let w = (info.srWindow.Right - info.srWindow.Left + 1).max(1) as usize;
            let h = (info.srWindow.Bottom - info.srWindow.Top + 1).max(1) as usize;
            (w, h)
        }
    }

    fn write(&self, s: &str) {
        let mut out = io::stdout().lock();
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    }
}
