//! The closed sets a layout may use and how the prompt lists them (TS `designBoard.ts` `pieces`, `designCatalog`,
//! `libraryText`, `pagesText`; `app/src/ui/designPageList.ts`, `designPageRegions.ts`, `designPageData.ts`).
//!
//! The data is a copy of what the app's drawing table holds, in `data/`: `elements.json` is `designElements` without
//! the old `shape-` elements (id, label, set, size: the drawings themselves are the app's), and `pages.json` is
//! `DESIGN_PAGES` and `PAGE_REGIONS` (the parts of each real page a change can be made to, written by the app's
//! `npm run design:extract`). Change them in Retaskable.com first, then copy them here.
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use serde::Deserialize;

use super::layout::Catalog;

/// One library element: what the drawing table says of it (not its drawing).
#[derive(Debug, Clone, Deserialize)]
pub struct Element {
    pub id: String,
    pub label: String,
    pub set: String,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Page {
    pub id: String,
    pub label: String,
}

/// One part of a page a change can be made to, with where it sits on the page.
#[derive(Debug, Clone, Deserialize)]
pub struct Region {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Deserialize)]
struct PagesFile {
    pages: Vec<Page>,
    regions: HashMap<String, HashMap<String, Vec<Region>>>,
}

/// The two sizes every design is shown at, the real viewports (points); `phone` is drawn first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Phone,
    Desktop,
}

impl Device {
    pub const PHONE_FIRST: [Device; 2] = [Device::Phone, Device::Desktop];

    pub fn name(self) -> &'static str {
        match self {
            Device::Phone => "phone",
            Device::Desktop => "desktop",
        }
    }

    /// The page's own size on this device (`DEVICES`).
    pub fn size(self) -> (f64, f64) {
        match self {
            Device::Desktop => (1280.0, 800.0),
            Device::Phone => (390.0, 844.0),
        }
    }
}

fn elements_file() -> &'static Vec<Element> {
    static E: OnceLock<Vec<Element>> = OnceLock::new();
    E.get_or_init(|| serde_json::from_str(include_str!("data/elements.json")).expect("data/elements.json"))
}

fn pages_file() -> &'static PagesFile {
    static P: OnceLock<PagesFile> = OnceLock::new();
    P.get_or_init(|| serde_json::from_str(include_str!("data/pages.json")).expect("data/pages.json"))
}

/// The pieces a layout may use: every library element.
pub fn elements() -> &'static [Element] {
    elements_file()
}

/// A library element by id (`elementOf`).
pub fn element_of(id: &str) -> Option<&'static Element> {
    elements_file().iter().find(|e| e.id == id)
}

pub fn pages() -> &'static [Page] {
    &pages_file().pages
}

/// The parts of `page` on `device`.
pub fn regions_of(page: &str, device: Device) -> &'static [Region] {
    pages_file()
        .regions
        .get(page)
        .and_then(|d| d.get(device.name()))
        .map_or(&[], Vec::as_slice)
}

/// What `region` names on a page's device, or the page's first card when this device doesn't show it (`regionOf`).
pub fn region_of(page: &str, device: Device, region: &str) -> Option<&'static Region> {
    let list = regions_of(page, device);
    list.iter()
        .find(|r| r.id == region)
        .or_else(|| list.iter().find(|r| r.kind == "card"))
        .or_else(|| list.first())
}

/// The element id of a page's drawing on a device: `page-board-desktop`.
pub fn page_element_id(page: &str, device: Device) -> String {
    format!("page-{page}-{}", device.name())
}

/// A new screen's blank page on a device.
pub fn blank_element_id(device: Device) -> String {
    format!("blank-{}", device.name())
}

/// The closed sets a layout may use: the library's piece ids, and each real page with the ids of the parts of it a
/// change can be made to (the union of what its desktop and its phone drawing show).
pub fn design_catalog() -> Catalog {
    Catalog {
        elements: elements().iter().map(|e| e.id.clone()).collect(),
        pages: pages()
            .iter()
            .map(|p| {
                // In the order the drawings show them (the first card is the fallback of a region that isn't one).
                let mut ids: Vec<String> = Vec::new();
                for r in [Device::Desktop, Device::Phone]
                    .iter()
                    .flat_map(|d| regions_of(&p.id, *d))
                {
                    if !ids.contains(&r.id) {
                        ids.push(r.id.clone());
                    }
                }
                (p.id.clone(), ids)
            })
            .collect::<BTreeMap<_, _>>(),
    }
}

/// The library as the prompt lists it, one line an element.
pub fn library_text() -> String {
    elements()
        .iter()
        .map(|e| format!("- `{}`: {} ({}, {} × {})", e.id, e.label, e.set, e.w, e.h))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The pages as the prompt lists them: each real page by id, and the parts of it a change can be made to, with where
/// each shows.
pub fn pages_text() -> String {
    pages()
        .iter()
        .map(|p| {
            // First seen first, as the TS `Map` keeps them: desktop's parts, then the phone's that desktop lacks.
            let mut parts: Vec<(&str, &str, &str, Vec<&str>)> = Vec::new();
            for device in [Device::Desktop, Device::Phone] {
                for r in regions_of(&p.id, device) {
                    match parts.iter_mut().find(|x| x.0 == r.id) {
                        Some(have) => have.3.push(device.name()),
                        None => parts.push((&r.id, &r.kind, &r.label, vec![device.name()])),
                    }
                }
            }
            let lines: Vec<String> = parts
                .iter()
                .map(|(id, kind, label, on)| {
                    let label = crate::text::collapse_whitespace(label);
                    format!(
                        "  - `{id}` ({kind}, on {}): {}",
                        on.join(" and "),
                        label.chars().take(70).collect::<String>()
                    )
                })
                .collect();
            format!("- `{}`: the {} page. Parts:\n{}", p.id, p.label, lines.join("\n"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_is_the_library_and_each_pages_parts() {
        let c = design_catalog();
        assert!(c.elements.contains("button") && c.elements.contains("newComponent"));
        assert!(!c.elements.iter().any(|e| e.starts_with("shape-")));
        assert_eq!(pages().len(), 8);
        let board = &c.pages["board"];
        assert!(board.iter().any(|r| r == "header") && board.iter().any(|r| r == "tabBar"));
        // Each id once, though the desktop and the phone both show it.
        assert_eq!(
            board.len(),
            board.iter().collect::<std::collections::BTreeSet<_>>().len()
        );
    }

    #[test]
    fn the_prompt_lists_name_the_parts_and_where_they_show() {
        assert!(library_text().contains("- `button`: Button (buttons, 160 × "));
        let pages = pages_text();
        assert!(pages.contains("`header` (header, on desktop and phone)"), "{pages}");
        assert!(pages.contains("- `board`: the Board page. Parts:\n"));
        assert!(pages.lines().all(|l| !l.contains('\n') && l.chars().count() < 200));
    }

    #[test]
    fn a_region_the_device_lacks_falls_to_its_first_card_and_a_page_with_none_to_nothing() {
        let r = region_of("board", Device::Phone, "waitingForALane").unwrap();
        assert_eq!(r.kind, "card");
        assert_eq!(region_of("board", Device::Desktop, "tabBar").unwrap().id, "tabBar");
        assert!(region_of("nowhere", Device::Phone, "x").is_none());
        assert_eq!(page_element_id("board", Device::Phone), "page-board-phone");
        assert_eq!(blank_element_id(Device::Desktop), "blank-desktop");
    }
}
