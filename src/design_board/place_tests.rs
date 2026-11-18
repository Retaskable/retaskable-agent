//! The placing's tests (TS `whiteboardDesignPlace.test.ts`, the parts the agent keeps).
use serde_json::{Value, json};

use super::catalog::{Device, regions_of};
use super::layout::{PageRef, ResolvedLayout, ResolvedPiece, ResolvedVariation};
use super::place::*;
use super::things::Thing;

fn region() -> String {
    regions_of("board", Device::Desktop)
        .iter()
        .find(|r| r.kind == "card")
        .unwrap()
        .id
        .clone()
}

fn lib(element: &str, label: Option<&str>) -> ResolvedPiece {
    ResolvedPiece::Library {
        element: element.into(),
        label: label.map(str::to_string),
    }
}

fn layout(replaces: Vec<String>) -> ResolvedLayout {
    ResolvedLayout {
        page: Some(PageRef {
            id: "board".into(),
            region: region(),
        }),
        variations: vec![
            ResolvedVariation {
                name: "A: button".into(),
                pieces: vec![lib("card", None), lib("button", Some("Snooze"))],
            },
            ResolvedVariation {
                name: "B: new".into(),
                pieces: vec![ResolvedPiece::New {
                    name: "Duration picker".into(),
                    purpose: "Pick how long.".into(),
                }],
            },
        ],
        replaces,
    }
}

fn sticky() -> Thing {
    Thing::from_value(
        &json!({"id": "s1", "kind": "sticky", "x": 10, "y": 10, "w": 200, "h": 200,
        "text": "Keep it to one tap", "colour": 0, "author": "sam"}),
    )
    .unwrap()
}

fn element(t: &Thing) -> &str {
    t.str("element").unwrap_or_default()
}

fn pages(things: &[Thing]) -> Vec<&Thing> {
    things
        .iter()
        .filter(|t| t.kind() == "design" && element(t).starts_with("page-"))
        .collect()
}

fn name_of(t: &Thing) -> &str {
    t.str("label")
        .unwrap_or_default()
        .split('\n')
        .nth(1)
        .unwrap_or_default()
}

fn x(t: &Thing) -> f64 {
    t.f64("x").unwrap()
}

#[test]
fn adds_now_and_each_variation_as_a_phone_page_and_a_desktop_page_all_marked_as_the_chats() {
    let p = place_layout(&[sticky()], &layout(vec![]), "seed");
    assert_eq!(
        p.things.len(),
        1 + p.added.len(),
        "the sticky stays and everything else is new"
    );
    assert!(
        !p.things.iter().any(|t| t.kind() == "section" || t.kind() == "text"),
        "no Frames, no headings"
    );
    let pages = pages(&p.things);
    let names: Vec<String> = pages
        .iter()
        .map(|t| format!("{} {}", name_of(t), element(t).split('-').nth(2).unwrap()))
        .collect();
    assert_eq!(
        names,
        [
            "Now phone",
            "Now desktop",
            "A: button phone",
            "A: button desktop",
            "B: new phone",
            "B: new desktop"
        ]
    );
    for t in &pages {
        let want = if element(t).ends_with("desktop") { 1280.0 } else { 390.0 };
        assert_eq!(t.f64("w"), Some(want), "the real viewport widths");
    }
    let labels: Vec<&str> = p
        .things
        .iter()
        .filter(|t| t.kind() == "sticky" && t.chat().is_some())
        .map(|t| t.str("text").unwrap())
        .collect();
    assert_eq!(labels, ["Now", "A: button", "B: new"]);
    for id in &p.added {
        assert_eq!(who_of(p.things.iter().find(|t| t.id() == id).unwrap()), Who::Chat);
    }
    assert_eq!(who_of(&sticky()), Who::Person);
}

