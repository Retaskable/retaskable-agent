//! A design board's layout (TS `shared/designLayout.ts`, docs/specs/design-view.md section 4, 14): what the
//! drawing session answers with, checked in plain code against the closed sets (never an LLM's say-so). A small
//! closed description: library element ids, words, and a dashed placeholder where the library has nothing. The
//! shape check is the TS zod schema by hand: strict objects, trimmed one-line words with their caps.
use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};

/// The most pieces in one variation, variations in one layout, and things a layout may replace.
pub const MAX_PIECES: usize = 12;
pub const MAX_VARIATIONS: usize = 3;
pub const MAX_REPLACES: usize = 40;
/// The most pieces of the board the session reads.
pub const MAX_BOARD_PIECES: usize = 80;

/// A piece after checking: a library element the table knows, or a new component to be shown as a placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedPiece {
    Library { element: String, label: Option<String> },
    New { name: String, purpose: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedVariation {
    pub name: String,
    pub pieces: Vec<ResolvedPiece>,
}

/// The real page a change is made to and the part of it that changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRef {
    pub id: String,
    pub region: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLayout {
    /// None is a new screen.
    pub page: Option<PageRef>,
    pub variations: Vec<ResolvedVariation>,
    pub replaces: Vec<String>,
}

/// What the closed sets are made of: the drawing table's element ids, and each real page's region ids (a page not in
/// it is not one).
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub elements: HashSet<String>,
    pub pages: BTreeMap<String, Vec<String>>,
}

/// "needsYouCard" as a name a person reads: "Needs you card".
pub fn humanise(id: &str) -> String {
    let mut spaced = String::new();
    let mut prev: Option<char> = None;
    for c in id.chars() {
        if c.is_ascii_uppercase() && prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit()) {
            spaced.push(' ');
        }
        spaced.push(if c == '-' { ' ' } else { c });
        prev = Some(c);
    }
    let words = spaced.trim().to_lowercase();
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => id.to_string(),
    }
}

/// The structural regions of a page: not where a card's change goes by default.
const STRUCTURAL: [&str; 3] = ["header", "sidebar", "tabBar"];

type Issues = Vec<(String, String)>;

/// JavaScript's string length, which is what the TS caps count.
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn is_id(s: &str, first: fn(char) -> bool, rest: fn(char) -> bool, max_after_first: usize) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(first) && s.chars().count() <= max_after_first + 1 && chars.all(rest)
}

fn lower(c: char) -> bool {
    c.is_ascii_lowercase()
}
fn letter(c: char) -> bool {
    c.is_ascii_alphabetic()
}
fn lower_digit(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}
fn alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}
fn alnum_dash(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

struct Check {
    issues: Issues,
}

impl Check {
    fn bad(&mut self, path: &str, message: impl Into<String>) {
        self.issues.push((path.to_string(), message.into()));
    }

    /// `z.string().trim().min(1).max(max).regex(/^[^\r\n]*$/)`: one line of words.
    fn line(&mut self, v: &Value, path: &str, max: usize) -> Option<String> {
        let Some(s) = v.as_str() else {
            self.bad(path, "Invalid input: expected string");
            return None;
        };
        let s = s.trim();
        let mut ok = true;
        if s.is_empty() {
            self.bad(path, "Too small: expected string to have >=1 characters");
            ok = false;
        }
        if js_len(s) > max {
            self.bad(path, format!("Too big: expected string to have <={max} characters"));
            ok = false;
        }
        if s.contains(['\r', '\n']) {
            self.bad(path, "one line");
            ok = false;
        }
        ok.then(|| s.to_string())
    }

    fn id(&mut self, v: &Value, path: &str, what: &str, f: fn(&str) -> bool) -> Option<String> {
        match v.as_str() {
            Some(s) if f(s) => Some(s.to_string()),
            Some(_) => {
                self.bad(path, format!("Invalid string: {what}"));
                None
            }
            None => {
                self.bad(path, "Invalid input: expected string");
                None
            }
        }
    }

    /// An object whose keys are all in `allowed` (`z.strictObject`).
    fn strict<'a>(&mut self, v: &'a Value, path: &str, allowed: &[&str]) -> Option<&'a Map<String, Value>> {
        let Some(o) = v.as_object() else {
            self.bad(path, "Invalid input: expected object");
            return None;
        };
        for key in o.keys().filter(|k| !allowed.contains(&k.as_str())) {
            self.bad(path, format!("Unrecognized key: \"{key}\""));
        }
        Some(o)
    }
}

