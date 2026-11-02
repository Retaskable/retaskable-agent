//! Per-file diff line counts for scout planning (`DiffInfo`, `diffInfo` in Retaskable.com's
//! `agent/src/git.ts`). `--no-renames`: a rename reads as "src/{a => b}.ts", no path a scout could
//! `git diff -- <file>`.
use std::path::Path;

use anyhow::{Context, Result};

use crate::lanes;

/// One changed file's added-plus-deleted line count.
#[derive(Debug, Clone, PartialEq)]
pub struct FileLines {
    pub file: String,
    pub lines: u32,
}

/// What [`diff_info`] gathers: the stat text, each file's line count, whether the backlog file is
/// among them, and the total — [`super::plan::plan_scouts`]'s input.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffInfo {
    pub stat: String,
    pub files: Vec<FileLines>,
    pub backlog_changed: bool,
    pub total_lines: u32,
}

/// Gathers `lane_dir`'s diff against `origin/<base_branch>...HEAD`: the stat text and, per file,
/// how many lines it changed.
pub fn diff_info(lane_dir: &Path, base_branch: &str, backlog_path: &str) -> Result<DiffInfo> {
    let range = format!("origin/{base_branch}...HEAD");
    let stat = lanes::git_output_in_worktree(lane_dir, &["diff", "--stat", &range]).context("git diff --stat")?;
    let numstat = lanes::git_output_in_worktree(lane_dir, &["diff", "--numstat", "--no-renames", &range])
        .context("git diff --numstat")?;
    let files = parse_numstat(&numstat);
    let total_lines = files.iter().map(|f| f.lines).sum();
    let backlog_changed = files.iter().any(|f| f.file == backlog_path);
    Ok(DiffInfo {
        stat: stat.trim().to_string(),
        files,
        backlog_changed,
        total_lines,
    })
}

fn parse_numstat(numstat: &str) -> Vec<FileLines> {
    numstat
        .lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let mut parts = line.split('\t');
            let added: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let deleted: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let file = parts.collect::<Vec<_>>().join("\t");
            FileLines {
                file,
                lines: added + deleted,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git, repo_with_origin, run as run_git};

    fn commit(repo: &Path, file: &str, contents: &str) {
        let path = repo.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        run_git(git(repo).args(["add", file]));
        run_git(git(repo).args(["commit", "--quiet", "-m", file]));
    }

    #[test]
    fn diff_info_counts_each_files_added_and_deleted_lines_and_flags_the_backlog() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        commit(&repo, "docs/backlog.md", "a\nb\nc");
        commit(&repo, "src/new.rs", "fn new() {}\nfn more() {}");

        let diff = diff_info(&repo, "main", "docs/backlog.md").unwrap();

        assert!(diff.backlog_changed);
        assert_eq!(diff.total_lines, diff.files.iter().map(|f| f.lines).sum::<u32>());
        assert!(diff.files.iter().any(|f| f.file == "docs/backlog.md" && f.lines == 3));
        assert!(diff.files.iter().any(|f| f.file == "src/new.rs" && f.lines == 2));
        assert!(diff.stat.contains("2 files changed"), "{}", diff.stat);
    }

    #[test]
    fn diff_info_with_no_changes_has_no_files_and_isnt_backlog_changed() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());

        let diff = diff_info(&repo, "main", "docs/backlog.md").unwrap();

        assert!(diff.files.is_empty());
        assert!(!diff.backlog_changed);
        assert_eq!(diff.total_lines, 0);
    }
}
