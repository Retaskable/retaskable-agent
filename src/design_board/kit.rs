//! The design step with a project's kit (docs/specs/project-kits.md section 2, TS `designBoard.ts` `boardLayoutOf`, `kitCatalogOf`):
//! the closed sets a layout is checked against, and the layout as the idea's board draws it.
use super::catalog::design_catalog;
use super::layout::{Catalog, ResolvedLayout, ResolvedPiece, ResolvedVariation};
use crate::kit::{Kit, Tier, kit_catalog};

/// The closed sets a layout may use for `kit`: Retaskable's own library and real pages, or the project's kit.
pub fn catalog_of(kit: &Kit) -> Catalog {
    if kit.tier == Tier::Library {
        design_catalog()
    } else {
        kit_catalog(kit)
    }
}

/// The board's own library element a kit kind is drawn with, so **Change it** has pieces to edit by hand (a kind with none is a
/// placeholder).
fn board_element(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "button" => "button",
        "input" => "textField",
        "header" => "header",
        "tabBar" => "bottomBar",
        "list" => "listGroup",
        "card" => "card",
        "toggle" => "switchRow",
        "badge" => "badge",
        "chip" => "chip",
        "nav" => "segmented",
        _ => return None,
    })
}

/// A layout in the active kit's ids as the idea's board draws it: library ids for the kinds the library has, a new-piece placeholder for
/// the rest, and a new screen (no real page). Retaskable's own library tier is as it was.
pub fn board_layout_of(layout: &ResolvedLayout, kit: &Kit) -> ResolvedLayout {
    if kit.tier == Tier::Library {
        return layout.clone();
    }
    let variations = layout
        .variations
        .iter()
        .map(|v| ResolvedVariation {
            name: v.name.clone(),
            pieces: v
                .pieces
                .iter()
                .map(|p| match p {
                    ResolvedPiece::New { .. } => p.clone(),
                    ResolvedPiece::Library { element, label } => {
                        let found = kit.pieces.iter().find(|k| &k.id == element);
                        match found.and_then(|k| board_element(&k.kind)) {
                            Some(e) => ResolvedPiece::Library {
                                element: e.to_string(),
                                label: label.clone(),
                            },
                            None => ResolvedPiece::New {
                                name: label
                                    .clone()
                                    .or_else(|| found.map(|k| k.name.clone()))
                                    .unwrap_or_else(|| element.clone()),
                                purpose: format!(
                                    "{} from this project's kit.",
                                    found.map_or("A piece", |k| k.name.as_str())
                                ),
                            },
                        }
                    }
                })
                .collect(),
        })
        .collect();
    ResolvedLayout {
        page: None,
        variations,
        replaces: layout.replaces.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::{Genre, library, wireframe_kit};

    fn layout() -> ResolvedLayout {
        ResolvedLayout {
            page: None,
            variations: vec![ResolvedVariation {
                name: "A".into(),
                pieces: vec![
                    ResolvedPiece::Library {
                        element: "button".into(),
                        label: Some("Go".into()),
                    },
                    ResolvedPiece::Library {
                        element: "table".into(),
                        label: None,
                    },
                    ResolvedPiece::New {
                        name: "Thing".into(),
                        purpose: "p".into(),
                    },
                ],
            }],
            replaces: vec!["x".into()],
        }
    }

    #[test]
    fn the_library_tier_is_checked_and_drawn_as_it_was() {
        assert_eq!(board_layout_of(&layout(), &library()), layout());
        assert!(catalog_of(&library()).elements.contains("button"));
    }

    #[test]
    fn a_kit_piece_is_drawn_by_its_kind_and_one_with_no_board_element_is_a_placeholder() {
        let kit = wireframe_kit(Genre::App, None, None, None);
        let out = board_layout_of(&layout(), &kit);
        assert_eq!(
            out.variations[0].pieces,
            [
                ResolvedPiece::Library {
                    element: "button".into(),
                    label: Some("Go".into())
                },
                ResolvedPiece::New {
                    name: "Table".into(),
                    purpose: "Table from this project's kit.".into()
                },
                ResolvedPiece::New {
                    name: "Thing".into(),
                    purpose: "p".into()
                },
            ]
        );
        assert!(out.page.is_none() && out.replaces == ["x"]);
        assert!(catalog_of(&kit).elements.contains("table") && !catalog_of(&kit).elements.contains("switchRow"));
    }
}
