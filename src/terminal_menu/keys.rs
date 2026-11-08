//! Raw terminal input bytes to `Key`s (tray/04-the-tray-agent.md section 4.7, Keys). Pure: no I/O, no per-OS code
//! (Windows sends the same bytes once ENABLE_VIRTUAL_TERMINAL_INPUT is on). A sequence split across reads waits in
//! the decoder; `flush`, called after a short wait with nothing more read, turns a lone ESC into Esc.

use super::Key;

const ESC: u8 = 0x1b;

#[derive(Debug, Default)]
pub struct KeyDecoder {
    pending: Vec<u8>,
}

impl KeyDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes what is complete; an unfinished sequence waits for the next `feed` or `flush`.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Key> {
        self.pending.extend_from_slice(bytes);
        let mut keys = Vec::new();
        let mut at = 0;
        while at < self.pending.len() {
            match parse(&self.pending[at..]) {
                Parsed::Wait => break,
                Parsed::Done(used, key) => {
                    keys.extend(key);
                    at += used;
                }
            }
        }
        self.pending.drain(..at);
        keys
    }

    /// Nothing more came: a lone ESC is Esc; any other unfinished sequence is dropped.
    pub fn flush(&mut self) -> Vec<Key> {
        let lone = self.pending == [ESC];
        self.pending.clear();
        if lone { vec![Key::Esc] } else { Vec::new() }
    }

    /// Whether bytes are waiting for more (the caller should `flush` after its short wait).
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

enum Parsed {
    Wait,
    /// Bytes used, and the key they make (none: dropped).
    Done(usize, Option<Key>),
}

fn parse(b: &[u8]) -> Parsed {
    match b[0] {
        ESC => parse_escape(b),
        0x0d | 0x0a => Parsed::Done(1, Some(Key::Enter)),
        0x7f | 0x08 => Parsed::Done(1, Some(Key::Backspace)),
        0x03 => Parsed::Done(1, Some(Key::CtrlC)),
        _ => parse_char(b),
    }
}

fn parse_escape(b: &[u8]) -> Parsed {
    match b.get(1) {
        None => Parsed::Wait,
        Some(b'[') => {
            // CSI: parameter bytes 0x30-0x3f, intermediates 0x20-0x2f, then a final byte 0x40-0x7e.
            let Some(end) = b[2..].iter().position(|c| (0x40..=0x7e).contains(c)) else {
                return match b[2..].iter().position(|c| !(0x20..=0x3f).contains(c)) {
                    None => Parsed::Wait,
                    Some(bad) => Parsed::Done(2 + bad, None),
                };
            };
            let end = end + 2;
            Parsed::Done(end + 1, csi_key(&b[2..end], b[end]))
        }
        Some(b'O') => match b.get(2) {
            None => Parsed::Wait,
            Some(&f) => Parsed::Done(3, final_key(f)),
        },
        Some(_) => Parsed::Done(1, Some(Key::Esc)),
    }
}

fn final_key(f: u8) -> Option<Key> {
    match f {
        b'A' => Some(Key::Up),
        b'B' => Some(Key::Down),
        b'C' => Some(Key::Right),
        b'D' => Some(Key::Left),
        b'H' => Some(Key::Home),
        b'F' => Some(Key::End),
        _ => None,
    }
}

fn csi_key(params: &[u8], f: u8) -> Option<Key> {
    match (params, f) {
        (b"1" | b"7", b'~') => Some(Key::Home),
        (b"4" | b"8", b'~') => Some(Key::End),
        // Plain, or with modifiers (ESC [ 1 ; 5 A), the letter finals are the arrows and Home/End.
        (p, _) if f != b'~' && p.iter().all(|c| c.is_ascii_digit() || *c == b';') => final_key(f),
        _ => None,
    }
}

fn parse_char(b: &[u8]) -> Parsed {
    let need = match b[0] {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Parsed::Done(1, None),
    };
    if b.len() < need {
        // Wait only while what is here could still be a character.
        return if b[1..].iter().all(|c| c & 0xc0 == 0x80) {
            Parsed::Wait
        } else {
            Parsed::Done(1, None)
        };
    }
    match std::str::from_utf8(&b[..need]) {
        Ok(s) => Parsed::Done(need, s.chars().next().map(Key::Char)),
        Err(_) => Parsed::Done(1, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bytes: &[u8]) -> Vec<Key> {
        KeyDecoder::new().feed(bytes)
    }

    #[test]
    fn arrows_in_csi_and_ss3_forms() {
        for (seq, key) in [("A", Key::Up), ("B", Key::Down), ("C", Key::Right), ("D", Key::Left)] {
            assert_eq!(keys(format!("\x1b[{seq}").as_bytes()), vec![key]);
            assert_eq!(keys(format!("\x1bO{seq}").as_bytes()), vec![key]);
        }
    }

    #[test]
    fn home_and_end_in_every_form() {
        for s in ["\x1b[H", "\x1b[1~", "\x1b[7~", "\x1bOH"] {
            assert_eq!(keys(s.as_bytes()), vec![Key::Home], "{s:?}");
        }
        for s in ["\x1b[F", "\x1b[4~", "\x1b[8~", "\x1bOF"] {
            assert_eq!(keys(s.as_bytes()), vec![Key::End], "{s:?}");
        }
    }

    #[test]
    fn enter_backspace_ctrl_c() {
        assert_eq!(keys(b"\r\n"), vec![Key::Enter, Key::Enter]);
        assert_eq!(keys(&[0x7f, 0x08]), vec![Key::Backspace, Key::Backspace]);
        assert_eq!(keys(&[0x03]), vec![Key::CtrlC]);
    }

    #[test]
    fn chars_including_multi_byte() {
        let want: Vec<Key> = "qjkhl é€😀".chars().map(Key::Char).collect();
        assert_eq!(keys("qjkhl é€😀".as_bytes()), want);
    }

    #[test]
    fn a_split_sequence_decodes() {
        let mut d = KeyDecoder::new();
        assert!(d.feed(b"\x1b").is_empty());
        assert!(d.feed(b"[").is_empty());
        assert_eq!(d.feed(b"A"), vec![Key::Up]);
        assert!(d.feed(&"€".as_bytes()[..2]).is_empty());
        assert_eq!(d.feed(&"€".as_bytes()[2..]), vec![Key::Char('€')]);
        assert!(d.flush().is_empty());
    }

    #[test]
    fn a_lone_esc_is_esc_only_after_flush() {
        let mut d = KeyDecoder::new();
        assert!(d.feed(&[ESC]).is_empty());
        assert!(d.is_pending());
        assert_eq!(d.flush(), vec![Key::Esc]);
        assert!(!d.is_pending());
        assert!(d.flush().is_empty());
    }

    #[test]
    fn unknown_sequences_are_dropped_whole() {
        assert!(keys(b"\x1b[99z").is_empty());
        assert!(keys(b"\x1b[3~").is_empty());
        assert!(keys(b"\x1bOZ").is_empty());
        assert_eq!(keys(b"\x1b[3~q"), vec![Key::Char('q')]);
    }

    #[test]
    fn a_bad_csi_byte_keeps_the_keys_after_it() {
        assert_eq!(keys(b"\x1b[\x03"), vec![Key::CtrlC]);
        assert_eq!(keys(b"\x1b[\x01q"), vec![Key::Char('q')]);
    }

    #[test]
    fn esc_before_another_key_is_esc_then_that_key() {
        assert_eq!(keys(b"\x1bq"), vec![Key::Esc, Key::Char('q')]);
    }
}
