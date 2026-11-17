//! An idea's design board, on the board's side (TS `app/src/ui/whiteboardDesignPlace.ts`,
//! docs/specs/design-view/14): what the drawing session reads of the board, and a checked layout turned into board
//! things. Plain maths, so it is tested. The session never touches the board: it answers with a layout
//! ([`super::layout`]), and this places it as the chat's own work.
//!
//! Not ported: `planRequest`, `pieceName` and the `PlacedDesign` cards. They belong to the app (**Approve design**
//! sends the plan back with `planRequest` of the board's pieces, design-view/14.4) and the removed chat's thread;
//! nothing on the agent reads them.
use std::collections::HashSet;

use serde_json::{Value, json};

use super::catalog::{Device, blank_element_id, element_of, page_element_id, region_of};
use super::layout::{ResolvedLayout, ResolvedPiece, ResolvedVariation};
use super::things::{Bounds, STICKY, Thing, js_round, num};
use crate::text::truncate;

/// Where the chat's work sits: on a grid of this many points.
const SNAP: f64 = 8.0;
/// The room between a design's two pages, between designs, round a label, and kept clear of everything on the board.
const PAGE_GAP: f64 = 64.0;
const ROW_GAP: f64 = 96.0;
const MARGIN: f64 = 96.0;
const LABEL_W: f64 = STICKY + 40.0;
/// The room between the pieces stacked in a region, and round the pieces of a new screen.
const PIECE_GAP: f64 = 8.0;
const SCREEN_PAD: f64 = 16.0;
/// A new-component placeholder's height (`NEW_H`).
const NEW_H: f64 = 96.0;
/// The mark round the changed region sticks out by this much.
const MARK: f64 = 4.0;
/// The most characters of a thing's words the session reads.
const WORDS: usize = 240;

fn snap(v: f64) -> f64 {
    js_round(v / SNAP) * SNAP
}

/// A number as JavaScript prints it in a string.
fn js_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 9e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// The words and place of a thing as the chat placed it, kept on it as `chat`. A thing whose look no longer matches
/// was moved, resized or re-worded by a person: it's theirs now ([`who_of`]). Nothing watches the board for this: it
/// is worked out from the thing itself.
pub fn chat_mark(t: &Thing) -> String {
    let r = |n: f64| js_num(js_round(n));
    let g = |k: &str| t.f64(k).unwrap_or(0.0);
    match t.kind() {
        "design" => format!(
            "d|{}|{}|{}|{}|{}|{}|{}",
            t.str("element").unwrap_or("undefined"),
            t.str("label").unwrap_or(""),
            r(g("x")),
            r(g("y")),
            r(g("w")),
            r(g("h")),
            js_num(g("rotation"))
        ),
        "section" => format!(
            "s|{}|{}|{}|{}|{}|{}",
            t.str("title").unwrap_or("undefined"),
            r(g("x")),
            r(g("y")),
            r(g("w")),
            r(g("h")),
            js_num(g("rotation"))
        ),
        "text" => format!(
            "t|{}|{}|{}|{}",
            t.str("text").unwrap_or("undefined"),
            r(g("x")),
            r(g("y")),
            r(g("w"))
        ),
        kind => {
            let b = t.bounds();
            format!("{kind}|{}|{}", r(b.x), r(b.y))
        }
    }
}

/// Who a thing is the work of, as the chat reads it. Only `Chat` may be changed by the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    /// Placed by the chat and nobody has touched it.
    Chat,
    /// The chat's, and a person has moved or changed it since.
    Edited,
    /// A person's own.
    Person,
}

pub fn who_of(t: &Thing) -> Who {
    match t.chat() {
        None => Who::Person,
        Some(mark) if mark == chat_mark(t) => Who::Chat,
        Some(_) => Who::Edited,
    }
}

/// A piece's size across a region `w` wide: a library element keeps its own height; a small one (a button, a chip)
/// its own width and a wide one (a card, a bar) the region's.
fn size_of(piece: &ResolvedPiece, w: f64) -> (f64, f64) {
    match piece {
        ResolvedPiece::New { .. } => (w.min(360.0), NEW_H),
        ResolvedPiece::Library { element, .. } => match element_of(element) {
            None => (w.min(160.0), 80.0),
            Some(e) => (if e.w >= 240.0 || e.set == "navigation" { w } else { e.w }, e.h),
        },
    }
}

fn marked(t: Thing) -> Thing {
    let mark = chat_mark(&t);
    t.with_chat(mark)
}

