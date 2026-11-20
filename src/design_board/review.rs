//! The designs a "Does this look right?" prompt shows, as the store keeps them with the plan's details (TS
//! `shared/designReview.ts`, add-and-design-review.md sections 5 to 7). The session draws one to three named variations in the
//! design board's layout format (checked by [`super::layout`]); what is kept is the SMALL layout (library piece ids and a few
//! words, never an image) and one plain sentence, so a phone can review them with no computer on. This is the shape it is kept
//! in (the zod `storedReview`, by hand: strict objects, trimmed one-line words with their caps) and its size cap. Pure plain code.
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};

use super::layout::{MAX_PIECES, MAX_VARIATIONS, ResolvedLayout, ResolvedPiece};
use super::sentence::SAFE_SENTENCE;

/// The most bytes of JSON a plan's stored review may take: a few KB of ids, not a picture (8 KB since the card carries the kit
/// pieces it uses, project-kits.md).
pub const REVIEW_MAX_BYTES: usize = 8 * 1024;

static PAGE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9]{0,30}$").unwrap());
static REGION_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9]{0,40}$").unwrap());
static ELEMENT_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9-]{0,60}$").unwrap());

/// JavaScript's `length`: UTF-16 units.
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `line(max)`: a string whose trimmed form is 1 to `max` long and on one line (`.trim()` then the checks; the trimmed value is
/// what is kept).
fn line(v: &Value, max: usize) -> Option<String> {
    let s = v.as_str()?.trim();
    (!s.is_empty() && js_len(s) <= max && !s.contains(['\r', '\n'])).then(|| s.to_string())
}

/// Checks a review against the stored shape and returns it as parsed (trimmed words, no unknown keys); `None` when it isn't one.
pub fn parse_review(raw: &Value) -> Option<Value> {
    let o = raw.as_object()?;
    if o.keys()
        .any(|k| !matches!(k.as_str(), "sentence" | "page" | "variations" | "chosen" | "kit"))
    {
        return None;
    }
    let sentence = o.get("sentence")?.as_str()?.trim();
    if sentence.is_empty() || js_len(sentence) > 240 {
        return None;
    }
    let mut out = Map::new();
    out.insert("sentence".into(), json!(sentence));
    if let Some(page) = o.get("page") {
        let p = page.as_object()?;
        if p.keys().any(|k| k != "id" && k != "region") {
            return None;
        }
        let (id, region) = (p.get("id")?.as_str()?, p.get("region")?.as_str()?);
        if !PAGE_ID.is_match(id) || !REGION_ID.is_match(region) {
            return None;
        }
        out.insert("page".into(), json!({ "id": id, "region": region }));
    }
    let variations = o.get("variations")?.as_array()?;
    if variations.is_empty() || variations.len() > MAX_VARIATIONS {
        return None;
    }
    let mut kept = Vec::new();
    for v in variations {
        let v = v.as_object()?;
        if v.keys().any(|k| k != "name" && k != "pieces") {
            return None;
        }
        let name = line(v.get("name")?, 40)?;
        let pieces = v.get("pieces")?.as_array()?;
        if pieces.is_empty() || pieces.len() > MAX_PIECES {
            return None;
        }
        let pieces: Option<Vec<Value>> = pieces.iter().map(parse_piece).collect();
        kept.push(json!({ "name": name, "pieces": pieces? }));
    }
    out.insert("variations".into(), Value::Array(kept));
    if let Some(chosen) = o.get("chosen") {
        let n = chosen.as_u64().filter(|n| (*n as usize) < MAX_VARIATIONS)?;
        out.insert("chosen".into(), json!(n));
    }
    if let Some(kit) = o.get("kit") {
        out.insert("kit".into(), parse_kit(kit)?);
    }
    Some(Value::Object(out))
}

/// The project's own kit pieces a review uses (`reviewKit`): tier, an accent colour, at most 12 pieces and the page's name.
fn parse_kit(raw: &Value) -> Option<Value> {
    let k = raw.as_object()?;
    if k.keys()
        .any(|k| !matches!(k.as_str(), "tier" | "accent" | "pieces" | "pageName"))
    {
        return None;
    }
    let tier = k
        .get("tier")?
        .as_str()
        .filter(|t| matches!(*t, "catalogue" | "wireframe"))?;
    let pieces = k.get("pieces")?.as_array()?;
    if pieces.len() > 12 {
        return None;
    }
    let pieces: Option<Vec<Value>> = pieces
        .iter()
        .map(|p| {
            let p = p.as_object()?;
            if p.keys().any(|k| !matches!(k.as_str(), "id" | "name" | "kind")) {
                return None;
            }
            let id = p.get("id")?.as_str().filter(|e| ELEMENT_ID.is_match(e))?;
            let kind = p.get("kind")?.as_str().filter(|k| crate::kit::KIT_KINDS.contains(k))?;
            Some(json!({ "id": id, "name": line(p.get("name")?, 40)?, "kind": kind }))
        })
        .collect();
    let mut out = json!({ "tier": tier, "pieces": pieces? });
    if let Some(a) = k.get("accent") {
        let a = a
            .as_str()
            .filter(|a| a.len() == 7 && a.starts_with('#') && a[1..].chars().all(|c| c.is_ascii_hexdigit()))?;
        out["accent"] = json!(a);
    }
    if let Some(n) = k.get("pageName") {
        out["pageName"] = json!(line(n, 40)?);
    }
    Some(out)
}

/// Which variation an approval's words name (`chosenIn`: "The design is approved. Plan it as shown: "<name>" ..."), or `None`: the plan
/// records it as the one chosen, so the ticket shows what was picked.
pub fn chosen_in(review: &Value, notes: &str) -> Option<usize> {
    if !notes.trim().starts_with("The design is approved.") {
        return None;
    }
    review
        .get("variations")?
        .as_array()?
        .iter()
        .position(|v| v["name"].as_str().is_some_and(|n| notes.contains(&format!("\"{n}\""))))
}

fn parse_piece(p: &Value) -> Option<Value> {
    let p = p.as_object()?;
    match p.get("kind")?.as_str()? {
        "library" => {
            if p.keys().any(|k| !matches!(k.as_str(), "kind" | "element" | "label")) {
                return None;
            }
            let element = p.get("element")?.as_str().filter(|e| ELEMENT_ID.is_match(e))?;
            let mut out = json!({ "kind": "library", "element": element });
            if let Some(label) = p.get("label") {
                out["label"] = json!(line(label, 60)?);
            }
            Some(out)
        }
        "new" => {
            if p.keys().any(|k| !matches!(k.as_str(), "kind" | "name" | "purpose")) {
                return None;
            }
            Some(json!({ "kind": "new", "name": line(p.get("name")?, 40)?, "purpose": line(p.get("purpose")?, 120)? }))
        }
        _ => None,
    }
}

fn piece_value(p: &ResolvedPiece) -> Value {
    match p {
        ResolvedPiece::Library { element, label } => {
            let mut v = json!({ "kind": "library", "element": element });
            if let Some(label) = label {
                v["label"] = json!(label);
            }
            v
        }
        ResolvedPiece::New { name, purpose } => json!({ "kind": "new", "name": name, "purpose": purpose }),
    }
}

/// `text` cut to `n` UTF-16 units (JavaScript's `slice(0, n)`), never in the middle of a character.
fn cut_js(text: &str, n: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        used += c.len_utf16();
        if used > n {
            break;
        }
        out.push(c);
    }
    out
}