#[test]
fn lays_the_pages_out_as_a_grid_clear_of_the_board_on_the_grid() {
    let p = place_layout(&[sticky()], &layout(vec![]), "seed");
    let pages = pages(&p.things);
    let desk: Vec<&&Thing> = pages.iter().filter(|t| element(t).ends_with("desktop")).collect();
    let phone: Vec<&&Thing> = pages.iter().filter(|t| element(t).ends_with("phone")).collect();
    assert!(desk.windows(2).all(|w| x(w[0]) == x(w[1])) && phone.windows(2).all(|w| x(w[0]) == x(w[1])));
    assert!(
        x(desk[0]) > x(phone[0]) + phone[0].f64("w").unwrap(),
        "beside the phone page, not over it"
    );
    let row = |i: usize| desk[i].f64("h").unwrap().max(phone[i].f64("h").unwrap());
    let y = |t: &Thing| t.f64("y").unwrap();
    assert!(
        y(desk[1]) >= y(desk[0]) + row(0) && y(desk[2]) >= y(desk[1]) + row(1),
        "no row overlaps"
    );
    let gap = |a: usize, b: usize| y(desk[b]) - (y(desk[a]) + row(a));
    assert!(
        (gap(0, 1) - gap(1, 2)).abs() <= 8.0,
        "the same gap between rows, to the grid"
    );
    assert!(
        desk.iter().zip(&phone).all(|(d, p)| y(d) == y(p)),
        "a design's two pages are level"
    );
    let right = sticky().bounds().x + sticky().bounds().w;
    for id in &p.added {
        assert!(
            p.things.iter().find(|t| t.id() == id).unwrap().bounds().x > right,
            "to the right of the board"
        );
    }
    for t in pages {
        let b = t.bounds();
        assert!(b.x % 8.0 == 0.0 && b.y % 8.0 == 0.0, "on the grid");
    }
}

#[test]
fn a_variations_pieces_go_in_the_changed_region_and_the_region_is_marked_on_every_page() {
    let p = place_layout(&[], &layout(vec![]), "seed");
    let pages = pages(&p.things);
    let find = |name: &str, dev: &str| {
        *pages
            .iter()
            .find(|t| name_of(t) == name && element(t).ends_with(dev))
            .unwrap()
    };
    let (now, a) = (find("Now", "desktop"), find("A: button", "desktop"));
    let r = regions_of("board", Device::Desktop)
        .iter()
        .find(|r| r.id == region())
        .unwrap();
    let marks = p.things.iter().filter(|t| element(t) == "regionMark").count();
    assert_eq!(marks, 6, "Now and each variation, desktop and phone");
    let on = |t: &&Thing| {
        t.kind() == "design"
            && ["card", "button"].contains(&element(t))
            && x(t) >= x(a)
            && x(t) < x(a) + 1280.0
            && t.f64("y").unwrap() >= a.f64("y").unwrap()
            && t.f64("y").unwrap() < a.f64("y").unwrap() + a.f64("h").unwrap()
    };
    let inside: Vec<&Thing> = p.things.iter().filter(on).collect();
    assert_eq!(inside.len(), 2);
    for d in inside {
        assert!(
            x(d) == x(a) + r.x && d.f64("y").unwrap() >= a.f64("y").unwrap() + r.y,
            "in the region's place"
        );
    }
    assert_eq!(
        now.str("label").unwrap().split('\n').next(),
        Some(""),
        "Now vacates nothing"
    );
    assert_eq!(a.str("label").unwrap().split('\n').next(), Some(region().as_str()));
    assert!(
        a.f64("h") > now.f64("h"),
        "the page grows to hold pieces taller than the region"
    );
}

#[test]
fn a_new_component_is_the_dashed_placeholder_with_its_name_and_purpose_on_the_page() {
    let p = place_layout(&[], &layout(vec![]), "seed");
    let b = *pages(&p.things)
        .iter()
        .find(|t| name_of(t) == "B: new" && element(t).ends_with("desktop"))
        .unwrap();
    let fresh = p
        .things
        .iter()
        .find(|t| element(t) == "newComponent" && x(t) >= x(b))
        .unwrap();
    assert_eq!(fresh.str("label"), Some("Duration picker\nPick how long."));
    let (fb, pb) = (fresh.bounds(), b.bounds());
    assert!(fb.x >= pb.x && fb.x + fb.w <= pb.x + pb.w && fb.y >= pb.y && fb.y + fb.h <= pb.y + pb.h);
}

