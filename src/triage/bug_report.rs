//! Reporting Retaskable's own bugs (docs/specs/bug-reports.md section 3.2; `agent/src/bugReport.ts`
//! is the ground truth): when the triager names a bug in Retaskable itself rather than the
//! project, this is what scrubs the project, its people and its computer out of it, and decides
//! whether it's worth sending, before it ever leaves this one. [`decide_bug_report`] covers
//! `reportBug` up to the message it would send; actually sending it over the cloud link (reading
//! the hub's own `bug_reports { on, known }` and answering with `reportedBug`, writing the
//! signature it sent so a later failure isn't retried) is the orchestrator's, once something runs
//! the triager at all (see the module doc on [`crate::triage`]).
use crate::env::Env;
use std::path::Path;
use std::sync::LazyLock;

use regex::{Match, Regex, RegexBuilder};

use crate::config::ProjectConfig;
use crate::link::{machine_name, truncate};

use super::{KnownBug, RetaskableBug};

/// bug-reports.md section 3.2: the limits a signature, a title and the details are cut to.
pub const BUG_SIGNATURE_MAX: usize = 200;
pub const BUG_TITLE_MAX: usize = 120;
pub const BUG_DETAILS_MAX: usize = 2000;

/// Shorter words aren't scrubbed: they'd take ordinary words with them and name nothing.
const MIN_WORD: usize = 3;

/// Something that names the project, its person or its computer, and what it becomes in a
/// report: `<project>`, `<repo>`, `<path>`, `<branch>`, `<item>`, `<computer>`, `<person>`.
pub type ScrubWord = (Option<String>, &'static str);

/// What a `retaskableBug` the triager named becomes once scrubbed, ready for the orchestrator to
/// send once it can. `None` when there's nothing worth sending: no title, or the project's own
/// words scrub it down to nothing (`bugReport.ts`'s `!signature || !title`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedBugReport {
    /// The same on every computer for the same bug (`bugSignature`), so it's one item on
    /// Retaskable's board however many agents hit it.
    pub signature: String,
    pub title: String,
    pub details: String,
}

/// Scrubs and signs a bug the triager named (06.9 bullet 18, `bugReport.ts`'s `reportBug` up to
/// its first await). `needs_human_reason` is the item's own reason it's `needs_human`, preferred
/// over the bug's title for the signature so the same underlying failure signs the same whether
/// the triager worded the bug differently each time.
pub fn prepare_bug_report(
    bug: &RetaskableBug,
    needs_human_reason: Option<&str>,
    words: &[ScrubWord],
) -> Option<PreparedBugReport> {
    if bug.title.trim().is_empty() {
        return None;
    }
    let signature_source = needs_human_reason
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(bug.title.trim());
    let signature = bug_signature(&scrub(signature_source, words, None));
    let title = scrub(&collapse_whitespace(&bug.title), words, Some(BUG_TITLE_MAX));
    if signature.is_empty() || title.is_empty() {
        return None;
    }
    let details = scrub(&bug.details, words, Some(BUG_DETAILS_MAX));
    Some(PreparedBugReport {
        signature,
        title,
        details,
    })
}

/// A scrubbed, signed bug the orchestrator has decided is worth sending: everything `bug_report`
/// needs besides its `ref` and `version`, which are the orchestrator's to attach (`BugReportMessage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BugReportMessage {
    pub signature: String,
    pub title: String,
    pub details: String,
    /// The id of a [`KnownBug`] this is the same bug as, worded differently, only when it's one
    /// the hub actually listed.
    pub same_as: Option<String>,
}

