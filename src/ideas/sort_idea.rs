//! Sorting what the plus box sent (docs/specs/idea-sort.md, TS `shared/sortIdea.ts` and `agent/src/sortIdea.ts`): the cheap
//! check that settles the obvious ([`rule_sort`]), the closed answer one read-only session gives for the rest
//! ([`read_answer`], checked again in plain code), and the fallback for a computer that cannot answer ([`later`]). The model
//! decides nothing but the answer's fields, and `unsure` is never guessed into a side. The person's own words are never
//! changed: `title` is only a label.
use std::future::Future;
use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::design_board::DesignError;
use crate::engines::engine::{EngineId, LocalTarget, SessionSpec, ephemeral_readonly_spec};
use crate::prompt::render_prompt;
use crate::text::truncate;

const TEMPLATE: &str = include_str!("../../prompts/sort-idea.md");

/// The most things one message may be split into, and the longest piece.
const SPLIT_MAX: usize = 4;
const SPLIT_TEXT_MAX: usize = 400;

/// The side an idea is sorted to; `Later` is an idea no computer could sort yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKind {
    Idea,
    Bug,
    Unsure,
    Later,
}

impl SortKind {
    pub fn name(self) -> &'static str {
        match self {
            SortKind::Idea => "idea",
            SortKind::Bug => "bug",
            SortKind::Unsure => "unsure",
            SortKind::Later => "later",
        }
    }
}

/// What `idea_sort` carries: the side, who decided (`rule` or `agent`), and the answer's other fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeaSort {
    pub kind: SortKind,
    pub by_rule: bool,
    pub reason: Option<String>,
    pub title: Option<String>,
    /// Whether people will see the change; none for a sort by rule (the plan's own test decides).
    pub visible: Option<bool>,
    pub split: Option<Vec<String>>,
}

impl IdeaSort {
    /// The op's args (`idea_sort { id, kind, by, reason?, title?, visible?, split? }`).
    pub fn args(&self, id: &str) -> Value {
        let mut a = Map::new();
        a.insert("id".into(), json!(id));
        a.insert("kind".into(), json!(self.kind.name()));
        a.insert("by".into(), json!(if self.by_rule { "rule" } else { "agent" }));
        if let Some(r) = &self.reason {
            a.insert("reason".into(), json!(r));
        }
        if let Some(t) = &self.title {
            a.insert("title".into(), json!(t));
        }
        if let Some(v) = self.visible {
            a.insert("visible".into(), json!(v));
        }
        if let Some(s) = &self.split {
            a.insert("split".into(), json!(s));
        }
        Value::Object(a)
    }
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap()
}

static BROKEN: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(crash(?:es|ed|ing)?|doesn['’]?t work|does not work|isn['’]?t working|not working|stopped working|broken|breaks|keeps? (?:failing|freezing)|freez(?:es|ing)|won['’]?t (?:load|open|save|start|send)|can['’]?t (?:open|save|log ?in|sign ?in|tap|press|click)|nothing happens|does nothing|error|bug|wrong)\b",
    )
});
static WISH: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)(?:^|[.!?,;:]\s+|\s[-–—]\s+|\b(?:and|but|so)\s+)(?:please\s+)?(?:add|what if|i wish|wish|it would be (?:nice|great|cool)|could we|can we|let me|allow|make it possible|support|new)\b|\bwhat if\b|\bi wish\b",
    )
});
/// More than one thing: a new line, a list, "also", or several sentences, which the session reads for a split.
static SEVERAL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\n\s*(?:[-*•]|\d+[.)])\s|\b(?:also|and another thing|another thing|plus)\b|[.!?]\s+\S[^.!?]*[.!?]\s+\S")
});

/// The sides the cheap check settles by itself: a clear break or a clear wish, and only one of them, in one thing. Else `None`.
pub fn pre_sort(text: &str) -> Option<(SortKind, &'static str)> {
    let t = text.trim();
    if t.is_empty() || SEVERAL.is_match(t) {
        return None;
    }
    match (BROKEN.is_match(t), WISH.is_match(t)) {
        (true, false) => Some((SortKind::Bug, "it says something is broken")),
        (false, true) => Some((SortKind::Idea, "it asks for something new")),
        _ => None,
    }
}

/// A short label from the person's own words: the first line, cut at a word, 80 characters at most.
pub fn label_of(text: &str) -> String {
    let first = text.trim().lines().next().unwrap_or("");
    let first = first.split_whitespace().collect::<Vec<_>>().join(" ");
    if first.chars().count() <= 80 {
        return first;
    }
    let head: String = first.chars().take(79).collect();
    let cut = match head.rfind(' ') {
        Some(at) if head[..at].chars().count() > 20 => &head[..at],
        _ => head.as_str(),
    };
    format!("{}…", cut.trim())
}