fn design(id: String, element: &str, label: Option<String>, b: Bounds) -> Thing {
    let mut v = json!({"id": id, "kind": "design", "element": element,
        "x": num(b.x), "y": num(b.y), "w": num(b.w), "h": num(b.h)});
    if let Some(label) = label {
        v["label"] = Value::String(label);
    }
    marked(Thing::from_value(&v).expect("a design thing has an id"))
}

/// Makes the new things' ids from a seed, so placing the same layout twice gives the same ids.
struct Mint<'a> {
    seed: &'a str,
    n: u32,
}

impl Mint<'_> {
    fn id(&mut self, kind: &str) -> String {
        self.n += 1;
        format!("{kind}-{}-{}", self.seed, self.n)
    }
}

/// The right edge, and the top, of what's already on the board: the chat's work goes clear of it, to the right.
fn free_origin(things: &[&Thing]) -> (f64, f64) {
    let mut right = f64::NEG_INFINITY;
    let mut top = f64::INFINITY;
    for t in things.iter().filter(|t| !t.is_line()) {
        let b = t.bounds();
        right = right.max(b.x + b.w);
        top = top.min(b.y);
    }
    if right == f64::NEG_INFINITY {
        (snap(MARGIN), snap(MARGIN))
    } else {
        (snap(right + MARGIN), snap(MARGIN.max(top)))
    }
}

/// What one device's page of a design is: the pieces in place, the page's width and height.
struct PageOf {
    things: Vec<Thing>,
    w: f64,
    h: f64,
}

/// One design as a page on a device (design-view.md section 8). `variation` None is Now, the real page as it is;
/// otherwise the pieces are stacked in the changed region, and the page grows by what they need beyond the region
/// (what lies below moves down). A layout with no page is a new screen: a blank page of the device's size with the
/// pieces down it. Pieces are ordinary board things on the page; the page is one thing, and a faint mark over the
/// changed region sits between them.
fn page_for(
    mint: &mut Mint,
    layout: &ResolvedLayout,
    variation: Option<&ResolvedVariation>,
    name: &str,
    device: Device,
    at: (f64, f64),
) -> PageOf {
    let (pw, ph) = device.size();
    let region_name = match (variation, &layout.page) {
        (Some(_), Some(p)) => p.region.as_str(),
        _ => "",
    };
    let label = format!("{region_name}\n{name}");
    let piece_things = |mint: &mut Mint, pieces: &[ResolvedPiece], bx: f64, by: f64, bw: f64| {
        let mut y = 0.0;
        let mut things = Vec::new();
        for p in pieces {
            let (w, h) = size_of(p, bw);
            let b = Bounds {
                x: at.0 + bx,
                y: at.1 + by + y,
                w,
                h,
            };
            things.push(match p {
                ResolvedPiece::Library { element, label } => design(mint.id("design"), element, label.clone(), b),
                ResolvedPiece::New { name, purpose } => design(
                    mint.id("design"),
                    "newComponent",
                    Some(format!("{}\n{}", truncate(name, 40), truncate(purpose, 120))),
                    b,
                ),
            });
            y += h + PIECE_GAP;
        }
        (things, f64::max(0.0, y - PIECE_GAP))
    };
    let pieces = variation.map_or(&[][..], |v| &v.pieces[..]);
    let Some(page) = &layout.page else {
        let (inner, inner_h) = piece_things(mint, pieces, SCREEN_PAD, SCREEN_PAD, pw - SCREEN_PAD * 2.0);
        let h = ph.max(inner_h + SCREEN_PAD * 2.0);
        let page = design(
            mint.id("design"),
            &blank_element_id(device),
            Some(label),
            Bounds {
                x: at.0,
                y: at.1,
                w: pw,
                h,
            },
        );
        return PageOf {
            things: std::iter::once(page).chain(inner).collect(),
            w: pw,
            h,
        };
    };
    let (rx, ry, rw, rh) = region_of(&page.id, device, &page.region)
        .map_or((SCREEN_PAD, SCREEN_PAD, pw - SCREEN_PAD * 2.0, 0.0), |r| {
            (r.x, r.y, r.w, r.h)
        });
    let (inner, inner_h) = match variation {
        Some(_) => piece_things(mint, pieces, rx, ry, rw),
        None => (Vec::new(), rh),
    };
    let extra = if variation.is_some() {
        (inner_h - rh).max(0.0)
    } else {
        0.0
    };
    let h = ph + extra;
    let page_thing = design(
        mint.id("design"),
        &page_element_id(&page.id, device),
        Some(label),
        Bounds {
            x: at.0,
            y: at.1,
            w: pw,
            h,
        },
    );
    let mark = design(
        mint.id("design"),
        "regionMark",
        None,
        Bounds {
            x: at.0 + rx - MARK,
            y: at.1 + ry - MARK,
            w: rw + MARK * 2.0,
            h: rh.max(inner_h) + MARK * 2.0,
        },
    );
    PageOf {
        things: [page_thing, mark].into_iter().chain(inner).collect(),
        w: pw,
        h,
    }
}

