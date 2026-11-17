//! The one plain sentence on the "Does this look right?" prompt (TS `shared/plainSentence.ts`, add-and-design-review.md
//! section 6): what changes for the people using the product, in words a person who has never seen the code understands. The
//! session writes it in a structured field; this deterministic check (never a reviewer's eye) rejects jargon and length, asks
//! the session for one rewrite, then falls back to a safe generic sentence. Pure plain code.
use std::future::Future;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

/// At most this many words, and this many sentences: one plain sentence, and at most one more saying what stays the same.
pub const SENTENCE_MAX_WORDS: usize = 30;
pub const SENTENCE_MAX_SENTENCES: usize = 2;
pub const SENTENCE_MAX_CHARS: usize = 240;

/// What is shown when neither the session's sentence nor its rewrite passes: true of any change a person will see, and no
/// jargon.
pub const SAFE_SENTENCE: &str = "You'll see a small change on one screen. Everything else stays the same.";

/// Words that belong to the code, the plan or the process, never to what a person sees.
const JARGON_WORDS: &[&str] = &[
    "component",
    "components",
    "flag",
    "flags",
    "pr",
    "prs",
    "pull request",
    "tier",
    "api",
    "apis",
    "schema",
    "endpoint",
    "endpoints",
    "refactor",
    "refactors",
    "refactoring",
    "plan",
    "plans",
    "commit",
    "branch",
    "repo",
    "repository",
    "merge",
    "deploy",
    "backend",
    "frontend",
    "database",
    "json",
    "url",
    "sdk",
    "ui",
    "ux",
    "props",
    "prop",
    "hook",
    "hooks",
    "regex",
    "config",
    "function",
    "variable",
    "module",
    "stylesheet",
    "tsx",
    "css",
    "html",
    "javascript",
    "typescript",
    "react",
    "library",
    "layout",
    "viewport",
    "render",
    "renders",
    "rendered",
];

/// JavaScript's `\b` and `\w` are ASCII, so these are too (`(?-u:\b)`, explicit classes).
static JARGON: LazyLock<Regex> = LazyLock::new(|| {
    let words: Vec<String> = JARGON_WORDS.iter().map(|w| w.replace(' ', r"\s+")).collect();
    Regex::new(&format!(r"(?i)(?-u:\b)(?:{})(?-u:\b)", words.join("|"))).unwrap()
});

/// Things that look like code: a path or file name, a camelCase or snake_case word, a backtick, brackets and arrows, a number
/// sign id.
static CODE_SHAPES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    let shapes: [(&str, &str); 8] = [
        ("`", "a backtick"),
        (
            r"(?:^|[\s(])(?:\.{0,2}/)?[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+",
            "a file path",
        ),
        (
            r"(?i)(?-u:\b)[A-Za-z0-9_-]+\.(?:tsx?|jsx?|mjs|json|md|css|html|ya?ml|sh|py)(?-u:\b)",
            "a file name",
        ),
        (r"(?-u:\b)[a-z]+(?:[A-Z][a-z0-9]*)+(?-u:\b)", "a code word (camelCase)"),
        (
            r"(?-u:\b)[A-Z][a-z0-9]+(?:[A-Z][a-z0-9]+)+(?-u:\b)",
            "a code word (PascalCase)",
        ),
        (
            r"(?i)(?-u:\b)[a-z0-9]+(?:_[a-z0-9]+)+(?-u:\b)",
            "a code word (snake_case)",
        ),
        (r"(?i)--[a-z]|=>|::|[{}<>\[\]]|\(\)", "code symbols"),
        (r"#[0-9]+", "a number sign and digits (like a PR number)"),
    ];
    shapes
        .into_iter()
        .map(|(re, why)| (Regex::new(re).unwrap(), why))
        .collect()
});

static YOU: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?-u:\b)(?:you|your|you['’]ll|you['’]re|you['’]ve)(?-u:\b)").unwrap());

/// JavaScript's `length`: UTF-16 units.
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The sentences of `s`: split after `.`, `!` or `?` followed by white space, blank ones dropped.
fn sentences(s: &str) -> usize {
    let mut count = 0;
    let mut current = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        if matches!(c, '.' | '!' | '?') && chars.peek().is_some_and(|n| n.is_whitespace()) {
            count += usize::from(!current.trim().is_empty());
            current.clear();
        }
    }
    count + usize::from(!current.trim().is_empty())
}

/// Whether `raw` is fit to show (the sentence, squeezed to single spaces), or every reason it is not (told to the session for
/// the one rewrite).
pub fn check_sentence(raw: &Value) -> Result<String, Vec<String>> {
    let text = raw.as_str().map(crate::text::squish).unwrap_or_default();
    if text.is_empty() {
        return Err(vec!["it is empty".into()]);
    }
    let mut problems: Vec<String> = Vec::new();
    if js_len(&text) > SENTENCE_MAX_CHARS || text.split_whitespace().count() > SENTENCE_MAX_WORDS {
        problems.push(format!("it is longer than {SENTENCE_MAX_WORDS} words"));
    }
    if sentences(&text) > SENTENCE_MAX_SENTENCES {
        problems.push(format!("it is more than {SENTENCE_MAX_SENTENCES} sentences"));
    }
    for (re, why) in CODE_SHAPES.iter() {
        if re.is_match(&text) {
            problems.push(format!("it has {why}"));
        }
    }
    if let Some(found) = JARGON.find(&text) {
        problems.push(format!("it uses the word \"{}\"", found.as_str().to_lowercase()));
    }
    if !YOU.is_match(&text) {
        problems.push("it does not say what you will see or can do".into());
    }
    if problems.is_empty() {
        return Ok(text);
    }
    let mut seen = Vec::new();
    problems.retain(|p| {
        let new = !seen.contains(p);
        seen.push(p.clone());
        new
    });
    Err(problems)
}

/// How the sentence shown was had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    First,
    Rewritten,
    Fallback,
}

