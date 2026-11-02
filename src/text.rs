//! Small text helpers shared across the codebase, following `shared/text.ts` in Retaskable.com.
//! Previously duplicated verbatim across `summarize.rs`, `claude.rs`, `pi.rs`, `cursor::result`
//! and `release_notes::group`.

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

/// `s` with runs of whitespace (including newlines) collapsed to single spaces, then [`clip`]ped
/// to at most `n` chars. Every engine's reader uses this to fit a tool call's argument onto one
/// transcript line.
pub fn collapse_and_clip(s: &str, n: usize) -> String {
    clip(&s.split_whitespace().collect::<Vec<_>>().join(" "), n)
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

    #[test]
    fn collapse_and_clip_joins_whitespace_runs_before_clipping() {
        assert_eq!(collapse_and_clip("hello\n  world", 20), "hello world");
        assert_eq!(collapse_and_clip("hello   world", 7), "hello …");
    }
}