struct RawPiece {
    element: Option<String>,
    label: Option<String>,
    name: Option<String>,
    purpose: Option<String>,
}

fn piece(c: &mut Check, v: &Value, path: &str) -> Option<RawPiece> {
    let before = c.issues.len();
    let o = c.strict(v, path, &["element", "label", "name", "purpose"])?;
    let at = |k: &str| format!("{path}.{k}");
    let mut p = RawPiece {
        element: None,
        label: None,
        name: None,
        purpose: None,
    };
    // A JSON null is a wrong type, as zod says (only a missing key is `undefined`).
    if let Some(v) = o.get("element") {
        p.element = c.id(v, &at("element"), "an element id", |s| is_id(s, letter, alnum_dash, 60));
    }
    if let Some(v) = o.get("label") {
        p.label = c.line(v, &at("label"), 60);
    }
    if let Some(v) = o.get("name") {
        p.name = c.line(v, &at("name"), 40);
    }
    if let Some(v) = o.get("purpose") {
        p.purpose = c.line(v, &at("purpose"), 120);
    }
    if c.issues.len() > before {
        return None;
    }
    if p.element.is_some() == p.name.is_some() {
        c.bad(
            path,
            "a piece is a library element or a new component, not both and not neither",
        );
        return None;
    }
    if p.element.is_none() && p.purpose.is_none() {
        c.bad(path, "a new component needs a name and a one-line purpose");
        return None;
    }
    Some(p)
}

/// What a checked answer is: the layout and what was changed on the way, or why it isn't one.
pub type Checked = Result<(ResolvedLayout, Vec<String>), String>;

/// A raw answer checked against the closed sets (`catalog`). A shape that isn't the schema is refused. An element id
/// outside the set is never drawn as itself: it becomes a new-component placeholder (named by its label or its id),
/// and the notes say so. A page outside the set makes the change a new screen; a region outside the page's list
/// becomes the page's first card (or its first region). `own` holds the ids of things a person has not touched that
/// this turn may replace, so a `replaces` naming anything else is dropped (the placing checks again).
pub fn check_layout(raw: &Value, catalog: &Catalog, own: &HashSet<String>) -> Checked {
    let mut c = Check { issues: Vec::new() };
    let parsed = parse(&mut c, raw);
    let Some((page, region, variations, replaces)) = parsed.filter(|_| c.issues.is_empty()) else {
        let shown: Vec<String> = c
            .issues
            .iter()
            .take(3)
            .map(|(path, message)| format!("{}: {message}", if path.is_empty() { "layout" } else { path }))
            .collect();
        return Err(shown.join("; "));
    };
    let mut notes = Vec::new();
    let variations: Vec<ResolvedVariation> = variations
        .into_iter()
        .map(|(name, pieces)| ResolvedVariation {
            name,
            pieces: pieces
                .into_iter()
                .map(|p| match p.element {
                    None => ResolvedPiece::New {
                        name: p.name.unwrap_or_default(),
                        purpose: p.purpose.unwrap_or_default(),
                    },
                    Some(element) if catalog.elements.contains(&element) => ResolvedPiece::Library {
                        element,
                        label: p.label,
                    },
                    Some(element) => {
                        notes.push(format!(
                            "\"{element}\" isn't in the library, so it is shown as a new component."
                        ));
                        ResolvedPiece::New {
                            name: p.label.unwrap_or_else(|| humanise(&element)),
                            purpose: "Not in the library yet.".to_string(),
                        }
                    }
                })
                .collect(),
        })
        .collect();
    let mut page_ref = None;
    if let (Some(page), Some(region)) = (&page, &region) {
        match catalog.pages.get(page) {
            None => notes.push(format!(
                "\"{page}\" isn't one of the pages, so this is shown as a new screen."
            )),
            Some(regions) if regions.contains(region) => {
                page_ref = Some(PageRef {
                    id: page.clone(),
                    region: region.clone(),
                });
            }
            Some(regions) => {
                let fallback = regions
                    .iter()
                    .find(|r| !STRUCTURAL.contains(&r.as_str()))
                    .or_else(|| regions.first());
                notes.push(format!(
                    "\"{region}\" isn't a part of the {page} page, so the change is shown in \"{}\".",
                    fallback.map_or("undefined", String::as_str)
                ));
                if let Some(fallback) = fallback {
                    page_ref = Some(PageRef {
                        id: page.clone(),
                        region: fallback.clone(),
                    });
                }
            }
        }
    }
    let kept: Vec<String> = replaces.iter().filter(|id| own.contains(*id)).cloned().collect();
    if kept.len() != replaces.len() {
        notes.push("Some things to replace were not the chat's own, untouched ones; they stay.".to_string());
    }
    Ok((
        ResolvedLayout {
            page: page_ref,
            variations,
            replaces: kept,
        },
        notes,
    ))
}