/// The sentence to show: the session's first, else its one rewrite (`rewrite` is told what was wrong and what was written), else
/// [`SAFE_SENTENCE`]. A rewrite that fails or doesn't pass its check is the fallback; the session is asked at most once more.
pub async fn settle_sentence<F, Fut>(first: &Value, rewrite: F) -> (String, How)
where
    F: FnOnce(Vec<String>, String) -> Fut,
    Fut: Future<Output = Option<Value>>,
{
    let problems = match check_sentence(first) {
        Ok(text) => return (text, How::First),
        Err(problems) => problems,
    };
    let was = first.as_str().unwrap_or_default().to_string();
    if let Some(second) = rewrite(problems, was).await
        && let Ok(text) = check_sentence(&second)
    {
        return (text, How::Rewritten);
    }
    (SAFE_SENTENCE.to_string(), How::Fallback)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const GOOD: &str =
        "You'll see a search box above the list, so you can find an idea fast. Everything else stays the same.";

    fn problems(s: &str) -> Vec<String> {
        check_sentence(&json!(s)).unwrap_err()
    }

    #[test]
    fn a_plain_sentence_about_what_you_see_or_can_do_passes_and_so_does_the_fallback() {
        assert!(check_sentence(&json!(GOOD)).is_ok());
        assert!(check_sentence(&json!("You can now pick a colour for each project.")).is_ok());
        assert!(
            check_sentence(&json!(SAFE_SENTENCE)).is_ok(),
            "the fallback passes its own check"
        );
        assert_eq!(check_sentence(&json!("  You   can\n pick  ")).unwrap(), "You can pick");
    }

    #[test]
    fn code_and_process_words_are_refused_with_ts_reasons() {
        let bad = |s: &str, why: &str| {
            let p = problems(s);
            assert!(p.iter().any(|p| p.contains(why)), "{s}: {p:?}");
        };
        bad("You'll see a change in app/src/ui/Board.tsx.", "file");
        bad("You'll see a new Board.tsx.", "file");
        bad("You'll see the needsYouCard change.", "camelCase");
        bad("You'll see the NeedsYouCard change.", "PascalCase");
        bad("You'll see the idea_add button.", "snake_case");
        bad("You'll see a `Save` button.", "backtick");
        bad("You'll see a new component on the board.", "component");
        bad("You'll see it behind a flag.", "flag");
        bad("You'll see it once PR #3904 lands.", "number sign");
        bad("You'll see the tier change.", "tier");
        bad("You can call the API for it.", "api");
        bad("You'll see the new schema.", "schema");
        bad("You'll see a new endpoint.", "endpoint");
        bad("You'll see a refactor of the board.", "refactor");
        bad("The plan adds a button you can press.", "plan");
        bad("You'll see <Button> on the page.", "symbols");
        bad("You'll see a pull   request here.", "pull request");
    }

    #[test]
    fn it_talks_to_the_person_and_is_short() {
        assert!(!check_sentence(&json!("The board shows a new search box.")).is_ok());
        for empty in [json!(""), Value::Null, json!(42)] {
            assert_eq!(check_sentence(&empty).unwrap_err(), ["it is empty"]);
        }
        let long = format!("You'll see {}on the board.", "a very nice thing ".repeat(10));
        assert!(problems(&long).iter().any(|p| p.contains("longer")));
        assert!(
            problems("You'll see a box. You can type in it. It stays open.")
                .iter()
                .any(|p| p.contains("3") || p.contains("more than 2 sentences")),
            "three sentences"
        );
    }

    #[tokio::test]
    async fn a_good_first_sentence_is_used_as_it_is_and_nothing_is_asked_again() {
        let (text, how) = settle_sentence(&json!(GOOD), |_, _| async { panic!("asked again") }).await;
        assert_eq!((text.as_str(), how), (GOOD, How::First));
    }

    #[tokio::test]
    async fn a_bad_sentence_is_sent_back_once_with_what_was_wrong_and_a_good_rewrite_is_used() {
        let told = std::sync::Mutex::new(None);
        let (text, how) = settle_sentence(&json!("The tier-2 component changes."), |p, was| {
            *told.lock().unwrap() = Some((p, was));
            async { Some(json!(GOOD)) }
        })
        .await;
        assert_eq!((text.as_str(), how), (GOOD, How::Rewritten));
        let (p, was) = told.lock().unwrap().take().unwrap();
        assert_eq!(was, "The tier-2 component changes.");
        assert!(p.len() >= 2, "{p:?}");
    }

    #[tokio::test]
    async fn a_rewrite_that_is_still_bad_or_fails_ends_at_the_safe_sentence() {
        let still = settle_sentence(&json!("bad `code`"), |_, _| async { Some(json!("still `bad`")) }).await;
        assert_eq!(still, (SAFE_SENTENCE.to_string(), How::Fallback));
        let offline = settle_sentence(&Value::Null, |_, was| async move {
            assert_eq!(was, "");
            None
        })
        .await;
        assert_eq!(offline, (SAFE_SENTENCE.to_string(), How::Fallback));
    }
}