#[test]
fn a_change_with_no_page_is_a_new_screen_of_blank_pages_with_no_now() {
    let l = ResolvedLayout {
        page: None,
        variations: vec![ResolvedVariation {
            name: "A".into(),
            pieces: vec![lib("topBar", Some("Snooze")), lib("card", None)],
        }],
        replaces: vec![],
    };
    let p = place_layout(&[], &l, "seed");
    let blanks: Vec<&Thing> = p.things.iter().filter(|t| element(t).starts_with("blank-")).collect();
    let sizes: Vec<(&str, f64, f64)> = blanks
        .iter()
        .map(|t| (element(t), t.f64("w").unwrap(), t.f64("h").unwrap()))
        .collect();
    assert_eq!(sizes, [("blank-phone", 390.0, 844.0), ("blank-desktop", 1280.0, 800.0)]);
    assert!(
        !p.things.iter().any(|t| element(t) == "regionMark"),
        "no region to mark on a new screen"
    );
    let stickies: Vec<&str> = p
        .things
        .iter()
        .filter(|t| t.kind() == "sticky")
        .map(|t| t.str("text").unwrap())
        .collect();
    assert_eq!(stickies, ["A"]);
    let card = p.things.iter().find(|t| element(t) == "card").unwrap().bounds();
    let phone = blanks[0].bounds();
    assert!(card.x >= phone.x && card.y >= phone.y && card.x + card.w <= phone.x + phone.w);
}

#[test]
fn never_changes_a_thing_a_person_touched_only_adds_and_replaces_what_is_still_as_placed() {
    let first = place_layout(&[], &layout(vec![]), "one");
    let ids = first.added.clone();
    // A person moves the third thing.
    let mut things = first.things.clone();
    let nudged = x(&things[2]) + 5.0;
    things[2].0.insert("x".into(), json!(nudged));
    let moved = things[2].clone();
    assert_eq!(who_of(&moved), Who::Edited);
    let second = place_layout(&things, &layout(ids.clone()), "two");
    let mut removed = second.removed.clone();
    removed.sort();
    let mut want: Vec<String> = ids.iter().filter(|id| id.as_str() != moved.id()).cloned().collect();
    want.sort();
    assert_eq!(removed, want, "all but the edited one are replaced");
    assert_eq!(
        second.things.iter().find(|t| t.id() == moved.id()),
        Some(&moved),
        "exactly as the person left it"
    );
    // A person's own sticky named in `replaces` is not touched either, nor an id that isn't on the board.
    let mut with_sticky = vec![sticky()];
    with_sticky.extend(first.things.clone());
    let third = place_layout(&with_sticky, &layout(vec!["s1".into(), "nothing".into()]), "three");
    assert!(third.removed.is_empty() && third.things.iter().any(|t| t.id() == "s1"));
}

#[test]
fn the_same_seed_makes_the_same_ids_and_a_redraw_goes_beside_what_stays() {
    let a = place_layout(&[], &layout(vec![]), "same");
    let b = place_layout(&[], &layout(vec![]), "same");
    assert_eq!(a.added, b.added);
    let again = place_layout(&a.things, &layout(vec![]), "other");
    let right = a
        .things
        .iter()
        .map(|t| t.bounds().x + t.bounds().w)
        .fold(f64::MIN, f64::max);
    assert!(
        again
            .added
            .iter()
            .all(|id| again.things.iter().find(|t| t.id() == id).unwrap().bounds().x > right)
    );
    assert_eq!(again.things.len(), a.things.len() + again.added.len());
}

