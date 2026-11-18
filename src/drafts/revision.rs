//! Changes stay on their card (docs/specs/agent/06.5-spec-drafts.md, "Revisions"; Retaskable.com's card-revisions.md
//! and `agent/src/revisions.ts`): a plan that revises a backlog item is told about it, the drafter's `revision` is
//! kept only when it names that item, and a fix asked on the card reopens the item. Plain code: the drafter only
//! names the item.
use serde::Deserialize;
use serde_json::Value;

use crate::text::truncate;

/// The most a revision's `remaining` keeps (the store's own limit).
pub const REMAINING_MAX: usize = 4000;

/// The drafter's `revision` as it wrote it: [`to_revision`] decides what is kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RawRevision {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub remaining: Option<String>,
}

/// What the drafter's `revision` came to for a plan that names an item to revise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Revised {
    /// The plan revises its item: what is left of it once the change is in.
    Keeps { remaining: String },
    /// None, or one for another item: the plan is new work after all, and its `revises` is cleared.
    NewWork,
}

/// `toRevision`: the drafter's `revision`, kept only when it names the item the plan revises and says what is left.
pub fn to_revision(raw: Option<&RawRevision>, revises: &str) -> Revised {
    match raw {
        Some(r) if r.id.as_deref().map(str::trim) == Some(revises) => {
            let remaining = r.remaining.as_deref().map(str::trim).unwrap_or("");
            if remaining.is_empty() {
                Revised::NewWork
            } else {
                Revised::Keeps {
                    remaining: truncate(remaining, REMAINING_MAX),
                }
            }
        }
        _ => Revised::NewWork,
    }
}

/// The item a draft revises, as the store's `revises` says (a string; `null`, `""` or a missing one is none).
pub fn revises_of(other: &serde_json::Map<String, Value>) -> Option<String> {
    other
        .get("revises")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(String::from)
}

/// An item's state in a few words, for the prompt (`stateWords`): its status, its slice and who is on it.
fn state_words(item: &Value) -> String {
    let status = item["status"].as_str().unwrap_or("").replacen('_', " ", 1);
    let detail = item["detail"]
        .as_str()
        .filter(|d| !d.is_empty())
        .map_or(String::new(), |d| format!(" ({d})"));
    let slice = item["slice"].as_u64().unwrap_or(0);
    let on = if !item["lease"].is_null() {
        "a session is on it".to_string()
    } else if let Some(w) = item["waiting"]["for"].as_str() {
        format!("waiting for {w}")
    } else {
        "nobody is on it".to_string()
    };
    let why = item["waitingFor"]
        .as_str()
        .filter(|w| !w.is_empty())
        .map_or(String::new(), |w| format!(": {w}"));
    format!(
        "{status}{detail}, {slice} slice{} built, {on}{why}",
        if slice == 1 { "" } else { "s" }
    )
}

