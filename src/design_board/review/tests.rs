use serde_json::json;

use super::*;
use crate::design_board::layout::{PageRef, ResolvedVariation};

fn library(element: &str, label: Option<&str>) -> ResolvedPiece {
    ResolvedPiece::Library {
        element: element.into(),
        label: label.map(Into::into),
    }
}

fn layout() -> ResolvedLayout {
    ResolvedLayout {
        page: Some(PageRef {
            id: "board".into(),
            region: "header".into(),
        }),
        variations: vec![
            ResolvedVariation {
                name: "Just a box".into(),
                pieces: vec![library("button", Some("Search"))],
            },
            ResolvedVariation {
                name: "Box with a hint".into(),
                pieces: vec![
                    library("card", None),
                    ResolvedPiece::New {
                        name: "Hint".into(),
                        purpose: "Says what to type.".into(),
                    },
                ],
            },
        ],
        replaces: vec!["a".into(), "b".into()],
    }
}

fn big_variation(n: usize) -> ResolvedVariation {
    ResolvedVariation {
        name: format!("Variation {n}"),
        pieces: (0..12)
            .map(|i| ResolvedPiece::New {
                name: format!("Piece {i}"),
                purpose: "p".repeat(120),
            })
            .collect(),
    }
}

#[test]
fn a_layout_is_kept_as_a_small_review_the_sentence_the_page_the_variations_nothing_to_replace() {
    let r = review_of(&layout(), "You'll see a search box.").unwrap();
    assert_eq!(r["sentence"], "You'll see a search box.");
    assert_eq!(r["page"], json!({ "id": "board", "region": "header" }));
    assert_eq!(r["variations"].as_array().unwrap().len(), 2);
    assert!(r.get("replaces").is_none());
    assert_eq!(
        r["variations"][0],
        json!({ "name": "Just a box", "pieces": [{ "kind": "library", "element": "button", "label": "Search" }] })
    );
    assert_eq!(read_review(&json!(r.to_string())), Some(r));
}

#[test]
fn a_review_is_never_over_its_cap_later_variations_then_pieces_are_dropped_and_a_hopeless_one_is_none() {
    let huge = ResolvedLayout {
        page: None,
        variations: vec![big_variation(1), big_variation(2), big_variation(3)],
        replaces: vec![],
    };
    let r = review_of(&huge, "You'll see more.").unwrap();
    assert!(r.to_string().len() <= REVIEW_MAX_BYTES);
    assert!(!r["variations"].as_array().unwrap().is_empty());
    let one = ResolvedLayout {
        variations: vec![big_variation(1)],
        ..huge.clone()
    };
    let kept = review_of(&one, "x").unwrap();
    assert!(kept.to_string().len() <= REVIEW_MAX_BYTES);
    // A single piece that alone is over the cap leaves nothing to keep.
    let hopeless = ResolvedLayout {
        page: None,
        variations: vec![ResolvedVariation {
            name: "n".into(),
            pieces: vec![ResolvedPiece::New {
                name: "x".into(),
                purpose: "y".repeat(8000),
            }],
        }],
        replaces: vec![],
    };
    assert_eq!(review_of(&hopeless, "You'll see it."), None);
}

#[test]
fn an_empty_sentence_is_the_safe_one_and_a_new_screen_has_no_page() {
    let no_page = ResolvedLayout { page: None, ..layout() };
    let r = review_of(&no_page, "  ").unwrap();
    assert_eq!(r["sentence"], SAFE_SENTENCE);
    assert!(r.get("page").is_none());
}

#[test]
fn a_stored_review_read_back_must_pass_its_schema_and_cap_or_there_is_none() {
    let r = review_of(&layout(), "You'll see a box.").unwrap();
    let with = |k: &str, v: Value| {
        let mut r = r.clone();
        r[k] = v;
        r
    };
    assert_eq!(read_review(&with("extra", json!(1))), None, "an extra field");
    assert_eq!(read_review(&with("variations", json!([]))), None);
    assert_eq!(
        read_review(&with(
            "variations",
            json!([{ "name": "a\nb", "pieces": r["variations"][0]["pieces"] }])
        )),
        None
    );
    assert_eq!(read_review(&json!("{not json")), None);
    assert_eq!(read_review(&json!("x".repeat(REVIEW_MAX_BYTES + 1))), None);
    assert_eq!(read_review(&with("chosen", json!(3))), None);
    assert_eq!(read_review(&with("chosen", json!(1))).unwrap()["chosen"], 1);
    assert_eq!(
        read_review(&with("sentence", json!("x".repeat(241)))),
        None,
        "a sentence over 240"
    );
    assert_eq!(
        read_review(&with("page", json!({ "id": "Board", "region": "header" }))),
        None
    );
}

#[test]
fn a_name_is_cut_to_forty_then_trimmed_as_the_stored_shape_does() {
    let mut l = layout();
    l.variations[0].name = format!("  {}  ", "n".repeat(50));
    let r = review_of(&l, "You'll see it.").unwrap();
    assert_eq!(r["variations"][0]["name"], "n".repeat(38));
}

#[test]
fn an_approval_names_the_variation_it_picked_and_other_notes_name_none() {
    let review = json!({ "sentence": "s", "variations": [
        { "name": "Side by side", "pieces": [] }, { "name": "Stacked", "pieces": [] }] });
    let notes = "The design is approved. Plan it as shown: \"Stacked\" as a new screen, made of x.";
    assert_eq!(chosen_in(&review, notes), Some(1));
    assert_eq!(
        chosen_in(&review, "Make it \"Stacked\" please"),
        None,
        "not an approval"
    );
    assert_eq!(
        chosen_in(&review, "The design is approved. Plan it as shown: \"Other\""),
        None
    );
}

#[test]
fn a_review_keeps_the_kit_pieces_it_uses_and_refuses_a_bad_kit() {
    let kit = json!({ "tier": "catalogue", "accent": "#112233", "pieces": [{ "id": "card", "name": "Card", "kind": "card" }], "pageName": "Home" });
    let r = review_of_with(&layout(), "You'll see a box.", Some(&kit)).unwrap();
    assert_eq!(r["kit"], kit);
    assert_eq!(read_review(&r).unwrap()["kit"], kit);
    assert_eq!(review_of(&layout(), "You'll see a box.").unwrap().get("kit"), None);
    let mut bad = r.clone();
    bad["kit"]["pieces"][0]["kind"] = json!("hologram");
    assert_eq!(read_review(&bad), None);
    bad["kit"]["pieces"][0]["kind"] = json!("card");
    bad["kit"]["tier"] = json!("library");
    assert_eq!(read_review(&bad), None, "the library is never carried");
    assert_eq!(REVIEW_MAX_BYTES, 8 * 1024);
}