/// Decides whether a `retaskableBug` the triager named is worth sending (06.9 bullet 18,
/// `bugReport.ts`'s `reportBug` up to the message it builds): `None` when there's no bug, the
/// project's hub hasn't said its person agreed (`consent_on`), nothing's left once it's scrubbed
/// ([`prepare_bug_report`] returning `None`), or this exact signature is what `already_reported`
/// (the item's own `reportedBug`) already sent — the same bug is reported once per item, answered
/// or not, so a retry never sends it again. `known_bugs` is the hub's own `bug_reports.known`;
/// `bug.same_as` only carries through when it names one of them, same as `reportBug`'s
/// `deps.known().find((b) => b.id === bug.sameAs)`.
pub fn decide_bug_report(
    bug: Option<&RetaskableBug>,
    needs_human_reason: Option<&str>,
    words: &[ScrubWord],
    consent_on: bool,
    already_reported: Option<&str>,
    known_bugs: &[KnownBug],
) -> Option<BugReportMessage> {
    let bug = bug?;
    if !consent_on {
        return None;
    }
    let prepared = prepare_bug_report(bug, needs_human_reason, words)?;
    if already_reported == Some(prepared.signature.as_str()) {
        return None;
    }
    let same_as = bug
        .same_as
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .and_then(|id| known_bugs.iter().find(|b| b.id == id))
        .map(|b| b.id.clone());
    Some(BugReportMessage {
        signature: prepared.signature,
        title: prepared.title,
        details: prepared.details,
        same_as,
    })
}

/// What names this project, its person and its computer in a report (`scrubWords`):
/// `project_id` is the project's own id as the caller resolved it, `cfg` its settings, and
/// `item_id`/`item_branch` the item's. `env` reads the environment (`USERPROFILE`/`HOME` for the
/// home folder, `USERNAME`/`USER` for the person), the same way the rest of this crate does.
pub fn scrub_words(
    project_id: &str,
    cfg: &ProjectConfig,
    item_id: &str,
    item_branch: Option<&str>,
    env: &Env,
) -> Vec<ScrubWord> {
    let home = env.get("USERPROFILE").or_else(|| env.get("HOME"));
    let user = env.get("USERNAME").or_else(|| env.get("USER"));
    let computer = Some(machine_name(env));
    let (owner, repo) = match cfg.github_repo.split_once('/') {
        Some((owner, repo)) => (Some(owner.to_string()), Some(repo.to_string())),
        None => (None, None),
    };
    let repo_basename = Path::new(&cfg.repo_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());

    vec![
        (home, "<path>"),
        (Some(cfg.worktrees_dir.clone()), "<path>"),
        (Some(cfg.repo_path.clone()), "<path>"),
        (Some(cfg.github_repo.clone()), "<repo>"),
        (owner, "<repo>"),
        (repo, "<repo>"),
        (Some(project_id.to_string()), "<project>"),
        (cfg.id.clone(), "<project>"),
        (repo_basename, "<project>"),
        (Some(cfg.base_branch.clone()), "<branch>"),
        (item_branch.map(str::to_string), "<branch>"),
        (Some(item_id.to_string()), "<item>"),
        (computer, "<computer>"),
        (user, "<person>"),
    ]
}

/// The bug's one-line signature, the same on every computer for the same bug: paths, numbers,
/// hex ids and quoted strings (other than ones holding a `{{name}}`) replaced, at most
/// `BUG_SIGNATURE_MAX` characters. Pure (`bugSignature`).
pub fn bug_signature(text: &str) -> String {
    let line = collapse_whitespace(text);
    let line = line.trim();
    let line = replace_quoted(line);
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    for m in PLACEHOLDER.find_iter(&line) {
        out.push_str(&transform_outside_placeholder(&line[last..m.start()]));
        out.push_str(m.as_str());
        last = m.end();
    }
    out.push_str(&transform_outside_placeholder(&line[last..]));
    truncate(&out, BUG_SIGNATURE_MAX).trim().to_string()
}

fn transform_outside_placeholder(s: &str) -> String {
    let s = replace_absolute_paths(s);
    let s = replace_uuid(&s);
    let s = replace_hex(&s);
    replace_number(&s)
}

