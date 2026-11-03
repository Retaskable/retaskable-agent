//! Small text helpers shared across the codebase, following `shared/text.ts` in Retaskable.com.
//! Previously duplicated verbatim across `summarize.rs`, `claude.rs`, `pi.rs`, `cursor::result`
//! and `release_notes::group`.

/// `s`, cut to at most `n` chars, with a trailing `…` when it was longer (`clip`).
pub fn clip(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    if n == 0 {
        return String::new();
    }
    let mut out: String = chars[..n - 1].iter().collect();
    out.push('…');
    out
}

/// The first (up to) `max` characters of `s`, with no ellipsis (see [`clip`] for one).
pub fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
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

/// `s` with every run of whitespace (including newlines) replaced by a single space.
pub fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !last_was_space {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    out
}

/// `s` with its first character upper-cased.
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
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
    fn collapse_whitespace_squeezes_runs_to_one_space() {
        assert_eq!(collapse_whitespace("a \n\t b  c"), "a b c");
    }

    #[test]
    fn capitalize_upper_cases_only_the_first_char() {
        assert_eq!(capitalize("élan vital"), "Élan vital");
        assert_eq!(capitalize(""), "");
    }

    #[test]
    fn truncate_keeps_the_first_chars_without_an_ellipsis() {
        assert_eq!(truncate("héllo", 3), "hél");
        assert_eq!(truncate("hi", 5), "hi");
    }

    #[test]
    fn clip_to_nothing_is_empty_not_an_ellipsis() {
        assert_eq!(clip("hello", 0), "");
        assert_eq!(clip("", 0), "");
        assert_eq!(clip("hello", 1), "…");
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
