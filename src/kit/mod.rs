//! A project's kit (docs/specs/project-kits.md, TS `shared/projectKit.ts`): a small, validated, size-capped description of the UI
//! pieces a project has, so the "Approve this change?" card and the design step work for any project. Pure: [`discovery`] finds
//! the facts, [`store`] keeps them as the `project_kit` fact, and this file is the shape, the caps, the generic wireframe kit and the
//! small subset a stored review carries. Nothing here reads a file or runs anything.
use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::design_board::layout::Catalog;

pub mod discovery;
pub mod store;
#[cfg(test)]
mod store_tests;

/// The most bytes of JSON a kit may take.
pub const KIT_MAX_BYTES: usize = 24 * 1024;
pub const KIT_MAX_PIECES: usize = 60;
pub const KIT_MAX_PAGES: usize = 30;
pub const KIT_MAX_COLORS: usize = 12;

/// What a piece is, from a closed set: how the app draws it, whatever the project calls it.
pub const KIT_KINDS: [&str; 23] = [
    "text",
    "heading",
    "button",
    "input",
    "list",
    "table",
    "image",
    "canvas",
    "header",
    "tabBar",
    "sheet",
    "dialog",
    "card",
    "toggle",
    "badge",
    "chip",
    "nav",
    "player",
    "enemy",
    "healthBar",
    "score",
    "menu",
    "dialogBox",
];

/// The one region a kit page has.
pub const KIT_REGION: &str = "content";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Library,
    Catalogue,
    Wireframe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Genre {
    App,
    Game,
    Data,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Piece {
    pub id: String,
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub id: String,
    pub name: String,
    /// `screen`, `route` or `scene`.
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tokens {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fonts: Option<Vec<String>>,
}

impl Tokens {
    pub fn is_empty(&self) -> bool {
        self.colors.as_ref().is_none_or(BTreeMap::is_empty)
            && self.spacing.is_none()
            && self.radius.is_none()
            && self.fonts.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kit {
    pub tier: Tier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<Genre>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub pieces: Vec<Piece>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Tokens>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<Vec<Page>>,
}

/// What the design step uses when the project has no kit of its own: Retaskable's library, which the app knows.
pub fn library() -> Kit {
    Kit {
        tier: Tier::Library,
        genre: None,
        fingerprint: None,
        pieces: Vec::new(),
        tokens: None,
        pages: None,
    }
}

/// [`library`], shared, for a borrow that must outlive its caller.
pub fn library_ref() -> &'static Kit {
    static LIB: LazyLock<Kit> = LazyLock::new(library);
    &LIB
}

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap()
}
static PIECE_ID: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z][A-Za-z0-9-]{0,40}$"));
static PAGE_ID: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-z][a-z0-9]{0,30}$"));
static COLOR_KEY: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-z][A-Za-z0-9-]{0,23}$"));
static HEX: LazyLock<Regex> = LazyLock::new(|| re(r"^#[0-9a-fA-F]{6}$"));
static FINGERPRINT: LazyLock<Regex> = LazyLock::new(|| re(r"^[0-9a-f]{8,64}$"));

/// One line of words: trimmed, 1 to `max` characters (UTF-16 units), no line break.
fn line_ok(s: &str, max: usize) -> bool {
    let t = s.trim();
    !t.is_empty() && t == s && s.encode_utf16().count() <= max && !s.contains(['\r', '\n'])
}

/// Whether `kit` is a kit: every field within its shape and caps (`projectKit`, not the byte cap).
fn valid(kit: &Kit) -> bool {
    if kit.tier != Tier::Library && kit.pieces.is_empty() {
        return false;
    }
    if kit.pieces.len() > KIT_MAX_PIECES || kit.fingerprint.as_deref().is_some_and(|f| !FINGERPRINT.is_match(f)) {
        return false;
    }
    let ids: HashSet<&str> = kit.pieces.iter().map(|p| p.id.as_str()).collect();
    if ids.len() != kit.pieces.len() {
        return false;
    }
    let pieces_ok = kit.pieces.iter().all(|p| {
        PIECE_ID.is_match(&p.id)
            && line_ok(&p.name, 40)
            && KIT_KINDS.contains(&p.kind.as_str())
            && p.options
                .as_ref()
                .is_none_or(|o| o.len() <= 8 && o.iter().all(|s| line_ok(s, 24)))
    });
    if !pieces_ok {
        return false;
    }
    if let Some(pages) = &kit.pages {
        let ids: HashSet<&str> = pages.iter().map(|p| p.id.as_str()).collect();
        if pages.len() > KIT_MAX_PAGES
            || ids.len() != pages.len()
            || !pages.iter().all(|p| {
                PAGE_ID.is_match(&p.id)
                    && line_ok(&p.name, 40)
                    && matches!(p.kind.as_str(), "screen" | "route" | "scene")
            })
        {
            return false;
        }
    }
    let Some(t) = &kit.tokens else { return true };
    let colors_ok = t
        .colors
        .as_ref()
        .is_none_or(|c| c.len() <= KIT_MAX_COLORS && c.iter().all(|(k, v)| COLOR_KEY.is_match(k) && HEX.is_match(v)));
    colors_ok
        && t.spacing
            .as_ref()
            .is_none_or(|s| s.len() <= 8 && s.iter().all(|n| (0.0..=128.0).contains(n)))
        && t.radius
            .as_ref()
            .is_none_or(|s| s.len() <= 6 && s.iter().all(|n| (0.0..=64.0).contains(n)))
        && t.fonts
            .as_ref()
            .is_none_or(|f| f.len() <= 3 && f.iter().all(|s| line_ok(s, 40)))
}

