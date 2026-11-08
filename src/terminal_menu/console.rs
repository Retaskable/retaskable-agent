//! The console capture for the terminal menu (tray/04-the-tray-agent.md section 4.7): while the menu is up the
//! agent's console lines go to a ring buffer for its log pane instead of the screen, and `finish` prints them on
//! the normal screen once the menu closes.
use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, LazyLock, Mutex};

use crate::tray::tui::strip_ansi;
use tokio::sync::watch;
use tracing_subscriber::fmt::MakeWriter;

/// How many lines the buffer keeps.
pub const KEPT: usize = 2000;

#[derive(Default)]
struct State {
    capturing: bool,
    lines: VecDeque<String>,
    /// A line still being written (no newline yet).
    partial: String,
}

pub struct ConsoleCapture {
    state: Mutex<State>,
    version: watch::Sender<u64>,
}

static CONSOLE: LazyLock<Arc<ConsoleCapture>> = LazyLock::new(|| Arc::new(ConsoleCapture::new()));

impl Default for ConsoleCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsoleCapture {
    pub fn new() -> Self {
        Self {
            state: Mutex::default(),
            version: watch::channel(0).0,
        }
    }

    /// The agent's own, shared by the tracing writer and [`say`].
    pub fn console() -> Arc<ConsoleCapture> {
        CONSOLE.clone()
    }

    pub fn start(&self) {
        self.state.lock().unwrap().capturing = true;
    }

    /// Goes up by one with each captured line; the menu redraws when it changes.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }

    pub fn lines(&self) -> Vec<String> {
        self.state.lock().unwrap().lines.iter().cloned().collect()
    }

    /// Stops capturing and writes every captured line to `out`, once.
    pub fn finish(&self, out: &mut impl Write) -> io::Result<()> {
        let lines = {
            let mut s = self.state.lock().unwrap();
            s.capturing = false;
            if !s.partial.is_empty() {
                let rest = std::mem::take(&mut s.partial);
                Self::push(&mut s, &self.version, rest);
            }
            s.lines.drain(..).collect::<Vec<_>>()
        };
        for line in lines {
            writeln!(out, "{line}")?;
        }
        out.flush()
    }

    fn push(s: &mut State, version: &watch::Sender<u64>, line: String) {
        if s.lines.len() == KEPT {
            s.lines.pop_front();
        }
        s.lines.push_back(line);
        version.send_modify(|v| *v += 1);
    }

    /// Keeps `buf`'s whole lines (without ANSI) while capturing, else passes it to `out`.
    pub fn write_to(&self, buf: &[u8], out: &mut impl Write) -> io::Result<usize> {
        let mut s = self.state.lock().unwrap();
        if !s.capturing {
            drop(s);
            return out.write(buf);
        }
        s.partial.push_str(&strip_ansi(&String::from_utf8_lossy(buf)));
        while let Some(i) = s.partial.find('\n') {
            let rest = s.partial.split_off(i + 1);
            let mut line = std::mem::replace(&mut s.partial, rest);
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
            Self::push(&mut s, &self.version, line);
        }
        Ok(buf.len())
    }

    /// A line of the agent's own console: in the pane while capturing, else printed.
    pub fn say_to(&self, line: &str, out: &mut impl Write) {
        let _ = self.write_to(format!("{line}\n").as_bytes(), out);
    }
}

/// Like `println!`, for the agent's own console lines: they go to the log pane while the menu is up.
pub fn say(line: &str) {
    ConsoleCapture::console().say_to(line, &mut io::stdout());
}

/// The tracing subscriber's writer: stdout as ever, or the buffer while capturing.
#[derive(Clone)]
pub struct ConsoleWriter(pub Arc<ConsoleCapture>);

pub struct ConsoleOut(Arc<ConsoleCapture>);

impl Write for ConsoleOut {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_to(buf, &mut io::stdout())
    }
    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()
    }
}

impl<'a> MakeWriter<'a> for ConsoleWriter {
    type Writer = ConsoleOut;
    fn make_writer(&'a self) -> ConsoleOut {
        ConsoleOut(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_while_off() {
        let c = ConsoleCapture::new();
        let mut out = vec![];
        c.write_to(b"hello\n", &mut out).unwrap();
        assert_eq!(out, b"hello\n");
        assert!(c.lines().is_empty());
    }

    #[test]
    fn captures_without_output_or_ansi_while_on() {
        let c = ConsoleCapture::new();
        c.start();
        let mut out = vec![];
        c.write_to(b"\x1b[32mgreen\x1b[0m\nnext\r\n", &mut out).unwrap();
        assert!(out.is_empty());
        assert_eq!(c.lines(), ["green", "next"]);
    }

    #[test]
    fn keeps_only_the_last_2000() {
        let c = ConsoleCapture::new();
        c.start();
        let mut out = vec![];
        for i in 0..KEPT + 5 {
            c.write_to(format!("l{i}\n").as_bytes(), &mut out).unwrap();
        }
        let lines = c.lines();
        assert_eq!(lines.len(), KEPT);
        assert_eq!(lines[0], "l5");
        assert_eq!(lines[KEPT - 1], format!("l{}", KEPT + 4));
    }

    #[test]
    fn a_partial_write_and_the_rest_make_one_line() {
        let c = ConsoleCapture::new();
        c.start();
        let mut out = vec![];
        c.write_to(b"half ", &mut out).unwrap();
        assert!(c.lines().is_empty());
        c.write_to(b"and half\n", &mut out).unwrap();
        assert_eq!(c.lines(), ["half and half"]);
    }

    #[test]
    fn finish_prints_once() {
        let c = ConsoleCapture::new();
        c.start();
        let mut sink = vec![];
        c.write_to(b"a\nb\nunfinished", &mut sink).unwrap();
        let mut out = vec![];
        c.finish(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a\nb\nunfinished\n");
        let mut again = vec![];
        c.finish(&mut again).unwrap();
        assert!(again.is_empty());
        // Capture is off again: lines pass through.
        c.write_to(b"x\n", &mut again).unwrap();
        assert_eq!(again, b"x\n");
    }

    #[test]
    fn version_goes_up_with_each_line() {
        let c = ConsoleCapture::new();
        let rx = c.subscribe();
        c.start();
        let mut out = vec![];
        c.write_to(b"a\nb\n", &mut out).unwrap();
        assert_eq!(*rx.borrow(), 2);
        c.write_to(b"c\n", &mut out).unwrap();
        assert_eq!(*rx.borrow(), 3);
    }

    #[test]
    fn say_goes_to_the_pane_while_capturing() {
        let c = ConsoleCapture::new();
        let mut out = vec![];
        c.say_to("one", &mut out);
        assert_eq!(out, b"one\n");
        c.start();
        c.say_to("two", &mut out);
        assert_eq!(out, b"one\n");
        assert_eq!(c.lines(), ["two"]);
    }
}