/// `text` with `words` replaced, longest first and in any case: the ones with a slash first
/// (folders, the repo slug, branches; either slash), then any other absolute path, then the rest
/// as whole words. Cut to `max` characters, if given. Pure (`scrub`).
pub fn scrub(text: &str, words: &[ScrubWord], max: Option<usize>) -> String {
    let mut known: Vec<(&str, &str)> = words
        .iter()
        .filter_map(|(find, as_)| find.as_deref().map(|f| (f.trim(), *as_)))
        .filter(|(f, _)| f.chars().count() >= MIN_WORD)
        .collect();
    known.sort_by_key(|(f, _)| std::cmp::Reverse(f.len()));

    let mut out = text.to_string();
    for (find, as_) in known.iter().filter(|(f, _)| is_slashed(f)) {
        out = replace_slashed_word(&out, find, as_);
    }
    out = replace_absolute_paths(&out);
    for (find, as_) in known.iter().filter(|(f, _)| !is_slashed(f)) {
        out = replace_bounded_word(&out, find, as_);
    }
    let out = match max {
        Some(max) => truncate(&out, max),
        None => out,
    };
    out.trim().to_string()
}

fn is_slashed(s: &str) -> bool {
    s.contains('\\') || s.contains('/')
}

fn collapse_whitespace(s: &str) -> String {
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

/// Quoted text, replaced with `"…"` unless it holds a `{{name}}` placeholder: a single quote
/// only where it can't be an apostrophe (not preceded by a letter). Rust's `regex` crate has no
/// lookbehind, so this walks the text itself rather than porting `QUOTED` as one pattern.
fn replace_quoted(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let opens_quote = match c {
            '"' | '`' => true,
            '\'' => i == 0 || !chars[i - 1].is_ascii_alphabetic(),
            _ => false,
        };
        if opens_quote && let Some(end) = find_closing_quote(&chars, i + 1, c) {
            let inner: String = chars[i + 1..end].iter().collect();
            if PLACEHOLDER.is_match(&inner) {
                out.extend(&chars[i..=end]);
            } else {
                out.push_str("\"\u{2026}\"");
            }
            i = end + 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn find_closing_quote(chars: &[char], start: usize, quote: char) -> Option<usize> {
    (start..chars.len())
        .take_while(|&j| chars[j] != '\n')
        .find(|&j| chars[j] == quote)
}

/// A `{{name}}` prompt variable: the bug is often that one wasn't filled, so it stays in the
/// signature, quoted or not.
static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{\{[0-9A-Za-z_]+\}\}").unwrap());

static UUID: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
        .case_insensitive(true)
        .build()
        .unwrap()
});

fn replace_uuid(s: &str) -> String {
    UUID.replace_all(s, "<id>").into_owned()
}

/// Maximal runs of hex digits; `replace_hex` keeps only the ones of least 7 digits, with at
/// least one actual digit among them, that aren't touching another alphanumeric on either side
/// (`HEX`'s lookaround, checked by hand instead of in the pattern).
static HEX_RUN: LazyLock<Regex> =
    LazyLock::new(|| RegexBuilder::new(r"[0-9a-f]+").case_insensitive(true).build().unwrap());

fn replace_hex(s: &str) -> String {
    replace_filtered(s, &HEX_RUN, "<id>", |text, m| {
        let run = m.as_str();
        run.len() >= 7
            && run.bytes().any(|b| b.is_ascii_digit())
            && !char_before(text, m).is_some_and(is_ascii_alnum)
            && !char_after(text, m).is_some_and(is_ascii_alnum)
    })
}

/// Numbers, not the digits in a word like `utf8` or `e2e` (`NUMBER`'s lookbehind, checked by
/// hand: a run of digits touching an alphanumeric on its left isn't a number of its own).
static NUMBER_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9]+(?:\.[0-9]+)*").unwrap());

fn replace_number(s: &str) -> String {
    replace_filtered(s, &NUMBER_RUN, "<n>", |text, m| {
        !char_before(text, m).is_some_and(is_ascii_alnum)
    })
}

