//! Tier escalation for a `build` or `fix` run (docs/specs/pieces/03-behavior.md "Approval reuse and tier
//! escalation"; `agent/src/tierEscalation.ts` and `review.ts` in the TypeScript agent): once a review approves, the
//! real diff's facts can raise the piece's tier above routine. The reasons go on `pr_opened` as `result.held`, and
//! the store holds the piece for a person's check once its watch is green.
use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::change_tier::{Choices, DiffFile, DiffSignals, RiskProfile, Size, Tier, TierResult, escalate_tier};
use crate::lanes::git_output_in_worktree as git;

/// The longest `held` the store takes.
pub(super) const HELD_MAX: usize = 500;

const DEP_KEYS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];

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

/// What the lane's diff against `origin/<base>` shows (`diffSignals`): migration text and new dependencies. `None`
/// when git can't say.
fn diff_signals(dir: &Path, base_branch: &str, backlog_path: &str, size: Option<Size>) -> Option<DiffSignals> {
    let diff = crate::scouts::diff_info(dir, base_branch, backlog_path).ok()?;
    let base = format!("origin/{base_branch}");
    let range = format!("{base}...HEAD");
    let files = diff
        .files
        .iter()
        .map(|f| {
            let migration = crate::change_tier::is_migration(&f.file);
            let added_text = migration
                .then(|| git(dir, &["diff", &range, "--", &f.file]).ok())
                .flatten()
                .map(|patch| {
                    patch
                        .lines()
                        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
                        .collect::<Vec<_>>()
                        .join("\n")
                });
            DiffFile {
                path: f.file.clone(),
                added: f.lines,
                removed: 0,
                added_text,
            }
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
    Some(DiffSignals {
        files,
        new_dependencies,
        size,
    })
}

/// The reasons the lane's diff holds the piece for a check, cut to [`HELD_MAX`]; `None` when it raises nothing.
/// The project's risk profile says which kinds of change may ship; facts that can't be read never use it (they take
/// Early's usual, one tier up).
pub(super) fn held(
    dir: &Path,
    base_branch: &str,
    backlog_path: &str,
    size: Option<&str>,
    profile: Option<&RiskProfile>,
) -> Option<String> {
    let plan = TierResult {
        tier: Tier::Routine,
        reasons: Vec::new(),
    };
    let result = match diff_signals(dir, base_branch, backlog_path, size.and_then(Size::parse)) {
        Some(signals) => escalate_tier(plan, &signals, &Choices::of(profile)),
        None => {
            let unread = DiffSignals {
                new_dependencies: true,
                ..Default::default()
            };
            escalate_tier(plan, &unread, &Choices::of(None))
        }
    };
    (result.tier != Tier::Routine).then(|| result.reasons.join("; ").chars().take(HELD_MAX).collect())
}