type Parsed = (
    Option<String>,
    Option<String>,
    Vec<(String, Vec<RawPiece>)>,
    Vec<String>,
);

fn parse(c: &mut Check, raw: &Value) -> Option<Parsed> {
    let o = c.strict(raw, "", &["page", "region", "variations", "replaces"])?;
    let page = o
        .get("page")
        .and_then(|v| c.id(v, "page", "a page id", |s| is_id(s, lower, lower_digit, 30)));
    let region = o
        .get("region")
        .and_then(|v| c.id(v, "region", "a region id", |s| is_id(s, letter, alnum, 40)));
    let mut variations = Vec::new();
    match o.get("variations") {
        None => c.bad("variations", "Invalid input: expected array"),
        Some(Value::Array(list)) => {
            if list.is_empty() {
                c.bad("variations", "Too small: expected array to have >=1 items");
            }
            if list.len() > MAX_VARIATIONS {
                c.bad(
                    "variations",
                    format!("Too big: expected array to have <={MAX_VARIATIONS} items"),
                );
            }
            for (i, v) in list.iter().enumerate() {
                let path = format!("variations.{i}");
                let Some(vo) = c.strict(v, &path, &["name", "pieces"]) else {
                    continue;
                };
                let name = match vo.get("name") {
                    Some(n) => c.line(n, &format!("{path}.name"), 40),
                    None => {
                        c.bad(&format!("{path}.name"), "Invalid input: expected string");
                        None
                    }
                };
                let mut pieces = Vec::new();
                match vo.get("pieces") {
                    Some(Value::Array(list)) => {
                        if list.is_empty() {
                            c.bad(&format!("{path}.pieces"), "Too small: expected array to have >=1 items");
                        }
                        if list.len() > MAX_PIECES {
                            c.bad(
                                &format!("{path}.pieces"),
                                format!("Too big: expected array to have <={MAX_PIECES} items"),
                            );
                        }
                        for (j, p) in list.iter().enumerate() {
                            pieces.extend(piece(c, p, &format!("{path}.pieces.{j}")));
                        }
                    }
                    _ => c.bad(&format!("{path}.pieces"), "Invalid input: expected array"),
                }
                if let Some(name) = name {
                    variations.push((name, pieces));
                }
            }
        }
        Some(_) => c.bad("variations", "Invalid input: expected array"),
    }
    let mut replaces = Vec::new();
    match o.get("replaces") {
        None => {}
        Some(Value::Array(list)) => {
            if list.len() > MAX_REPLACES {
                c.bad(
                    "replaces",
                    format!("Too big: expected array to have <={MAX_REPLACES} items"),
                );
            }
            for (i, v) in list.iter().enumerate() {
                match v.as_str() {
                    Some(s) if !s.is_empty() && js_len(s) <= 80 => replaces.push(s.to_string()),
                    Some(_) => c.bad(&format!("replaces.{i}"), "Invalid string: expected 1 to 80 characters"),
                    None => c.bad(&format!("replaces.{i}"), "Invalid input: expected string"),
                }
            }
        }
        Some(_) => c.bad("replaces", "Invalid input: expected array"),
    }
    if c.issues.is_empty() && page.is_some() != region.is_some() {
        c.bad(
            "",
            "a change to a page names its page and its region; a new screen names neither",
        );
    }
    Some((page, region, variations, replaces))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn catalog() -> Catalog {
        Catalog {
            elements: ["card", "button", "needsYouCard"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            pages: BTreeMap::from([
                (
                    "board".to_string(),
                    ["header", "tabBar", "googlePlayRelease", "readmeBadge"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                ),
                (
                    "ideas".to_string(),
                    ["header", "newIdea"].iter().map(|s| s.to_string()).collect(),
                ),
            ]),
        }
    }

    fn base() -> Value {
        json!({"page": "board", "region": "googlePlayRelease",
            "variations": [{"name": "A", "pieces": [{"element": "card"}, {"element": "button", "label": "Snooze"}]}]})
    }

    fn check(v: &Value) -> Checked {
        check_layout(v, &catalog(), &HashSet::new())
    }

    fn with(mut v: Value, key: &str, to: Value) -> Value {
        v[key] = to;
        v
    }

    fn without(mut v: Value, key: &str) -> Value {
        v.as_object_mut().unwrap().remove(key);
        v
    }

    #[test]
    fn a_layout_of_library_elements_on_a_pages_region_is_kept_as_it_is() {
        let (layout, notes) = check(&base()).unwrap();
        assert_eq!(
            layout.page,
            Some(PageRef {
                id: "board".into(),
                region: "googlePlayRelease".into()
            })
        );
        assert_eq!(
            layout.variations[0].pieces,
            [
                ResolvedPiece::Library {
                    element: "card".into(),
                    label: None
                },
                ResolvedPiece::Library {
                    element: "button".into(),
                    label: Some("Snooze".into())
                }
            ]
        );
        assert!(notes.is_empty());
    }

    #[test]
    fn a_page_and_a_region_go_together_a_change_names_both_a_new_screen_neither() {
        assert!(check(&without(base(), "region")).is_err(), "a page with no region");
        assert!(check(&without(base(), "page")).is_err(), "a region with no page");
        let screen = check(&json!({"variations": base()["variations"]})).unwrap();
        assert!(screen.0.page.is_none() && screen.1.is_empty());
    }

    #[test]
    fn a_page_outside_the_list_is_a_new_screen_and_a_region_outside_falls_to_the_first_card() {
        let (nowhere, notes) = check(&with(base(), "page", json!("nowhere"))).unwrap();
        assert!(nowhere.page.is_none() && notes.len() == 1);
        let (odd, notes) = check(&with(base(), "region", json!("madeUp"))).unwrap();
        assert_eq!(odd.page.unwrap().region, "googlePlayRelease");
        assert_eq!(notes.len(), 1);
        let ideas = with(with(base(), "page", json!("ideas")), "region", json!("madeUp"));
        assert_eq!(check(&ideas).unwrap().0.page.unwrap().region, "newIdea");
    }

    #[test]
    fn an_element_outside_the_set_becomes_a_new_component_placeholder_and_says_so() {
        let v = with(
            base(),
            "variations",
            json!([{"name": "A", "pieces": [{"element": "hologramPicker", "label": "Duration picker"}, {"element": "snoozeDial"}]}]),
        );
        let (layout, notes) = check(&v).unwrap();
        assert_eq!(
            layout.variations[0].pieces,
            [
                ResolvedPiece::New {
                    name: "Duration picker".into(),
                    purpose: "Not in the library yet.".into()
                },
                ResolvedPiece::New {
                    name: "Snooze dial".into(),
                    purpose: "Not in the library yet.".into()
                }
            ]
        );
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn a_new_component_needs_a_name_and_a_one_line_purpose_and_is_not_also_a_library_element() {
        let one = |p: Value| with(base(), "variations", json!([{"name": "A", "pieces": [p]}]));
        assert!(
            check(&one(
                json!({"name": "Duration picker", "purpose": "Pick how long to snooze."})
            ))
            .is_ok()
        );
        assert!(check(&one(json!({"name": "Duration picker"}))).is_err(), "no purpose");
        assert!(
            check(&one(json!({"element": "card", "name": "X", "purpose": "y"}))).is_err(),
            "both"
        );
        assert!(check(&one(json!({}))).is_err(), "neither");
        assert!(
            check(&one(json!({"name": "X", "purpose": "two\nlines"}))).is_err(),
            "purpose is one line"
        );
    }

    #[test]
    fn the_shape_is_strict_three_variations_twelve_pieces_no_extra_fields() {
        let group = base()["variations"][0].clone();
        assert!(check(&with(base(), "variations", json!([]))).is_err());
        assert!(check(&with(base(), "variations", json!([group, group, group, group]))).is_err());
        let thirteen: Vec<Value> = (0..13).map(|_| json!({"element": "card"})).collect();
        assert!(check(&with(base(), "variations", json!([{"name": "A", "pieces": thirteen}]))).is_err());
        assert!(check(&with(base(), "extra", json!(1))).is_err());
        assert!(check(&with(base(), "title", json!("A title"))).is_err(), "no title");
        assert!(
            check(&with(base(), "current", json!({"element": "card"}))).is_err(),
            "now is the page as it is"
        );
        let mut direction = group.clone();
        direction["direction"] = json!("row");
        assert!(check(&with(base(), "variations", json!([direction]))).is_err());
        assert!(check(&json!("not a layout")).is_err());
        assert!(check(&json!({"variations": null})).is_err());
    }

    #[test]
    fn words_are_trimmed_and_capped_and_ids_have_the_shape_the_table_writes() {
        let (layout, _) = check(&with(
            base(),
            "variations",
            json!([{"name": "  A: button \n", "pieces": [{"element": "card", "label": " Hi "}]}]),
        ))
        .unwrap();
        assert_eq!(layout.variations[0].name, "A: button");
        let long = "x".repeat(61);
        assert!(
            check(&with(
                base(),
                "variations",
                json!([{"name": "A", "pieces": [{"element": "card", "label": long}]}])
            ))
            .is_err()
        );
        assert!(
            check(&with(base(), "page", json!("Board"))).is_err(),
            "a page id is lower case"
        );
        assert!(
            check(&with(base(), "region", json!("a-b"))).is_err(),
            "a region id is letters and digits"
        );
        let reason = check(&with(base(), "extra", json!(1))).unwrap_err();
        assert!(reason.starts_with("layout: Unrecognized key"), "{reason}");
    }

    #[test]
    fn replaces_keeps_only_the_ids_the_chat_placed_and_nobody_has_touched() {
        let own: HashSet<String> = ["mine".to_string()].into();
        let (layout, notes) =
            check_layout(&with(base(), "replaces", json!(["mine", "theirs"])), &catalog(), &own).unwrap();
        assert_eq!(layout.replaces, ["mine"]);
        assert_eq!(notes.len(), 1);
        assert!(check(&with(base(), "replaces", json!(vec!["a"; 41]))).is_err());
    }

    #[test]
    fn humanise_reads_an_id_as_a_name() {
        assert_eq!(humanise("needsYouCard"), "Needs you card");
        assert_eq!(humanise("snooze-dial"), "Snooze dial");
        assert_eq!(humanise("card2Row"), "Card2 row");
    }
}