/// Absolute paths: Windows (`C:\...`, `C:/...`), UNC (`\\server\...`), POSIX (`/...`) and home
/// (`~/...`). The quantified tail is a plain regex quantifier (no lookaround needed for it); only
/// which alternative opened the path, and its one boundary check, are done by hand.
const PATH_CHAR_CLASS: &str = r#"[^\s"'`<>|()\[\]{},;]"#;

static ABSOLUTE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(r"(?:(?i:[a-z]):[\\/]|\\\\|~[\\/]|/){PATH_CHAR_CLASS}*");
    Regex::new(&pattern).unwrap()
});

fn replace_absolute_paths(s: &str) -> String {
    replace_filtered(s, &ABSOLUTE_PATH, "<path>", |text, m| {
        let found = m.as_str();
        let before = char_before(text, m);
        if found.as_bytes().get(1) == Some(&b':') {
            // A drive letter: not one if the letter itself is part of a longer word.
            !before.is_some_and(|c| c.is_ascii_alphabetic())
        } else if found.starts_with('/') {
            // POSIX: not a path separator inside a word, number, URL or another path; and there
            // has to be something path-like after the slash, or it's just a stray `/`.
            let boundary_before = before.is_some_and(is_posix_path_boundary);
            let has_tail = found.chars().nth(1).is_some_and(is_path_char);
            !boundary_before && has_tail
        } else {
            // `\\...` (UNC) or `~/...` (home): unambiguous enough to need no boundary check.
            true
        }
    })
}

fn is_ascii_alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

fn is_posix_path_boundary(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || matches!(c, '.' | ':' | '/' | '\\' | '<' | '>' | '~' | '-')
}

fn is_path_char(c: char) -> bool {
    !(c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '`' | '<' | '>' | '|' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
        ))
}

fn is_word_boundary_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn char_before(text: &str, m: &Match<'_>) -> Option<char> {
    text[..m.start()].chars().last()
}

fn char_after(text: &str, m: &Match<'_>) -> Option<char> {
    text[m.end()..].chars().next()
}

/// Replaces every match of `re` in `text` with `replacement`, except the ones `accept` rejects,
/// which are left as they were.
fn replace_filtered(text: &str, re: &Regex, replacement: &str, accept: impl Fn(&str, &Match<'_>) -> bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in re.find_iter(text) {
        if accept(text, &m) {
            out.push_str(&text[last..m.start()]);
            out.push_str(replacement);
            last = m.end();
        }
    }
    out.push_str(&text[last..]);
    out
}

/// A known word, not a slash in it: replaced only as a whole word (`(?<![A-Za-z0-9_])...(?![A-Za-z0-9_])`,
/// checked by hand).
fn replace_bounded_word(text: &str, find: &str, as_: &str) -> String {
    let Ok(re) = RegexBuilder::new(&regex::escape(find)).case_insensitive(true).build() else {
        return text.to_string();
    };
    replace_filtered(text, &re, as_, |text, m| {
        !char_before(text, m).is_some_and(is_word_boundary_char)
            && !char_after(text, m).is_some_and(is_word_boundary_char)
    })
}

