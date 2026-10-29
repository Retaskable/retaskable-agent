//! The two small text helpers every engine's reader needs, following `shared/text.ts` in
//! Retaskable.com. Previously duplicated verbatim across `summarize.rs`, `claude.rs`, `pi.rs` and
//! `cursor::result`.

/// `s`, cut to at most `n` chars, with a trailing `…` when it was longer (`clip`).
pub fn clip(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    let mut out: String = chars[..n.saturating_sub(1)].iter().collect();
    out.push('…');
    out
}

/// The last (up to) `max_chars` characters of `s`, on a char boundary (`tail`).
pub fn tail(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        chars[chars.len() - max_chars..].iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_passes_short_text_through_and_truncates_with_an_ellipsis_otherwise() {
        assert_eq!(clip("hello", 10), "hello");
        assert_eq!(clip("hello world", 5), "hell…");
    }

    #[test]
    fn tail_keeps_only_the_last_n_characters() {
        assert_eq!(tail("hello world", 5), "world");
        assert_eq!(tail("hi", 10), "hi");
    }
}
