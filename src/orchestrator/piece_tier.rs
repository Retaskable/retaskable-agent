//! The real diff's facts for a `build` or `fix` run's `pr_opened` (docs/specs/pieces/03-behavior.md "Approval reuse and tier
//! escalation"; pools-on-server.md section 18.4): once a review approves, what the lane's diff shows (paths and sizes, a
//! migration's added text, new dependencies, the piece's size) goes on the result as `diff`. The store runs the shared tier rule with the
//! project's risk profile and holds the piece for a person's check; the agent decides nothing from them.
use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Value, json};

use crate::lanes::git_output_in_worktree as git;

const DEP_KEYS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];

/// Whether `path` (repo-relative) is a migration file: under a `migration` or `migrations` folder.
fn is_migration(path: &str) -> bool {
    let lower = path.to_lowercase();
    let mut parts: Vec<&str> = lower.split('/').collect();
    parts.pop();
    parts.iter().any(|p| matches!(*p, "migration" | "migrations"))
}

/// A package.json's dependency names at `rev`, each with its kind. An empty set when the file isn't there at that
/// revision (every dependency in the other is new); `None` when it can't be read (counts as a new dependency).
fn dep_names(dir: &Path, rev: &str, file: &str) -> Option<BTreeSet<String>> {
    let Ok(text) = git(dir, &["show", &format!("{rev}:{file}")]) else {
        return Some(BTreeSet::new());
    };
    let json: Value = serde_json::from_str(&text).ok()?;
    let object = json.as_object()?;
    Some(
        DEP_KEYS
            .iter()
            .flat_map(|k| {
                let names = object.get(*k).and_then(Value::as_object);
                names.into_iter().flatten().map(move |(n, _)| format!("{k}:{n}"))
            })
            .collect(),
    )
}

/// `diff` for `pr_opened`: `{ files: [{ path, added, removed, addedText? }], newDependencies, size? }` from the lane's diff against
/// `origin/<base>` (`addedText` only for a migration file, for the store to see a DROP or ALTER), or `{ unreadable: true }` when git
/// can't say: never guessed.
pub(super) fn diff_facts(dir: &Path, base_branch: &str, backlog_path: &str, size: Option<&str>) -> Value {
    let Ok(diff) = crate::scouts::diff_info(dir, base_branch, backlog_path) else {
        return json!({ "unreadable": true });
    };
    let base = format!("origin/{base_branch}");
    let range = format!("{base}...HEAD");
    let files: Vec<Value> = diff
        .files
        .iter()
        .map(|f| {
            let added_text = is_migration(&f.file)
                .then(|| git(dir, &["diff", &range, "--", &f.file]).ok())
                .flatten()
                .map(|patch| {
                    patch
                        .lines()
                        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
                        .collect::<Vec<_>>()
                        .join("\n")
                });
            let mut file = json!({ "path": f.file, "added": f.lines, "removed": 0 });
            if let Some(text) = added_text {
                file["addedText"] = json!(text);
            }
            file
        })
        .collect();
    let new_dependencies = diff
        .files
        .iter()
        .filter(|f| f.file == "package.json" || f.file.ends_with("/package.json"))
        .any(
            |f| match (dep_names(dir, &base, &f.file), dep_names(dir, "HEAD", &f.file)) {
                (Some(before), Some(after)) => after.iter().any(|n| !before.contains(n)),
                _ => true,
            },
        );
    let mut facts = json!({ "files": files, "newDependencies": new_dependencies });
    if let Some(size) = size.filter(|s| matches!(*s, "S" | "M" | "L" | "XL")) {
        facts["size"] = json!(size);
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_the_files_under_a_migration_folder() {
        assert!(
            is_migration("db/migrations/1.sql") && is_migration("Migration/2.sql") && is_migration("migrations/x.sql")
        );
        assert!(!is_migration("src/migrations.rs") && !is_migration("db/migrate/1.sql") && !is_migration("migrations"));
    }

    #[test]
    fn a_diff_git_cannot_read_is_unreadable_never_guessed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            diff_facts(dir.path(), "main", "docs/specs/build-order.md", Some("S")),
            json!({ "unreadable": true })
        );
    }
}