fn bytes(v: &impl Serialize) -> usize {
    serde_json::to_string(v).map_or(usize::MAX, |s| s.len())
}

/// A kit read back from the store or a session's answer: `None` when it is not valid or is over the cap. A string is parsed first.
pub fn read_kit(raw: &Value) -> Option<Kit> {
    let value = match raw {
        Value::String(s) => {
            if s.len() > KIT_MAX_BYTES * 2 {
                return None;
            }
            serde_json::from_str::<Value>(s).ok()?
        }
        other => other.clone(),
    };
    let kit: Kit = serde_json::from_value(value).ok()?;
    (valid(&kit) && bytes(&kit) <= KIT_MAX_BYTES).then_some(kit)
}

fn fits(k: &Kit) -> bool {
    valid(k) && bytes(k) <= KIT_MAX_BYTES
}

/// A kit trimmed to fit the cap: pages, then options, then trailing pieces are dropped; `None` when it cannot.
pub fn cap_kit(kit: Kit) -> Option<Kit> {
    let mut k = kit;
    k.pieces.truncate(KIT_MAX_PIECES);
    if let Some(p) = k.pages.as_mut() {
        p.truncate(KIT_MAX_PAGES);
    }
    if fits(&k) {
        return Some(k);
    }
    if let Some(p) = k.pages.as_mut() {
        p.truncate(10);
        if fits(&k) {
            return Some(k);
        }
        k.pages = None;
        if fits(&k) {
            return Some(k);
        }
    }
    for p in &mut k.pieces {
        p.options = None;
    }
    while !fits(&k) && k.pieces.len() > 1 {
        k.pieces.pop();
    }
    fits(&k).then_some(k)
}

fn piece(id: &str, name: &str, kind: &str) -> Piece {
    Piece {
        id: id.into(),
        name: name.into(),
        kind: kind.into(),
        options: None,
    }
}

/// The generic built-in kit (deterministic): plain pieces, and for a game the game's own. `extra` is what the code found.
pub fn wireframe_kit(
    genre: Genre,
    tokens: Option<Tokens>,
    pages: Option<Vec<Page>>,
    fingerprint: Option<String>,
) -> Kit {
    let mut pieces = vec![
        piece("text", "Text", "text"),
        piece("heading", "Heading", "heading"),
        piece("button", "Button", "button"),
        piece("input", "Input", "input"),
        piece("list", "List", "list"),
        piece("table", "Table", "table"),
        piece("image", "Image area", "image"),
        piece("canvas", "Canvas area", "canvas"),
        piece("header", "Header", "header"),
        piece("tabBar", "Tab bar", "tabBar"),
        piece("sheet", "Sheet", "sheet"),
        piece("dialog", "Dialog", "dialog"),
        piece("card", "Card", "card"),
    ];
    if genre == Genre::Game {
        pieces.extend([
            piece("player", "Player", "player"),
            piece("enemy", "Enemy", "enemy"),
            piece("healthBar", "Health bar", "healthBar"),
            piece("score", "Score", "score"),
            piece("menu", "Menu", "menu"),
            piece("dialogBox", "Dialog box", "dialogBox"),
        ]);
    }
    Kit {
        tier: Tier::Wireframe,
        genre: Some(genre),
        fingerprint,
        pieces,
        tokens,
        pages,
    }
}

/// The closed sets a layout may use for this kit (`check_layout`): the piece ids, and each kit page with one region, `content`.
pub fn kit_catalog(kit: &Kit) -> Catalog {
    Catalog {
        elements: kit.pieces.iter().map(|p| p.id.clone()).collect(),
        pages: kit
            .pages
            .iter()
            .flatten()
            .map(|p| (p.id.clone(), vec![KIT_REGION.to_string()]))
            .collect(),
    }
}

