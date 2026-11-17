//! Approval reuse for a `build` or `fix` run (docs/specs/pieces/03-behavior.md "Approval reuse and tier escalation";
//! `reviewStep` and `git.ts`' `sliceChanges` and `patchDrift` in the TypeScript agent): the store hands back the head
//! a review approved (`approvedSha`) and, for a fix sent only to merge the base in, `conflictOf`; a head that is that
//! one, whose own changes read the same, or (a conflict fix only) read the same but for a resolution's few lines,
//! needs no new review. The store does no git; this is the agent's.
use std::collections::HashMap;
use std::path::Path;

use crate::lanes::git_output_in_worktree as git;

/// The most a merge of the base may change of an approved slice's own lines ([`patch_drift`]) and keep its approval:
/// about one conflict's resolution, the base's new version of a few lines both sides changed and the line merged
/// from them.
pub(super) const RESOLUTION_MAX_LINES: usize = 12;

/// Why a head needs no new review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reuse {
    /// It is the approved head.
    SameHead,
    /// Its own changes are exactly the approved ones (a rebase, or the base merged in without a conflict).
    SameSlice,
    /// A conflict's resolution changed this many lines of the approved changes.
    Resolution(usize),
}

/// A diff cut down to what it changes: no index or hunk-header lines, so the same change at other line numbers
/// reads the same. Use with `git diff -U0`.
fn normalize_patch(diff: &str) -> String {
    diff.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter(|l| !l.starts_with("index ") && !l.starts_with("@@ "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// A slice's own changes, from where it branched off `base_ref` to `tip`, leaving out `exclude` (the backlog file,
/// which every slice edits). `None` if git can't tell (e.g. `tip` isn't in the repo any more).
fn slice_changes(dir: &Path, base_ref: &str, tip: &str, exclude: &str) -> Option<String> {
    let mb = git(dir, &["merge-base", base_ref, tip]).ok()?;
    let exclude = format!(":(exclude){exclude}");
    let mut args = vec!["diff", "-U0", "--no-color", "--no-ext-diff", mb.trim(), tip, "--", "."];
    // A project with no backlog file has nothing to leave out (an empty exclude would leave out everything).
    if !exclude.ends_with(')') {
        args.push(&exclude);
    }
    let diff = git(dir, &args).ok()?;
    Some(normalize_patch(&diff))
}

/// A normalized patch's files: each one's header (its `diff --git`, mode and `---`/`+++` lines) and the lines after
/// it. `None` with a binary file in it, whose change the patch doesn't show.
fn patch_files(patch: &str) -> Option<Vec<(String, Vec<&str>)>> {
    let mut sections: Vec<(Vec<&str>, Vec<&str>)> = Vec::new();
    let mut in_header = false;
    for l in patch.split('\n') {
        if l.starts_with("Binary files ") || l.starts_with("GIT binary patch") {
            return None;
        }
        if l.starts_with("diff --git ") {
            sections.push((vec![l], Vec::new()));
            in_header = true;
            continue;
        }
        let Some((header, lines)) = sections.last_mut() else {
            continue;
        };
        let changed = l.starts_with('+') || l.starts_with('-');
        // The header runs to its `+++` line (a file with no text change has none: all of it is header).
        if in_header && !changed {
            header.push(l);
        } else if in_header && (l.starts_with("--- ") || l.starts_with("+++ ")) {
            header.push(l);
            if l.starts_with("+++ ") {
                in_header = false;
            }
        } else {
            in_header = false;
            lines.push(l);
        }
    }
    Some(sections.into_iter().map(|(h, l)| (h.join("\n"), l)).collect())
}

/// How far two of a slice's normalized patches are apart: the changed lines (`+` or `-`) in one and not the other,
/// file by file, counted as multisets. `None` when they don't touch the same files the same way (a file added or
/// dropped, or its header changed): that's never a small difference.
fn patch_drift(a: &str, b: &str) -> Option<usize> {
    let (fa, fb) = (patch_files(a)?, patch_files(b)?);
    let by_header: HashMap<&str, &Vec<&str>> = fb.iter().map(|(h, l)| (h.as_str(), l)).collect();
    // Two sections under one header (a file listed twice) is a shape this doesn't compare.
    if fa.len() != fb.len() || by_header.len() != fb.len() {
        return None;
    }
    let mut drift = 0;
    for (header, lines_a) in &fa {
        let lines_b = by_header.get(header.as_str())?;
        let mut left: HashMap<&str, usize> = HashMap::new();
        for l in lines_a {
            *left.entry(l).or_default() += 1;
        }
        for l in *lines_b {
            match left.get_mut(l) {
                Some(n) if *n > 0 => *n -= 1,
                _ => drift += 1,
            }
        }
        drift += left.values().sum::<usize>();
    }
    Some(drift)
}

/// Whether `head` (the lane's HEAD) needs no new review given the piece's approved head, and why. A run that has
/// no `approved` is never reused. The drift rule applies only when the fix was sent just to merge the base in
/// (`conflict_of` is the approved head).
pub(super) fn reuse(
    cwd: &Path,
    base_ref: &str,
    backlog_path: &str,
    head: &str,
    approved: Option<&str>,
    conflict_of: Option<&str>,
) -> Option<Reuse> {
    let approved = approved.filter(|a| !a.is_empty())?;
    if approved == head {
        return Some(Reuse::SameHead);
    }
    let then = slice_changes(cwd, base_ref, approved, backlog_path)?;
    let now = slice_changes(cwd, base_ref, head, backlog_path)?;
    if then == now {
        return Some(Reuse::SameSlice);
    }
    if conflict_of? != approved {
        return None;
    }
    patch_drift(&then, &now)
        .filter(|d| *d <= RESOLUTION_MAX_LINES)
        .map(Reuse::Resolution)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(lines: &[&str]) -> String {
        let mut p = String::from("diff --git a/f b/f\n--- a/f\n+++ b/f\n");
        p += &lines.join("\n");
        p
    }

    #[test]
    fn normalizing_drops_index_and_hunk_header_lines_and_carriage_returns() {
        let raw = "diff --git a/f b/f\r\nindex 1..2 100644\r\n--- a/f\r\n+++ b/f\r\n@@ -1 +1 @@\r\n+x\r\n";
        assert_eq!(normalize_patch(raw), "diff --git a/f b/f\n--- a/f\n+++ b/f\n+x");
    }

    #[test]
    fn drift_counts_changed_lines_in_one_patch_and_not_the_other_as_multisets() {
        let a = patch(&["+one", "+two", "-gone"]);
        assert_eq!(patch_drift(&a, &a), Some(0));
        assert_eq!(patch_drift(&a, &patch(&["+one", "+TWO", "-gone"])), Some(2));
        // The same line twice is two lines, not one.
        assert_eq!(patch_drift(&patch(&["+x", "+x"]), &patch(&["+x"])), Some(1));
    }

    #[test]
    fn drift_is_none_when_the_files_differ_or_a_file_is_binary() {
        let a = patch(&["+one"]);
        let other = a.replace("a/f b/f", "a/g b/g");
        assert_eq!(patch_drift(&a, &other), None);
        assert_eq!(patch_drift(&a, ""), None);
        assert_eq!(
            patch_drift(&a, "diff --git a/f b/f\nBinary files a/f and b/f differ"),
            None
        );
    }
}