/// The review for a checked layout and its sentence, within [`REVIEW_MAX_BYTES`]: later variations then pieces are dropped until it
/// fits (the first variation always stays); `None` when even that does not fit. Only the validated layout and the sentence:
/// nothing else the person wrote.
pub fn review_of(layout: &ResolvedLayout, sentence: &str) -> Option<Value> {
    review_of_with(layout, sentence, None)
}

/// [`review_of`] for a project that has a kit of its own: `kit` is the subset the layout uses (`crate::kit::review_kit_of`), kept with
/// the review so a phone can draw the card with no computer on.
pub fn review_of_with(layout: &ResolvedLayout, sentence: &str, kit: Option<&Value>) -> Option<Value> {
    let sentence = match sentence.trim() {
        "" => SAFE_SENTENCE,
        s => s,
    };
    let mut variations: Vec<Value> = layout
        .variations
        .iter()
        .take(MAX_VARIATIONS)
        .map(|v| {
            json!({
                "name": cut_js(&v.name, 40),
                "pieces": v.pieces.iter().take(MAX_PIECES).map(piece_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    loop {
        let mut candidate = json!({ "sentence": sentence, "variations": variations });
        if let Some(kit) = kit {
            candidate["kit"] = kit.clone();
        }
        if let Some(page) = &layout.page {
            candidate["page"] = json!({ "id": page.id, "region": page.region });
        }
        if let Some(parsed) = parse_review(&candidate)
            && parsed.to_string().len() <= REVIEW_MAX_BYTES
        {
            return Some(parsed);
        }
        if variations.len() > 1 {
            variations.pop();
        } else {
            let pieces = variations.first_mut()?["pieces"].as_array_mut()?;
            if pieces.len() <= 1 {
                return None;
            }
            pieces.pop();
        }
    }
}

/// A stored review read back (a bad or oversized one is `None`, so the card shows no window rather than a wrong one). A string
/// is the JSON the details keep it as.
pub fn read_review(raw: &Value) -> Option<Value> {
    let parsed = match raw {
        Value::String(s) if s.len() > REVIEW_MAX_BYTES => return None,
        Value::String(s) => serde_json::from_str::<Value>(s).ok()?,
        other => other.clone(),
    };
    parse_review(&parsed).filter(|r| r.to_string().len() <= REVIEW_MAX_BYTES)
}

#[cfg(test)]
mod tests;
