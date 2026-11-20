use serde_json::json;

use super::*;

fn piece_json(id: &str, kind: &str) -> Value {
    json!({ "id": id, "name": id, "kind": kind })
}

fn catalogue() -> Value {
    json!({ "tier": "catalogue", "genre": "app", "fingerprint": "0123456789abcdef",
        "pieces": [piece_json("dataGrid", "table"), piece_json("button", "button")],
        "tokens": { "colors": { "primary": "#112233", "surface": "#ffffff" }, "spacing": [4, 8] },
        "pages": [{ "id": "home", "name": "Home", "kind": "screen" }] })
}

#[test]
fn a_good_kit_is_read_back_whole_and_from_a_string() {
    let kit = read_kit(&catalogue()).unwrap();
    assert_eq!((kit.tier, kit.pieces.len()), (Tier::Catalogue, 2));
    assert_eq!(read_kit(&json!(catalogue().to_string())), Some(kit));
}

#[test]
fn a_kit_that_breaks_its_shape_is_refused() {
    let with = |f: &dyn Fn(&mut Value)| {
        let mut k = catalogue();
        f(&mut k);
        read_kit(&k)
    };
    assert!(
        with(&|k| k["pieces"][0]["kind"] = json!("hologram")).is_none(),
        "a kind outside the closed set"
    );
    assert!(with(&|k| k["pieces"][0]["id"] = json!("9bad")).is_none());
    assert!(
        with(&|k| k["pieces"][1]["id"] = json!("dataGrid")).is_none(),
        "ids differ"
    );
    assert!(
        with(&|k| k["pieces"] = json!([])).is_none(),
        "a kit that is not the library has pieces"
    );
    assert!(with(&|k| k["extra"] = json!(1)).is_none(), "no unknown keys");
    assert!(with(&|k| k["tokens"]["colors"]["primary"] = json!("blue")).is_none());
    assert!(with(&|k| k["pages"][0]["kind"] = json!("page")).is_none());
    assert!(with(&|k| k["fingerprint"] = json!("zz")).is_none());
    assert!(with(&|k| k["pieces"][0]["name"] = json!("two\nlines")).is_none());
    assert!(with(&|k| k["pieces"][0]["options"] = json!(["a", "b", "c", "d", "e", "f", "g", "h", "i"])).is_none());
}

#[test]
fn the_library_has_no_pieces_and_a_string_far_over_the_byte_cap_is_refused() {
    assert!(read_kit(&json!({ "tier": "library", "pieces": [] })).is_some());
    assert!(read_kit(&json!("x".repeat(KIT_MAX_BYTES * 2 + 1))).is_none());
    let sixty_one: Vec<Value> = (0..61).map(|i| piece_json(&format!("p{i}"), "card")).collect();
    assert!(
        read_kit(&json!({ "tier": "catalogue", "pieces": sixty_one })).is_none(),
        "at most 60 pieces"
    );
}

#[test]
fn capping_keeps_a_kit_that_fits_whole_and_cuts_extra_pieces_and_pages() {
    let pieces: Vec<Piece> = (0..70)
        .map(|i| Piece {
            id: format!("p{i}"),
            name: "n".into(),
            kind: "card".into(),
            options: Some(vec!["a".into()]),
        })
        .collect();
    let pages: Vec<Page> = (0..40)
        .map(|i| Page {
            id: format!("pg{i}"),
            name: "n".into(),
            kind: "screen".into(),
        })
        .collect();
    let kit = Kit {
        tier: Tier::Catalogue,
        genre: None,
        fingerprint: None,
        pieces,
        tokens: None,
        pages: Some(pages),
    };
    let capped = cap_kit(kit).unwrap();
    assert_eq!((capped.pieces.len(), capped.pages.unwrap().len()), (60, 30));
    assert!(
        capped.pieces.iter().all(|p| p.options.is_some()),
        "it fit, so the options stay"
    );
}

#[test]
fn the_wireframe_kit_is_plain_and_a_game_adds_its_own() {
    let app = wireframe_kit(Genre::App, None, None, None);
    assert_eq!((app.tier, app.pieces.len()), (Tier::Wireframe, 13));
    let game = wireframe_kit(Genre::Game, None, None, Some("0123456789abcdef".into()));
    assert!(game.pieces.iter().any(|p| p.kind == "healthBar") && game.pieces.len() == 19);
    assert!(read_kit(&serde_json::to_value(&game).unwrap()).is_some());
}

#[test]
fn a_layout_is_checked_against_the_kits_ids_and_pages() {
    let kit = read_kit(&catalogue()).unwrap();
    let cat = kit_catalog(&kit);
    assert!(cat.elements.contains("dataGrid") && !cat.elements.contains("card"));
    assert_eq!(cat.pages["home"], ["content"]);
    assert_eq!(
        pieces_text(&kit),
        "- `dataGrid`: dataGrid (table)\n- `button`: button (button)"
    );
    assert!(pages_text(&kit).contains("- `home`: the Home screen. Parts:\n  - `content`"));
    assert!(pages_text(&wireframe_kit(Genre::App, None, None, None)).starts_with("(none:"));
}

#[test]
fn the_accent_is_a_primary_brand_or_accent_colour_never_a_variant() {
    let kit = read_kit(&catalogue()).unwrap();
    assert_eq!(accent_of(&kit).as_deref(), Some("#112233"));
    let mut plain = kit.clone();
    plain.tokens.as_mut().unwrap().colors = Some(
        [
            ("primaryMuted".to_string(), "#aaaaaa".to_string()),
            ("surface".into(), "#ffffff".into()),
        ]
        .into(),
    );
    assert_eq!(accent_of(&plain), None);
    let mut accent = kit;
    accent.tokens.as_mut().unwrap().colors = Some([("brandBlue".to_string(), "#0000ff".to_string())].into());
    assert_eq!(accent_of(&accent).as_deref(), Some("#0000ff"));
}

#[test]
fn a_review_carries_only_the_pieces_it_uses_and_the_library_carries_nothing() {
    let kit = read_kit(&catalogue()).unwrap();
    let r = review_kit_of(&kit, &["button".to_string()], Some("home")).unwrap();
    assert_eq!(
        r,
        json!({ "tier": "catalogue", "pieces": [{ "id": "button", "name": "button", "kind": "button" }], "accent": "#112233", "pageName": "Home" })
    );
    assert!(review_kit_of(&library(), &[], None).is_none());
    let (note, lib, pages) = prompt_text(&kit, || "L".into(), || "P".into());
    assert!(
        note.contains("not Retaskable's own")
            && note.contains("tier catalogue")
            && lib.contains("dataGrid")
            && pages.contains("home")
    );
    assert_eq!(
        prompt_text(&library(), || "L".into(), || "P".into()),
        (String::new(), "L".into(), "P".into())
    );
}