#[test]
fn the_chats_mark_is_how_a_thing_is_told_from_one_a_person_changed() {
    let t = Thing::from_value(
        &json!({"id": "d", "kind": "design", "element": "card", "label": "Hi\nthere",
        "x": 1.4, "y": -2.5, "w": 100, "h": 50}),
    )
    .unwrap();
    assert_eq!(chat_mark(&t), "d|card|Hi\nthere|1|-2|100|50|0");
    let sticky = Thing::from_value(&json!({"id": "s", "kind": "sticky", "x": 3, "y": 4, "w": 200, "h": 100})).unwrap();
    assert_eq!(chat_mark(&sticky), "sticky|3|4");
    assert_eq!(who_of(&t.clone().with_chat(chat_mark(&t))), Who::Chat);
    assert_eq!(who_of(&t.with_chat("d|card|other|1|-2|100|50|0".into())), Who::Edited);
}

fn read(things: &[Thing], selected: &[String], max: usize) -> BoardRead {
    read_board(things, selected, max)
}

#[test]
fn reads_notes_first_who_put_each_piece_there_and_the_picked_pieces() {
    let p = place_layout(&[sticky()], &layout(vec![]), "seed");
    let picked = vec![p.added[1].clone(), "nothing".to_string()];
    let r = read(&p.things, &picked, 80);
    assert_eq!(r.pieces[0].id, "s1", "people's notes first");
    assert_eq!(r.pieces[0].who, Who::Person);
    assert!(r.pieces.iter().any(|x| x.kind == "design"
        && x.who == Who::Chat
        && x.element.as_deref().is_some_and(|e| e.starts_with("page-"))
        && x.text.as_deref().is_some_and(|t| t.contains("Now"))));
    assert_eq!(r.selected, [p.added[1].clone()], "only the picks that are on the board");
    assert!(own_ids(&r).len() == p.added.len());
    assert_eq!(read(&p.things, &[], 3).pieces.len(), 3, "capped");
}

#[test]
fn a_piece_the_person_moved_reads_as_edited_and_lines_are_left_out() {
    let p = place_layout(&[], &layout(vec![]), "seed");
    let mut things: Vec<Thing> = p
        .things
        .iter()
        .map(|t| {
            let mut t = t.clone();
            t.0.insert("y".into(), json!(t.f64("y").unwrap() + 40.0));
            t
        })
        .collect();
    things.push(
        Thing::from_value(&json!({"id": "pen", "kind": "stroke", "points": [0, 0, 5, 5], "colour": 0, "width": 3}))
            .unwrap(),
    );
    let r = read(&things, &[], 80);
    assert!(
        r.pieces
            .iter()
            .filter(|x| x.kind == "design")
            .all(|x| x.who == Who::Edited)
    );
    assert!(!r.pieces.iter().any(|x| x.id == "pen"));
    assert!(own_ids(&r).is_empty(), "nothing is the chat's to replace once touched");
}

#[test]
fn a_thing_in_a_frame_names_the_smallest_one_and_the_board_text_lists_a_line_a_piece() {
    let frame = |id: &str, w: u32| {
        Thing::from_value(&json!({"id": id, "kind": "section", "x": 0, "y": 0, "w": w, "h": w, "title": id})).unwrap()
    };
    let note = Thing::from_value(&json!({"id": "n", "kind": "sticky", "x": 10, "y": 10, "w": 20, "h": 20,
        "text": "  one\n two ", "colour": 1}))
    .unwrap();
    let r = read(&[frame("big", 900), frame("small", 300), note], &[], 80);
    let n = r.pieces.iter().find(|p| p.id == "n").unwrap();
    assert_eq!(
        (n.frame.as_deref(), n.text.as_deref()),
        (Some("small"), Some("one two"))
    );
    let text = board_text(&r);
    assert!(
        text.contains(
            "- `n` sticky \"one two\" at (10, 10) 20 × 20 in frame `small`: a person's own (never change it)"
        )
    );
    assert_eq!(board_text(&BoardRead::default()), "(the board is empty)");
    let _: Value = json!(null);
}