/// What the cheap check settles, as it is kept: sorted by rule, with no `visible`.
pub fn rule_sort(text: &str) -> Option<IdeaSort> {
    pre_sort(text).map(|(kind, reason)| IdeaSort {
        kind,
        by_rule: true,
        reason: Some(reason.into()),
        title: Some(label_of(text)),
        visible: None,
        split: None,
    })
}

/// A computer that cannot answer: an idea, sorted later. Never a guess at a bug.
pub fn later() -> IdeaSort {
    IdeaSort {
        kind: SortKind::Later,
        by_rule: true,
        reason: None,
        title: None,
        visible: None,
        split: None,
    }
}

/// An idea with a picture is read by its words alone, which may say "see the picture": a person says which it is.
pub fn pictured(title: &str) -> IdeaSort {
    IdeaSort {
        kind: SortKind::Unsure,
        by_rule: true,
        reason: Some("it has a picture to look at".into()),
        title: Some(label_of(title)),
        visible: None,
        split: None,
    }
}

/// One line of words: no line breaks, trimmed, 1 to `max` characters.
fn line(v: &Value, max: usize) -> Option<String> {
    let s = v.as_str()?.trim();
    (!s.is_empty() && s.encode_utf16().count() <= max && !s.contains(['\r', '\n'])).then(|| s.to_string())
}

/// The session's answer, checked (`readSortAnswer`): a failed check is `None`, so the caller sorts later.
pub fn read_answer(raw: &Value) -> Option<IdeaSort> {
    let o = raw.as_object()?;
    if o.keys()
        .any(|k| !matches!(k.as_str(), "kind" | "reason" | "title" | "visible" | "split"))
    {
        return None;
    }
    let kind = match o.get("kind")?.as_str()? {
        "idea" => SortKind::Idea,
        "bug" => SortKind::Bug,
        "unsure" => SortKind::Unsure,
        _ => return None,
    };
    let reason = line(o.get("reason")?, 120)?;
    let title = line(o.get("title")?, 80)?;
    let visible = o.get("visible")?.as_bool()?;
    let split = match o.get("split") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let parts = v.as_array()?;
            if parts.len() < 2 || parts.len() > SPLIT_MAX {
                return None;
            }
            Some(
                parts
                    .iter()
                    .map(|p| line(p, SPLIT_TEXT_MAX))
                    .collect::<Option<Vec<_>>>()?,
            )
        }
    };
    Some(IdeaSort {
        kind,
        by_rule: false,
        reason: Some(reason),
        title: Some(title),
        visible: Some(visible),
        split,
    })
}

/// The structure the session answers in (`SORT_SCHEMA`).
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": { "type": "string", "enum": ["idea", "bug", "unsure"] },
            "reason": { "type": "string" },
            "title": { "type": "string" },
            "visible": { "type": "boolean" },
            "split": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["kind", "reason", "title", "visible"]
    })
}

/// The prompt: the person's words, cut at 4000 characters.
pub fn prompt(text: &str) -> anyhow::Result<String> {
    render_prompt(TEMPLATE, &[("text", truncate(text, 4000))])
}

/// The session's `SessionSpec`: the design board's kind (read-only, fresh, no shell, a scratch directory), answering [`schema`].
pub fn spec(prompt: String, model: &str, budget_usd: f64, cwd: PathBuf, local: Option<LocalTarget>) -> SessionSpec {
    ephemeral_readonly_spec(
        Some(EngineId::Claude),
        model.to_string(),
        prompt,
        schema(),
        cwd,
        budget_usd,
        None,
        local,
        false,
        false,
    )
}