/// The kit as the design prompt lists it, a line a piece.
pub fn pieces_text(kit: &Kit) -> String {
    kit.pieces
        .iter()
        .map(|p| {
            let options = p
                .options
                .as_ref()
                .filter(|o| !o.is_empty())
                .map_or(String::new(), |o| format!("; options: {}", o.join(", ")));
            format!("- `{}`: {} ({}{options})", p.id, p.name, p.kind)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The kit's pages as the design prompt lists them.
pub fn pages_text(kit: &Kit) -> String {
    match kit.pages.as_deref().filter(|p| !p.is_empty()) {
        Some(pages) => pages
            .iter()
            .map(|p| {
                format!(
                    "- `{id}`: the {name} {kind}. Parts:\n  - `{KIT_REGION}` (card, on desktop and phone): the main part of the {name} {kind}",
                    id = p.id,
                    name = p.name,
                    kind = p.kind
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => "(none: this project's screens are not known, so leave `page` and `region` out and draw a new screen)".into(),
    }
}

/// The accent colour token of a kit: a colour named primary, brand or accent (not its muted, surface, edge or text variants), else
/// none: never an arbitrary colour.
pub fn accent_of(kit: &Kit) -> Option<String> {
    static EXACT: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(primary|brand|accent)$"));
    static PRIMARY: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(primary|brand)"));
    static ACCENT: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)accent"));
    static VARIANT: LazyLock<Regex> = LazyLock::new(|| {
        re(r"(?i)(muted|surface|edge|on|text|strong|soft|light|dark|bg|background|border|hover|disabled)")
    });
    let colors = kit.tokens.as_ref()?.colors.as_ref()?;
    let plain = |k: &str| !VARIANT.is_match(k);
    let key = colors
        .keys()
        .find(|k| EXACT.is_match(k))
        .or_else(|| colors.keys().find(|k| PRIMARY.is_match(k) && plain(k)))
        .or_else(|| colors.keys().find(|k| ACCENT.is_match(k) && plain(k)))?;
    colors.get(key).cloned()
}

/// The small subset of `kit` a review needs: the pieces `used` (by id) and the page's name. `None` for the library tier (the app
/// knows it). A JSON value ready to go in the stored review's `kit`.
pub fn review_kit_of(kit: &Kit, used: &[String], page_id: Option<&str>) -> Option<Value> {
    if kit.tier == Tier::Library {
        return None;
    }
    let pieces: Vec<Value> = kit
        .pieces
        .iter()
        .filter(|p| used.contains(&p.id))
        .take(12)
        .map(|p| json!({ "id": p.id, "name": p.name, "kind": p.kind }))
        .collect();
    let mut out =
        json!({ "tier": if kit.tier == Tier::Catalogue { "catalogue" } else { "wireframe" }, "pieces": pieces });
    if let Some(a) = accent_of(kit) {
        out["accent"] = json!(a);
    }
    if let Some(name) = page_id.and_then(|id| kit.pages.as_ref()?.iter().find(|p| p.id == id)) {
        out["pageName"] = json!(name.name);
    }
    Some(out)
}

/// The kit's pieces and pages as a prompt lists them (`kitNote`, `library`, `pages`): Retaskable's own library for its own project
/// (`own`: its text), the project's kit otherwise.
pub fn prompt_text(
    kit: &Kit,
    own_library: impl FnOnce() -> String,
    own_pages: impl FnOnce() -> String,
) -> (String, String, String) {
    if kit.tier == Tier::Library {
        return (String::new(), own_library(), own_pages());
    }
    let sketch = if kit.tier == Tier::Wireframe {
        ", a plain wireframe sketch"
    } else {
        ""
    };
    (
        format!(
            "This project is not Retaskable's own: draw with ITS kit below (tier {}{sketch}), never with Retaskable's pieces. A piece's kind says how it is drawn.\n\n",
            if kit.tier == Tier::Catalogue {
                "catalogue"
            } else {
                "wireframe"
            }
        ),
        pieces_text(kit),
        pages_text(kit),
    )
}

/// What the drafter's prompt ends with: the kit's pieces and pages, which its `design` is made of (`prompts/drafter.md`, step 4).
pub fn design_kit_section(kit: &Kit) -> String {
    let (note, library, pages) = prompt_text(
        kit,
        crate::design_board::catalog::library_text,
        crate::design_board::catalog::pages_text,
    );
    format!(
        "\n\n## The design kit\n\n{note}One line each piece: `id`: name (for the Retaskable library: set, width × height; for a project's own kit: its kind).\n\n{library}\n\n### The pages\n\n{pages}\n"
    )
}

/// The structure the kit-tidying session answers in (`KIT_SCHEMA`): [`read_kit`] validates it in plain code.
pub fn summary_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "pieces": { "type": "array", "items": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" }, "name": { "type": "string" }, "kind": { "type": "string" },
                    "options": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["id", "name", "kind"]
            } }
        },
        "required": ["pieces"]
    })
}

/// The prompt that tidies the found names into the closed kit (`prompts/kit-summary.md`).
pub fn summary_prompt(found: &str) -> anyhow::Result<String> {
    crate::prompt::render_prompt(
        include_str!("../../prompts/kit-summary.md"),
        &[("kinds", KIT_KINDS.join(", ")), ("found", found.to_string())],
    )
}

/// The tidying session's `SessionSpec`: the design board's kind (read-only, fresh, no shell, a scratch directory).
pub fn summary_spec(
    prompt: String,
    model: &str,
    budget_usd: f64,
    cwd: std::path::PathBuf,
    local: Option<crate::engines::engine::LocalTarget>,
) -> crate::engines::engine::SessionSpec {
    crate::engines::engine::SessionSpec {
        schema: summary_schema(),
        ..crate::design_board::session::layout_spec(prompt, model, budget_usd, cwd, local)
    }
}

#[cfg(test)]
mod tests;