/// A design's name on the board: a small sticky, the board's own note, beside its first page.
fn label_for(mint: &mut Mint, text: &str, colour: usize, at: (f64, f64)) -> Thing {
    let v = json!({"id": mint.id("sticky"), "kind": "sticky", "x": num(at.0), "y": num(at.1),
        "w": num(STICKY), "h": num(STICKY / 2.0), "text": truncate(text, 60), "colour": colour});
    marked(Thing::from_value(&v).expect("a sticky has an id"))
}

/// What placing a layout did to the board.
#[derive(Debug, Clone)]
pub struct Placement {
    /// The board as it is after: what stayed and what was added.
    pub things: Vec<Thing>,
    /// The ids of the new things, in the order they are written.
    pub added: Vec<String>,
    /// The ids of the chat's own untouched things taken away.
    pub removed: Vec<String>,
}

/// `things` with a checked layout added in a free area to the right of what's there: Now first (when the change is to
/// a page), then each variation, one under another, each as its phone page and its desktop page side by side, with
/// its name on a small sticky at the left. No heading, no Frames, no titles. Things a person has touched are never
/// changed: the layout only ever adds, and `replaces` takes away only things that are the chat's own and still look
/// as it placed them (anything else named there stays). Every id in `replaces` is checked here, whatever the
/// layout check did. `seed` makes the new ids.
pub fn place_layout(things: &[Thing], layout: &ResolvedLayout, seed: &str) -> Placement {
    let mut mint = Mint { seed, n: 0 };
    let mut take_away: Vec<String> = Vec::new();
    for id in &layout.replaces {
        let own = things
            .iter()
            .find(|t| t.id() == id)
            .is_some_and(|t| who_of(t) == Who::Chat);
        if own && !take_away.contains(id) {
            take_away.push(id.clone());
        }
    }
    let kept: Vec<&Thing> = things
        .iter()
        .filter(|t| !take_away.iter().any(|id| id == t.id()))
        .collect();
    let origin = free_origin(&kept);
    let mut added: Vec<Thing> = Vec::new();
    let mut designs: Vec<(String, Option<&ResolvedVariation>)> = Vec::new();
    if layout.page.is_some() {
        designs.push(("Now".to_string(), None));
    }
    designs.extend(layout.variations.iter().map(|v| (v.name.clone(), Some(v))));
    let left = origin.0 + LABEL_W;
    let mut y = origin.1;
    for (i, (name, variation)) in designs.iter().enumerate() {
        let mut x = left;
        let mut tallest: f64 = 0.0;
        for device in Device::PHONE_FIRST {
            let page = page_for(&mut mint, layout, *variation, name, device, (x, y));
            x = snap(x + page.w + PAGE_GAP);
            tallest = tallest.max(page.h);
            added.extend(page.things);
        }
        let colour = if i == 0 && layout.page.is_some() {
            0
        } else {
            (i % 4) + 1
        };
        added.push(label_for(&mut mint, name, colour, (origin.0, y)));
        y = snap(y + tallest + ROW_GAP);
    }
    let added_ids = added.iter().map(|t| t.id().to_string()).collect();
    let mut all: Vec<Thing> = kept.into_iter().cloned().collect();
    all.extend(added);
    Placement {
        things: all,
        added: added_ids,
        removed: take_away,
    }
}

/// One thing of the board as the session reads it (`BoardReadPiece`).
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub id: String,
    pub kind: &'static str,
    pub element: Option<String>,
    pub text: Option<String>,
    pub frame: Option<String>,
    pub who: Who,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

/// What the session reads of the board before it answers: what is on it, who put it there and what people wrote.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BoardRead {
    pub pieces: Vec<Piece>,
    pub selected: Vec<String>,
}