/// A known word with a slash in it (a folder, the repo slug, a branch): matched whichever slash
/// direction the text uses, with no word-boundary check (its separators are boundary enough).
fn replace_slashed_word(text: &str, find: &str, as_: &str) -> String {
    let pattern = find
        .split(['\\', '/'])
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join(r"[\\/]");
    let Ok(re) = RegexBuilder::new(&pattern).case_insensitive(true).build() else {
        return text.to_string();
    };
    re.replace_all(text, as_).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // bug_signature

    #[test]
    fn the_same_bug_with_different_numbers_paths_ids_and_quoted_text_is_one_signature() {
        let a = bug_signature(
            "Session 12 stopped after 3.5 s at C:\\Users\\ann\\game\\src\\a.ts:40:7\n  commit 1a2b3c4d5e, run 0f8e1a2b-3c4d-4e5f-8a9b-0c1d2e3f4a5b, file 'level.json'",
        );
        let b = bug_signature(
            "Session 7 stopped after 12.25 s at /home/bob/site/lib/b.ts:2:1 commit 9f9f9f9, run 11111111-2222-4333-8444-555555555555, file \"other.json\"",
        );
        assert_eq!(a, b);
        assert_eq!(
            a,
            "Session <n> stopped after <n> s at <path> commit <id>, run <id>, file \"\u{2026}\""
        );
    }

    #[test]
    fn placeholders_are_kept_quoted_or_not_and_words_with_digits_stay_words() {
        assert_eq!(
            bug_signature("Prompt variable {{codeLimits}} not provided"),
            "Prompt variable {{codeLimits}} not provided"
        );
        assert_eq!(
            bug_signature("Missing \"{{item2}}\" in utf8 e2e prompt, it's \"x\""),
            "Missing \"{{item2}}\" in utf8 e2e prompt, it's \"\u{2026}\""
        );
        assert_eq!(bug_signature(&"x".repeat(500)).len(), BUG_SIGNATURE_MAX);
        assert_eq!(bug_signature("a  \n\t b"), "a b");
    }

    // scrub

    fn words() -> Vec<ScrubWord> {
        vec![
            (Some("C:\\Users\\ann".to_string()), "<path>"),
            (Some("acme/void-miner".to_string()), "<repo>"),
            (Some("acme".to_string()), "<repo>"),
            (Some("void-miner".to_string()), "<repo>"),
            (Some("void-miner".to_string()), "<project>"),
            (Some("main".to_string()), "<branch>"),
            (Some("retaskable/level-select".to_string()), "<branch>"),
            (Some("ab".to_string()), "<project>"),
            (None, "<project>"),
        ]
    }

    #[test]
    fn the_project_its_repo_branches_home_folder_and_any_absolute_path_are_taken_out_then_cut() {
        let text = "Void-Miner on acme/void-miner (branch retaskable/level-select, base main) failed in c:/users/ann/My Game/x.ts and /home/ann/void-miner/y.ts, also D:\\work\\z.ts; mainly about tab";
        assert_eq!(
            scrub(text, &words(), None),
            "<repo> on <repo> (branch <branch>, base <branch>) failed in <path>/My Game/x.ts and <path>, also <path>; mainly about tab"
        );
        assert_eq!(scrub(&"x".repeat(300), &[], Some(120)).len(), 120);
        assert_eq!(
            scrub("see https://github.com/acme/void-miner/pull/3", &words(), None),
            "see https://github.com/<repo>/pull/3"
        );
    }

    // scrub_words

    #[test]
    fn names_the_projects_words_repo_branches_folders_item_computer_and_home() {
        let cfg = ProjectConfig {
            github_repo: "acme/void-miner".to_string(),
            repo_path: "/tmp/vm".to_string(),
            worktrees_dir: "/tmp/wt".to_string(),
            base_branch: "main".to_string(),
            ..Default::default()
        };
        let env = Env::from_pairs([("HOME", "/home/ann"), ("USER", "ann")]);
        let found = scrub_words("p1", &cfg, "level-select", Some("retaskable/level-select"), &env);
        let has = |find: &str, as_: &str| found.iter().any(|(f, a)| f.as_deref() == Some(find) && *a == as_);
        assert!(has("acme/void-miner", "<repo>"));
        assert!(has("acme", "<repo>"));
        assert!(has("void-miner", "<repo>"));
        assert!(has("p1", "<project>"));
        assert!(has("vm", "<project>"));
        assert!(has("main", "<branch>"));
        assert!(has("retaskable/level-select", "<branch>"));
        assert!(has("level-select", "<item>"));
        assert!(has("/home/ann", "<path>"));
        assert!(has("ann", "<person>"));
    }

    // prepare_bug_report

    fn bug() -> RetaskableBug {
        RetaskableBug {
            title: "Retaskable didn't fill {{codeLimits}} for void-miner".to_string(),
            details: "In C:\\code\\void-miner\\prompts it stopped on acme/void-miner's main.".to_string(),
            same_as: None,
        }
    }

    fn harness_words() -> Vec<ScrubWord> {
        vec![
            (Some("acme/void-miner".to_string()), "<repo>"),
            (Some("acme".to_string()), "<repo>"),
            (Some("void-miner".to_string()), "<repo>"),
            (Some("C:\\code\\void-miner".to_string()), "<path>"),
            (Some("main".to_string()), "<branch>"),
        ]
    }

    #[test]
    fn scrubbed_and_signed_by_the_items_own_reason_when_it_has_one() {
        let prepared = prepare_bug_report(
            &bug(),
            Some("Prompt variable {{codeLimits}} not provided"),
            &harness_words(),
        )
        .unwrap();
        assert_eq!(prepared.signature, "Prompt variable {{codeLimits}} not provided");
        assert_eq!(prepared.title, "Retaskable didn't fill {{codeLimits}} for <repo>");
        assert_eq!(prepared.details, "In <path>\\prompts it stopped on <repo>'s <branch>.");
    }

    #[test]
    fn signs_by_the_bugs_own_title_when_the_item_has_no_reason() {
        let prepared = prepare_bug_report(&bug(), None, &harness_words()).unwrap();
        assert_eq!(prepared.signature, "Retaskable didn't fill {{codeLimits}} for <repo>");
    }

    #[test]
    fn nothing_without_a_title() {
        let bug = RetaskableBug {
            title: "   ".to_string(),
            details: String::new(),
            same_as: None,
        };
        assert!(prepare_bug_report(&bug, Some("reason"), &[]).is_none());
    }

    // decide_bug_report

    #[test]
    fn nothing_with_no_bug_no_consent_or_nothing_left_once_scrubbed() {
        assert!(decide_bug_report(None, Some("reason"), &harness_words(), true, None, &[]).is_none());
        assert!(decide_bug_report(Some(&bug()), Some("reason"), &harness_words(), false, None, &[]).is_none());
        let empty = RetaskableBug {
            title: " ".to_string(),
            details: String::new(),
            same_as: None,
        };
        assert!(decide_bug_report(Some(&empty), Some("reason"), &harness_words(), true, None, &[]).is_none());
    }

    #[test]
    fn nothing_when_this_items_already_reported_bug_has_the_same_signature() {
        let reason = "Prompt variable {{codeLimits}} not provided";
        assert!(decide_bug_report(Some(&bug()), Some(reason), &harness_words(), true, Some(reason), &[]).is_none());
    }

    #[test]
    fn a_decided_report_carries_through_a_same_as_the_hub_actually_listed() {
        let mut named = bug();
        named.same_as = Some("retaskable-bug-abc123".to_string());
        let known = [KnownBug {
            id: "retaskable-bug-abc123".to_string(),
            title: "A known bug".to_string(),
        }];
        let decided = decide_bug_report(Some(&named), Some("reason"), &harness_words(), true, None, &known).unwrap();
        assert_eq!(decided.same_as, Some("retaskable-bug-abc123".to_string()));
    }

    #[test]
    fn a_same_as_the_hub_never_listed_is_dropped() {
        let mut named = bug();
        named.same_as = Some("retaskable-bug-not-listed".to_string());
        let decided = decide_bug_report(Some(&named), Some("reason"), &harness_words(), true, None, &[]).unwrap();
        assert_eq!(decided.same_as, None);
    }

    #[test]
    fn decides_to_send_the_scrubbed_bug_when_nothing_blocks_it() {
        let decided = decide_bug_report(
            Some(&bug()),
            Some("Prompt variable {{codeLimits}} not provided"),
            &harness_words(),
            true,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(decided.signature, "Prompt variable {{codeLimits}} not provided");
        assert_eq!(decided.title, "Retaskable didn't fill {{codeLimits}} for <repo>");
        assert_eq!(decided.same_as, None);
    }
}
