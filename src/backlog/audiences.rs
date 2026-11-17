//! The company's audience list from its `audiences` fact (shared/audiences.ts): the starters, then the ones the
//! company added, with its renames applied. The fact holds only `{ added, edits, assignments }`.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Audience {
    pub id: String,
    pub name: String,
    /// Who they are, in a sentence.
    #[serde(default)]
    pub who: String,
    /// The part of the product they use.
    #[serde(default)]
    pub part: String,
    #[serde(default)]
    pub trial: bool,
}

#[derive(Debug, Default, Deserialize)]
struct Edit {
    name: Option<String>,
    who: Option<String>,
    part: Option<String>,
    trial: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct Book {
    #[serde(default)]
    added: Vec<Value>,
    #[serde(default)]
    edits: BTreeMap<String, Value>,
}

const STARTERS: [(&str, &str, &str, &str, bool); 8] = [
    ("admin", "Admin", "Runs the admin panel.", "admin", false),
    (
        "support",
        "Support",
        "Looks up a customer and fixes an account.",
        "admin",
        false,
    ),
    ("shopper", "Shopper", "Buys from the shop.", "shop", true),
    (
        "guest",
        "Guest",
        "Looks around the shop before signing in.",
        "shop",
        false,
    ),
    (
        "analyst",
        "Analyst",
        "Reads the reports and checks the numbers.",
        "reports",
        false,
    ),
    (
        "buyer",
        "Buyer",
        "Chooses the product for a company.",
        "enterprise",
        true,
    ),
    (
        "account-admin",
        "Account admin",
        "Manages the people on an enterprise account.",
        "enterprise",
        false,
    ),
    (
        "infrastructure",
        "Infrastructure",
        "Keeps the servers, builds and deploys running for everyone else.",
        "platform",
        false,
    ),
];

fn starters() -> Vec<Audience> {
    STARTERS
        .iter()
        .map(|&(id, name, who, part, trial)| Audience {
            id: id.into(),
            name: name.into(),
            who: who.into(),
            part: part.into(),
            trial,
        })
        .collect()
}

fn apply(mut a: Audience, edit: Option<&Value>) -> Audience {
    let Some(edit) = edit.and_then(|e| serde_json::from_value::<Edit>(e.clone()).ok()) else {
        return a;
    };
    let text = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(name) = text(edit.name) {
        a.name = name;
    }
    if let Some(who) = text(edit.who) {
        a.who = who;
    }
    if let Some(part) = edit.part {
        a.part = part;
    }
    if let Some(trial) = edit.trial {
        a.trial = trial;
    }
    a
}

/// Starters first, then the audiences the company added (a malformed one skipped), de-duplicated by id, edits
/// applied. `data` is the fact's data; anything that isn't a book means an empty one.
pub fn audiences_of(data: &Value) -> Vec<Audience> {
    let book: Book = serde_json::from_value(data.clone()).unwrap_or_default();
    let added = book
        .added
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Audience>(v).ok());
    let mut seen = HashSet::new();
    starters()
        .into_iter()
        .chain(added)
        .filter(|a| !a.name.trim().is_empty() && seen.insert(a.id.clone()))
        .map(|a| {
            let edit = book.edits.get(&a.id);
            apply(a, edit)
        })
        .collect()
}

#[cfg(test)]
mod tests;