/// Sorts `text`: by rule when it is obvious, else by the session `ask` (its prompt in, its structured answer out); [`later`] when
/// no answer could be had.
pub async fn sort_text<F, Fut>(text: &str, ask: F) -> IdeaSort
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<Value, DesignError>>,
{
    if let Some(sorted) = rule_sort(text) {
        return sorted;
    }
    let Ok(prompt) = prompt(text) else {
        return later();
    };
    match ask(prompt).await {
        Ok(raw) => read_answer(&raw).unwrap_or_else(later),
        Err(_) => later(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(text: &str) -> Option<SortKind> {
        pre_sort(text).map(|(k, _)| k)
    }

    #[test]
    fn broken_words_alone_are_a_bug_and_wish_words_alone_an_idea() {
        assert_eq!(kind("The save button doesn't work"), Some(SortKind::Bug));
        assert_eq!(kind("App crashes when I open it"), Some(SortKind::Bug));
        assert_eq!(kind("Add a dark mode"), Some(SortKind::Idea));
        assert_eq!(kind("What if it could snooze"), Some(SortKind::Idea));
        assert_eq!(kind("it would be nice to have tabs"), Some(SortKind::Idea));
    }

    #[test]
    fn both_neither_a_list_also_or_several_sentences_go_to_the_session() {
        assert_eq!(kind("Add a snooze button, the old one is broken"), None);
        assert_eq!(kind("The header looks nice"), None);
        assert_eq!(kind("Fix the logo\n- and the colours"), None);
        assert_eq!(kind("It crashes. Also the menu is slow"), None);
        assert_eq!(kind("It crashes. It is bad. Very bad."), None);
        assert_eq!(kind("   "), None);
    }

    #[test]
    fn a_rule_sort_has_a_label_and_no_visible() {
        let s = rule_sort("Add a dark mode").unwrap();
        assert_eq!(
            (s.kind, s.by_rule, s.visible, s.title.as_deref()),
            (SortKind::Idea, true, None, Some("Add a dark mode"))
        );
        assert_eq!(
            s.args("i1"),
            json!({ "id": "i1", "kind": "idea", "by": "rule", "reason": "it asks for something new", "title": "Add a dark mode" })
        );
    }

    #[test]
    fn the_label_is_the_first_line_cut_at_a_word() {
        assert_eq!(label_of("  one\ntwo "), "one");
        let long = "word ".repeat(30);
        let l = label_of(&long);
        assert!(
            l.chars().count() <= 80 && l.ends_with('…') && !l.contains("wor…"),
            "{l}"
        );
    }

    fn good() -> Value {
        json!({ "kind": "bug", "reason": "fits the Flags screen", "title": "Save is dead", "visible": true })
    }

    #[test]
    fn a_good_answer_is_kept_with_visible_and_a_split() {
        let s = read_answer(&good()).unwrap();
        assert_eq!((s.kind, s.by_rule, s.visible), (SortKind::Bug, false, Some(true)));
        let mut with = good();
        with["split"] = json!(["One thing", "Another thing"]);
        let s = read_answer(&with).unwrap();
        assert_eq!(s.args("i")["split"], json!(["One thing", "Another thing"]));
        assert_eq!(s.args("i")["visible"], json!(true));
    }

    #[test]
    fn an_answer_that_fails_its_check_is_no_answer() {
        let with = |k: &str, v: Value| {
            let mut a = good();
            a[k] = v;
            read_answer(&a)
        };
        assert!(with("reason", json!("two\nlines")).is_none());
        assert!(with("reason", json!("x".repeat(121))).is_none());
        assert!(with("title", json!("")).is_none());
        assert!(with("kind", json!("later")).is_none());
        assert!(with("visible", json!("yes")).is_none());
        assert!(with("split", json!(["only one"])).is_none());
        assert!(with("split", json!(["a", "b", "c", "d", "e"])).is_none());
        assert!(with("extra", json!(1)).is_none());
        let mut missing = good();
        missing.as_object_mut().unwrap().remove("visible");
        assert!(read_answer(&missing).is_none());
    }

    #[tokio::test]
    async fn the_session_is_asked_only_for_what_the_rule_cannot_settle_and_a_failure_sorts_later() {
        let asked = std::cell::Cell::new(0);
        let s = sort_text("Add a dark mode", |_| {
            asked.set(asked.get() + 1);
            async { Ok(good()) }
        })
        .await;
        assert_eq!((s.kind, asked.get()), (SortKind::Idea, 0));
        let s = sort_text("The header is odd", |p| async move {
            assert!(p.contains("The header is odd") && !p.contains("{{"));
            Ok(good())
        })
        .await;
        assert_eq!((s.kind, s.by_rule), (SortKind::Bug, false));
        let s = sort_text("The header is odd", |_| async { Err(DesignError::conflict("no")) }).await;
        assert_eq!(s, later());
        let s = sort_text("The header is odd", |_| async { Ok(json!({ "kind": "bug" })) }).await;
        assert_eq!(s, later());
        assert_eq!(later().args("i"), json!({ "id": "i", "kind": "later", "by": "rule" }));
    }

    #[test]
    fn the_session_is_read_only_with_no_shell_and_asks_for_the_closed_answer() {
        let s = spec("p".into(), "haiku", 1.0, std::env::temp_dir(), None);
        assert!(s.readonly && !s.shell);
        assert_eq!(s.schema["required"], json!(["kind", "reason", "title", "visible"]));
        assert!(pictured("A long\nthing").reason.is_some());
    }
}
