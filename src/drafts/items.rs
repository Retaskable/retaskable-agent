//! The backlog items a plan adds (Retaskable.com's backlog.md section 5, `agent/src/draftItems.ts`): the drafter returns
//! them in its result's `items` and writes no `Items:` header; the agent keeps them with the plan and sends them to the
//! store when the plan merges (`driver/plan_items.rs`). Here: the check on what the drafter returned, the plan's tier as
//! the default `checking`, each item's milestone from its spec, and the list of open items the drafter is shown.
use std::path::{Component, Path};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::backlog::{Item, ItemStatus};
use crate::spec_items::{is_item_id, milestone_of};
use crate::text::{squish, truncate};

static SECTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\s+sections?\b.*$").expect("a valid regex"));

/// One entry of the drafter's `items`, as it wrote it; [`checked_items`] decides what's kept.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawItem {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<String>,
    #[serde(default, rename = "specRefs")]
    pub spec_refs: Vec<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub milestone: Option<String>,
    #[serde(default)]
    pub checking: Option<String>,
    #[serde(default, rename = "checkingWhy")]
    pub checking_why: Option<String>,
}

/// An item a plan adds, as the store's `add` takes it (`DraftItem` in `shared/draftTypes.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub spec_refs: Vec<String>,
    /// `tweak` or `feature`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    /// Who checks it before it goes live: `manual` (a person) or `auto`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checking_why: Option<String>,
}

/// The non-blank trimmed strings of `v`, each once.
fn strings(v: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in v.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// `checkedItems` (draftItems.ts): the drafter's `items`, checked. An id that isn't a readable slug, is named twice or is the
/// item the plan revises is dropped, and so is an item with no title. A dependency that isn't a slug, or is the item itself, is
/// dropped (one the store doesn't have is dropped when it is added). `derive`: an item with no `checking` stays without one, for
/// the plan's tier to fill in ([`with_derived_checking`]); otherwise it is a person's check. A spec ref loses a trailing
/// " section 3", so it names a file.
pub fn checked_items(raw: &[RawItem], revises: Option<&str>, derive: bool) -> Vec<PlanItem> {
    let mut out: Vec<PlanItem> = Vec::new();
    for r in raw {
        let id = r.id.as_deref().unwrap_or("").trim();
        let title = truncate(&squish(r.title.as_deref().unwrap_or("")), 300);
        if !is_item_id(id) || title.is_empty() || Some(id) == revises || out.iter().any(|i| i.id == id) {
            continue;
        }
        let depends_on = strings(r.depends_on.clone())
            .into_iter()
            .filter(|d| is_item_id(d) && d != id)
            .collect();
        let spec_refs = strings(r.spec_refs.iter().map(|s| SECTION.replace(s, "").into_owned()));
        let milestone = truncate(&squish(r.milestone.as_deref().unwrap_or("")), 100);
        let why = truncate(&squish(r.checking_why.as_deref().unwrap_or("")), 200);
        let called = matches!(r.checking.as_deref(), Some("auto" | "manual"));
        // The plan's call on who checks it; anything but "auto" is a person's check.
        let checking = if r.checking.as_deref() == Some("auto") {
            "auto"
        } else {
            "manual"
        };
        out.push(PlanItem {
            id: id.to_string(),
            title,
            depends_on,
            spec_refs,
            kind: r.kind.clone().filter(|k| k == "tweak" || k == "feature"),
            milestone: (!milestone.is_empty()).then_some(milestone),
            checking: (!derive || called).then(|| checking.to_string()),
            checking_why: (!why.is_empty()).then_some(why),
        });
    }
    out
}

/// `withDerivedChecking` (shared/changeTier.ts): the plan's tier as a default for each item the drafter left without a `checking`
/// call. `tryfirst` and `needsyou` are `manual` (a person's Looks good before it merges), with the tier's reasons as the why
/// (at most 200 characters); `routine` leaves it unset, so the project's `playtests` setting governs. An item the drafter did
/// call is never changed.
pub fn with_derived_checking(items: &[PlanItem], tier: &str, reasons: &[String]) -> Vec<PlanItem> {
    if tier == "routine" {
        return items.to_vec();
    }
    let why = truncate(&squish(&reasons.join("; ")), 200);
    (items.iter())
        .map(|i| match i.checking {
            Some(_) => i.clone(),
            None => PlanItem {
                checking: Some("manual".into()),
                checking_why: (!why.is_empty()).then(|| why.clone()),
                ..i.clone()
            },
        })
        .collect()
}

/// `withMilestones` (draftItems.ts): each item's milestone, from the `Theme:` line of the spec that holds it (its first spec ref)
/// in the plan's checkout `dir`. One that has a milestone, has no spec ref, points outside `dir` or can't be read is left as it is.
pub fn with_milestones(items: Vec<PlanItem>, dir: &Path) -> Vec<PlanItem> {
    items
        .into_iter()
        .map(|item| {
            let Some(first) = item.spec_refs.first().filter(|_| item.milestone.is_none()) else {
                return item;
            };
            let path = Path::new(first);
            let inside = path
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
            let milestone = inside
                .then(|| std::fs::read_to_string(dir.join(path)).ok())
                .flatten()
                .and_then(|text| milestone_of(&text));
            match milestone {
                Some(milestone) => PlanItem {
                    milestone: Some(milestone),
                    ..item
                },
                None => item,
            }
        })
        .collect()
}

/// Each item's kind by id, and the kinds of all of them in order when every one is a tweak or a feature (none otherwise): the
/// facts the store decides a plan's tier and design review from (`itemKinds`).
pub fn kinds_of(items: &[PlanItem]) -> (std::collections::BTreeMap<String, String>, Vec<String>) {
    let by_id = (items.iter())
        .filter_map(|i| Some((i.id.clone(), i.kind.clone()?)))
        .collect::<std::collections::BTreeMap<_, _>>();
    let all = match items.len() == by_id.len() {
        true => items.iter().filter_map(|i| i.kind.clone()).collect(),
        false => Vec::new(),
    };
    (by_id, all)
}

/// The most open items a drafter is shown.
const OPEN_MAX: usize = 200;

/// `openItemsText` (draftItems.ts): the backlog's items that aren't built yet, as the drafter sees them (to depend on, and to
/// keep its ids apart from); empty when there are none.
pub fn open_items_text(items: &[Item]) -> String {
    let open: Vec<&Item> = (items.iter())
        .filter(|i| {
            !matches!(
                i.status,
                ItemStatus::OnMain | ItemStatus::InProduction | ItemStatus::Skipped
            )
        })
        .collect();
    if open.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = (open.iter().take(OPEN_MAX))
        .map(|i| match i.depends_on.is_empty() {
            true => format!("- {}: {}", i.id, i.title),
            false => format!("- {}: {} (depends on {})", i.id, i.title, i.depends_on.join(", ")),
        })
        .collect();
    if open.len() > OPEN_MAX {
        lines.push(format!("(and {} more)", open.len() - OPEN_MAX));
    }
    format!(
        "The backlog's items not built yet (id: title), which a new item may depend on; built ones are not listed, and a new item's id must differ from every id here:\n{}",
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests;
