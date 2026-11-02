//! A draft's changed files with their patches, the answer to the app's `draft_files` command
//! (docs/specs/agent/06.5-spec-drafts.md, "Changes, not whole files").

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::lanes;

/// One changed file: its whole `content` and, when edited, a `patch` (the unified diff).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftFile {
    pub path: String,
    /// The path a renamed file had before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    /// Why a file is listed in full instead of as a patch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The files changed on `HEAD` against `origin/<base_branch>` (the three-dot diff `checks` uses).
pub fn draft_files(lane_dir: &Path, base_branch: &str) -> Result<Vec<DraftFile>> {
    let range = format!("origin/{base_branch}...HEAD");
    let status = lanes::git_output_in_worktree(lane_dir, &["diff", "-M", "--name-status", "-z", &range])
        .context("git diff --name-status")?;
    let mut parts = status.split('\0').filter(|p| !p.is_empty());
    let mut files = Vec::new();
    while let Some(code) = parts.next() {
        let first = parts.next().unwrap_or_default();
        let (old, path) = if code.starts_with('R') || code.starts_with('C') {
            (Some(first), parts.next().unwrap_or_default())
        } else {
            (None, first)
        };
        files.push(describe(lane_dir, &range, code, old, path)?);
    }
    Ok(files)
}

fn describe(lane_dir: &Path, range: &str, code: &str, old: Option<&str>, path: &str) -> Result<DraftFile> {
    let mut file = DraftFile {
        path: path.to_string(),
        old_path: old.map(str::to_string),
        content: String::new(),
        patch: None,
        deleted: code.starts_with('D'),
        note: None,
    };
    if file.deleted {
        return Ok(file);
    }
    let bytes = std::fs::read(lane_dir.join(path)).with_context(|| format!("reading {path}"))?;
    file.content = String::from_utf8_lossy(&bytes).into_owned();
    if let Some(old) = old {
        file.note = Some(format!("Renamed from {old}, so it is listed in full."));
    } else if code.starts_with('M') {
        let patch =
            lanes::git_output_in_worktree(lane_dir, &["diff", "-M", "-U3", range, "--", path]).context("git diff")?;
        if patch
            .lines()
            .any(|l| l.starts_with("Binary files ") || l == "GIT binary patch")
        {
            file.note = Some("Binary file, so it is listed in full.".to_string());
        } else if !patch.is_empty() {
            file.patch = Some(patch);
        }
    } else if bytes.contains(&0) {
        file.note = Some("Binary file, so it is listed in full.".to_string());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git, repo_with_origin, run};

    fn commit(repo: &Path, file: &str, contents: &[u8]) {
        std::fs::write(repo.join(file), contents).unwrap();
        run(git(repo).args(["add", file]));
        run(git(repo).args(["commit", "--quiet", "-m", file]));
    }

    fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(tmp.path());
        let big: String = (0..500).map(|i| format!("line {i}\n")).collect();
        commit(&repo, "big.md", big.as_bytes());
        commit(&repo, "old.md", b"some words that stay the same\nmore words here\n");
        commit(&repo, "gone.md", b"bye\n");
        commit(&repo, "pic.bin", &[0, 1, 2, 3]);
        run(git(&repo).args(["push", "--quiet", "origin", "main"]));
        run(git(&repo).args(["checkout", "--quiet", "-b", "draft"]));
        (tmp, repo)
    }

    fn find<'a>(files: &'a [DraftFile], path: &str) -> &'a DraftFile {
        files.iter().find(|f| f.path == path).unwrap()
    }

    #[test]
    fn one_line_edit_to_a_large_file_is_a_small_patch() {
        let (_tmp, repo) = setup();
        let big: String = (0..500)
            .map(|i| {
                if i == 250 {
                    "changed\n".to_string()
                } else {
                    format!("line {i}\n")
                }
            })
            .collect();
        commit(&repo, "big.md", big.as_bytes());
        let files = draft_files(&repo, "main").unwrap();
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.content, big);
        let patch = f.patch.as_ref().unwrap();
        assert!(patch.contains("-line 250\n+changed\n"));
        assert!(patch.lines().count() < 20);
        assert!(f.note.is_none() && !f.deleted);
    }

    #[test]
    fn new_file_has_content_only_and_deleted_is_marked() {
        let (_tmp, repo) = setup();
        commit(&repo, "new.md", b"fresh\n");
        run(git(&repo).args(["rm", "--quiet", "gone.md"]));
        run(git(&repo).args(["commit", "--quiet", "-m", "rm"]));
        let files = draft_files(&repo, "main").unwrap();
        let new = find(&files, "new.md");
        assert_eq!(new.content, "fresh\n");
        assert!(new.patch.is_none() && new.note.is_none() && !new.deleted);
        let gone = find(&files, "gone.md");
        assert!(gone.deleted && gone.content.is_empty() && gone.patch.is_none());
    }

    #[test]
    fn rename_shows_both_paths_and_a_reason() {
        let (_tmp, repo) = setup();
        run(git(&repo).args(["mv", "old.md", "renamed.md"]));
        run(git(&repo).args(["commit", "--quiet", "-m", "mv"]));
        let files = draft_files(&repo, "main").unwrap();
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!((f.path.as_str(), f.old_path.as_deref()), ("renamed.md", Some("old.md")));
        assert!(f.note.as_ref().unwrap().contains("old.md"));
        assert!(f.patch.is_none());
        assert!(f.content.contains("more words"));
    }

    #[test]
    fn binary_file_gives_a_reason_and_no_patch() {
        let (_tmp, repo) = setup();
        commit(&repo, "pic.bin", &[0, 9, 8, 7]);
        commit(&repo, "other.bin", &[0, 1]);
        let files = draft_files(&repo, "main").unwrap();
        for name in ["pic.bin", "other.bin"] {
            let f = find(&files, name);
            assert!(f.patch.is_none());
            assert!(f.note.as_ref().unwrap().contains("Binary"));
        }
        let json = serde_json::to_value(find(&files, "pic.bin")).unwrap();
        assert!(json.get("patch").is_none() && json.get("oldPath").is_none());
    }
}
