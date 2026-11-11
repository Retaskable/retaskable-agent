//! Reading a manifest's relative dependency paths (`path = "…"`, `file:`, `link:`) by plain
//! text scan, for `lanes::outside_paths`.

/// Drops `# …` to end of line (a TOML comment) outside a quoted string, so a commented-out
/// `path = "../…"` isn't scanned as a live dependency.
pub(super) fn strip_line_comments(manifest: &str) -> String {
    let mut out = String::with_capacity(manifest.len());
    for line in manifest.split('\n') {
        // `None` outside a string, else the quote that opened it; a basic (`"`) string honours
        // `\"`, a literal (`'`) string has no escapes.
        let mut quote: Option<char> = None;
        let mut escaped = false;
        let mut end = line.len();
        for (idx, ch) in line.char_indices() {
            match (quote, ch) {
                (Some('"'), _) if escaped => escaped = false,
                (Some('"'), '\\') => escaped = true,
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"' | '\'') => quote = Some(ch),
                (None, '#') => {
                    end = idx;
                    break;
                }
                _ => {}
            }
        }
        out.push_str(&line[..end]);
        out.push('\n');
    }
    out
}

pub(super) fn path_eq_values(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = manifest.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if &bytes[i..i + 4] == b"path" && word_boundary_before(bytes, i) {
            let after = skip_ws(&manifest[i + 4..]);
            if let Some(after) = after.strip_prefix('=') {
                let after = skip_ws(after);
                if let Some(value) = quoted_string(after) {
                    out.push(value.to_string());
                }
            }
        }
        i += 1;
    }
    out
}

pub(super) fn file_or_link_values(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    for prefix in ["file:", "link:"] {
        let mut rest = manifest;
        while let Some(idx) = rest.find(prefix) {
            if idx > 0 && rest.as_bytes()[idx - 1] == b'"' {
                let after = &rest[idx + prefix.len()..];
                if let Some(end) = after.find('"') {
                    out.push(after[..end].to_string());
                }
            }
            rest = &rest[idx + prefix.len()..];
        }
    }
    out
}

fn word_boundary_before(bytes: &[u8], i: usize) -> bool {
    i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_'
}

fn skip_ws(s: &str) -> &str {
    s.trim_start_matches(|c: char| c.is_whitespace())
}

fn quoted_string(s: &str) -> Option<&str> {
    let s = s.strip_prefix('"')?;
    let end = s.find('"')?;
    Some(&s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_are_dropped_outside_strings_only() {
        let got = strip_line_comments("a = 1 # note\n# path = \"../x\"\nb = \"has # inside\" # tail");
        assert_eq!(got, "a = 1 \n\nb = \"has # inside\" \n");
    }

    #[test]
    fn basic_strings_honour_escaped_quotes_and_literal_strings_do_not() {
        // `\"` keeps the basic string open, so the `#` is still inside it.
        assert_eq!(strip_line_comments(r#"a = "q\" # in" # out"#), "a = \"q\\\" # in\" \n");
        // A literal string has no escapes: the backslash is plain, the quote closes it.
        assert_eq!(strip_line_comments(r"a = 'x\' # out"), "a = 'x\\' \n");
        // The other quote kind inside a string doesn't close it.
        assert_eq!(strip_line_comments(r#"a = "it's # in""#), "a = \"it's # in\"\n");
    }

    #[test]
    fn path_values_are_read_with_or_without_spaces() {
        let m = "a = { path = \"../a\" }\nb = {path=\"../b\"}\nc = { path   =   \"c\" }";
        assert_eq!(path_eq_values(m), vec!["../a", "../b", "c"]);
    }

    #[test]
    fn path_needs_a_word_boundary_an_equals_and_a_quoted_value() {
        assert!(path_eq_values("subpath = \"../x\"").is_empty());
        assert!(path_eq_values("my_path = \"../x\"").is_empty());
        assert!(path_eq_values("path \"../x\"").is_empty());
        assert!(path_eq_values("path = ../x").is_empty());
        assert!(path_eq_values("path = \"unterminated").is_empty());
        assert_eq!(path_eq_values("path = \"p\""), vec!["p"]);
    }

    #[test]
    fn file_and_link_values_need_an_opening_quote() {
        let m = r#"{"a": "file:../a", "b": "link:../b", "c": "x file:../no", "d": "file:../d"}"#;
        assert_eq!(file_or_link_values(m), vec!["../a", "../d", "../b"]);
    }

    #[test]
    fn file_value_without_a_closing_quote_is_skipped() {
        assert!(file_or_link_values("\"file:../a").is_empty());
        assert!(file_or_link_values("no prefixes here").is_empty());
    }
}