/// A thing's words as the session reads them: its sticky text, a section's title, a comment's first line, a design
/// thing's label.
fn words_of(t: &Thing) -> Option<String> {
    let raw: Option<String> = match t.kind() {
        "sticky" | "text" => t.str("text").map(str::to_string),
        "section" => t.str("title").map(str::to_string),
        "comment" => {
            t.0.get("thread")
                .and_then(|a| a.get(0))
                .and_then(|m| m.get("text"))
                .and_then(Value::as_str)
                .map(str::to_string)
        }
        "design" => t.str("label").map(|l| l.replacen('\n', " · ", 1)),
        _ => None,
    };
    let one = raw.map(|r| r.split_whitespace().collect::<Vec<_>>().join(" "))?;
    (!one.is_empty()).then(|| truncate(&one, WORDS))
}

/// The Frame (section) a thing sits in, by its middle, the smallest if frames are nested.
fn frame_around(t: &Thing, sections: &[&Thing]) -> Option<String> {
    let b = t.bounds();
    let (mx, my) = (b.x + b.w / 2.0, b.y + b.h / 2.0);
    let area = |s: &Thing| s.f64("w").unwrap_or(0.0) * s.f64("h").unwrap_or(0.0);
    let mut best: Option<&Thing> = None;
    for s in sections.iter().filter(|s| s.id() != t.id()) {
        let sb = s.bounds();
        if mx >= sb.x && mx <= sb.x + sb.w && my >= sb.y && my <= sb.y + sb.h && best.is_none_or(|b| area(s) < area(b))
        {
            best = Some(s);
        }
    }
    best.map(|s| s.id().to_string())
}

/// What the chat reads of the board before it answers. Notes first (people's words are the brief), then the rest in
/// board order, capped. Pen lines and connectors are left out; every other kind is `other` with no words.
pub fn read_board(things: &[Thing], selected: &[String], max: usize) -> BoardRead {
    let sections: Vec<&Thing> = things.iter().filter(|t| t.kind() == "section").collect();
    let rows: Vec<&Thing> = things.iter().filter(|t| !t.is_line()).collect();
    let note = |t: &&Thing| matches!(t.kind(), "sticky" | "text" | "comment");
    let ordered = rows
        .iter()
        .filter(|t| note(t))
        .chain(rows.iter().filter(|t| !note(t)))
        .take(max);
    let pieces: Vec<Piece> = ordered
        .map(|t| {
            let b = t.bounds();
            Piece {
                id: t.id().to_string(),
                kind: match t.kind() {
                    "design" => "design",
                    "sticky" => "sticky",
                    "text" => "text",
                    "section" => "section",
                    "comment" => "comment",
                    _ => "other",
                },
                element: (t.kind() == "design").then(|| t.str("element").unwrap_or_default().to_string()),
                text: words_of(t),
                frame: frame_around(t, &sections),
                who: who_of(t),
                x: js_round(b.x) as i64,
                y: js_round(b.y) as i64,
                w: js_round(b.w) as i64,
                h: js_round(b.h) as i64,
            }
        })
        .collect();
    let here: HashSet<&str> = pieces.iter().map(|p| p.id.as_str()).collect();
    let selected = selected
        .iter()
        .filter(|id| here.contains(id.as_str()))
        .take(8)
        .cloned()
        .collect();
    BoardRead { pieces, selected }
}

/// The ids of pieces the chat placed and nobody touched: the only ones a layout may replace.
pub fn own_ids(board: &BoardRead) -> HashSet<String> {
    board
        .pieces
        .iter()
        .filter(|p| p.who == Who::Chat)
        .map(|p| p.id.clone())
        .collect()
}

/// The board as the prompt shows it, a line a piece, or that it is empty (`boardText`).
pub fn board_text(board: &BoardRead) -> String {
    if board.pieces.is_empty() {
        return "(the board is empty)".to_string();
    }
    let lines: Vec<String> = board
        .pieces
        .iter()
        .map(|p| {
            let who = match p.who {
                Who::Chat => "chat (untouched, yours to replace)",
                Who::Edited => "edited by a person (never change it)",
                Who::Person => "a person's own (never change it)",
            };
            format!(
                "- `{}` {}{}{} at ({}, {}) {} × {}{}: {who}",
                p.id,
                p.kind,
                p.element
                    .as_ref()
                    .filter(|e| !e.is_empty())
                    .map_or(String::new(), |e| format!(" `{e}`")),
                p.text.as_ref().map_or(String::new(), |t| format!(
                    " \"{}\"",
                    crate::text::collapse_whitespace(t)
                )),
                p.x,
                p.y,
                p.w,
                p.h,
                p.frame.as_ref().map_or(String::new(), |f| format!(" in frame `{f}`")),
            )
        })
        .collect();
    let picked = if board.selected.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nThe person has picked: {}.",
            board
                .selected
                .iter()
                .map(|id| format!("`{id}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!("{}{picked}", lines.join("\n"))
}