/// The drafter's block for the item it revises (`revisionText`), put after the pieces in its prompt; nothing for a
/// plan that revises none.
pub fn revision_text(item: &Value, draft_id: &str) -> String {
    let id = item["id"].as_str().unwrap_or("");
    let title = item["title"].as_str().unwrap_or("");
    let left = item["remaining"]
        .as_str()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("(nothing listed)");
    let specs: Vec<&str> = (item["specRefs"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .collect();
    let specs = if specs.is_empty() {
        "(none)".to_string()
    } else {
        specs.join(", ")
    };
    [
        "The item it revises:".to_string(),
        String::new(),
        format!("This request was made about backlog item `{id}`, \"{title}\", so it is a **revision** of that item, not new work: change that item, and don't add a backlog entry for it."),
        String::new(),
        format!("- State: {}.", state_words(item)),
        format!("- What's left of it: {left}"),
        format!("- Its specs: {specs}"),
        String::new(),
        "Revise it:".to_string(),
        String::new(),
        format!("1. Edit the spec that holds `{id}` (the first of its specs above), in place and in its style: the sections the change touches; the decisions (what the person asked for marked as decided, with today's date, in their words; a decided line it replaces moved, with its date, under the spec's history or \"As built\" notes as \"Replaced on <date> by …\"); and its status line, which gains \"Revised <date> ({draft_id}): <one line>\". The item keeps its id, title and dependencies: no spec lists items. A request made on the card is the person deciding again: ask first only when you can't tell whether they meant to change a decided line."),
        format!("2. Return `revision`: `{{\"id\": \"{id}\", \"remaining\": \"…\"}}`, what's left to build once the change is in, written for the implementer as an item's remaining is. It replaces what was left: leave out steps or work the change makes unnecessary, and keep what is still needed."),
        "3. A part of the request that is separate new work is an entry as usual (`itemIds`), beside the revision in the same plan. When the whole request turns out to be separate work, return no `revision` and add an entry as usual. A fix asked about this item is a fix as usual.".to_string(),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(id: &str, remaining: &str) -> RawRevision {
        RawRevision {
            id: Some(id.into()),
            remaining: Some(remaining.into()),
        }
    }

    #[test]
    fn a_revision_is_kept_only_for_the_item_the_plan_revises() {
        let kept = to_revision(Some(&raw(" i1 ", "  Do the rest  ")), "i1");
        assert_eq!(
            kept,
            Revised::Keeps {
                remaining: "Do the rest".into()
            }
        );
        assert_eq!(to_revision(Some(&raw("other", "x")), "i1"), Revised::NewWork);
        assert_eq!(to_revision(Some(&raw("i1", "  ")), "i1"), Revised::NewWork);
        assert_eq!(to_revision(None, "i1"), Revised::NewWork);
    }

    #[test]
    fn a_kept_remaining_is_cut_to_the_stores_limit() {
        let Revised::Keeps { remaining } = to_revision(Some(&raw("i1", &"x".repeat(5000))), "i1") else {
            panic!("kept");
        };
        assert_eq!(remaining.chars().count(), REMAINING_MAX);
    }

    #[test]
    fn revises_of_reads_a_named_item_only() {
        let map = |v: Value| v.as_object().unwrap().clone();
        assert_eq!(revises_of(&map(json!({ "revises": " i1 " }))), Some("i1".into()));
        for none in [json!({}), json!({ "revises": null }), json!({ "revises": "" })] {
            assert_eq!(revises_of(&map(none)), None);
        }
    }

    #[test]
    fn the_drafters_block_names_the_item_its_state_its_remaining_and_its_specs() {
        let item = json!({ "id": "rollout", "title": "Automatic rollout", "status": "in_review", "detail": "checking",
            "slice": 1, "lease": { "token": "t" }, "remaining": "Set up CI", "specRefs": ["a.md", "b.md"] });
        let text = revision_text(&item, "d7");
        assert!(text.starts_with(
            "The item it revises:\n\nThis request was made about backlog item `rollout`, \"Automatic rollout\""
        ));
        assert!(text.contains("- State: in review (checking), 1 slice built, a session is on it."));
        assert!(text.contains("- What's left of it: Set up CI\n- Its specs: a.md, b.md"));
        assert!(text.contains("gains \"Revised <date> (d7): <one line>\""));
        assert!(text.contains("`{\"id\": \"rollout\", \"remaining\": \"…\"}`"));
    }

    #[test]
    fn a_waiting_item_with_nothing_listed_reads_plainly() {
        let item = json!({ "id": "i", "title": "T", "status": "todo", "slice": 2, "lease": null,
            "waiting": { "for": "answer" }, "waitingFor": "a question", "specRefs": [] });
        let text = revision_text(&item, "d");
        assert!(text.contains("- State: todo, 2 slices built, waiting for answer: a question."));
        assert!(text.contains("- What's left of it: (nothing listed)\n- Its specs: (none)"));
    }
}
