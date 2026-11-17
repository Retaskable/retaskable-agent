//! A plan's item ids must reach the store, and in this agent they reach it only through an Items header in a spec
//! (`spec_items.rs`, `report.rs`'s `upsert_from_specs`). A plan that names an id no header adds would be reported ready
//! and then build nothing (draft-28e6e812, 2026-11-17: `docker-ci-info-line` was named, never added, and the store got
//! "0 new"). So before a plan is reported, every id it names is looked for in the Items headers of its branch; a missing
//! one is put back to the drafter once, and if it is still missing the draft fails with that reason.
//!
//! The TS agent has no such hole, by another route: its drafter returns `items` in its result and the agent adds them to
//! the store when the plan merges (backlog.md section 5), writing no header at all. This agent has not ported that.
use std::collections::BTreeSet;

use crate::spec_items::{SPECS_DIR, read_spec_items};

/// The ids in `ids` that no spec in `specs` (path and text, the top-level specs of the plan's branch) adds in its Items header.
pub(super) fn missing(specs: &[(String, String)], ids: &[String]) -> Vec<String> {
    let added: BTreeSet<String> = specs
        .iter()
        .flat_map(|(path, text)| read_spec_items(path, text).0)
        .map(|i| i.id)
        .collect();
    ids.iter().filter(|id| !added.contains(*id)).cloned().collect()
}

fn pronoun(missing: &[String]) -> &'static str {
    if missing.len() == 1 { "it" } else { "them" }
}

/// What the drafter is told when its plan names items no header adds.
pub(super) fn correction(missing: &[String]) -> String {
    let named = missing
        .iter()
        .map(|id| format!("`{id}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let them = pronoun(missing);
    format!(
        "You named {named} in `itemIds`, but no Items header in the specs on your branch adds {them}, so Retaskable would never \
         create {them} and nothing would be built. Add {them} to the `Items:` block in the opening of the spec that holds the piece \
         (before its first `##` heading, under its `Theme:` and `Kind:` lines; add the block if the spec has none), one line per item \
         in exactly this form, ending with a full stop:\n\n\
         Items:\n- **<id>**: <the piece's title>. Depends on: <none, or ids joined by \", \">.\n\n\
         The id is the slug you returned. Then commit, and return the same result again."
    )
}

/// Why the draft fails when the drafter still hasn't added them.
pub(super) fn failure(missing: &[String]) -> String {
    let them = pronoun(missing);
    format!(
        "The plan names {} but no Items header in its specs adds {them}, so nothing would ever be built from it \
         (the drafter was asked once to add {them}).",
        missing.join(", "),
    )
}

/// The top-level specs of a worktree (path and text), where Items headers live; a split spec's header is in its index.
pub(crate) fn read_specs(dir: &std::path::Path) -> std::io::Result<Vec<(String, String)>> {
    let mut specs = Vec::new();
    for entry in std::fs::read_dir(dir.join(SPECS_DIR))? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "md")
            && path.is_file()
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
        {
            specs.push((format!("{SPECS_DIR}/{name}"), std::fs::read_to_string(&path)?));
        }
    }
    specs.sort();
    Ok(specs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(items: &str) -> Vec<(String, String)> {
        vec![(
            "docs/specs/a.md".into(),
            format!("# A\n\nTheme: T\n\n{items}\n## 1. Body\n"),
        )]
    }

    #[test]
    fn an_id_a_header_adds_is_not_missing() {
        let specs = spec("Items:\n- **a-one**: One. Depends on: none.\n");
        assert!(missing(&specs, &["a-one".into()]).is_empty());
    }

    #[test]
    fn an_id_no_header_adds_is_missing_and_a_header_below_the_opening_does_not_count() {
        let late = vec![(
            "docs/specs/a.md".into(),
            "# A\n\n## 1. Body\n\nItems:\n- **a-one**: One. Depends on: none.\n".to_string(),
        )];
        assert_eq!(missing(&late, &["a-one".into()]), ["a-one"]);
        assert_eq!(missing(&spec(""), &["x".into(), "y".into()]), ["x", "y"]);
    }

    #[test]
    fn the_correction_names_the_id_and_the_format() {
        let text = correction(&["docker-ci-info-line".into()]);
        assert!(
            text.contains("`docker-ci-info-line`") && text.contains("- **<id>**: <the piece's title>. Depends on:")
        );
    }

    #[test]
    fn read_specs_reads_the_top_level_markdown_of_the_specs_folder() {
        let dir = std::env::temp_dir().join(format!("plan-items-{}", std::process::id()));
        let specs = dir.join(SPECS_DIR);
        std::fs::create_dir_all(specs.join("sub")).unwrap();
        std::fs::write(specs.join("a.md"), "# A\n").unwrap();
        std::fs::write(specs.join("b.txt"), "x").unwrap();
        std::fs::write(specs.join("sub").join("c.md"), "# C\n").unwrap();
        assert_eq!(
            read_specs(&dir).unwrap(),
            [("docs/specs/a.md".to_string(), "# A\n".to_string())]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
