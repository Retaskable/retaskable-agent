//! "Where to start" (agent/src/startFiles.ts): the files a fresh implementer session is pointed at
//! before it searches. The item's own spec files are read straight off disk, since the lane's
//! worktree is already checked out to the branch they're on; the history (the files its earlier
//! slices changed, and what changed since each spec last did) comes from a plain `git log`/
//! `git diff-tree`, run with the caller's own git binary so tests can swap in a fake one, matching
//! `release_notes::gather`'s pattern.
//!
//! Simplified where the backlog store's richer spec-link lines (`Specs: section 5.6; ...`) aren't
//! modelled yet: [`crate::backlog::Item::spec_refs`] is a flat list of file paths with no section,
//! so every spec is treated as section 6.2's "the line names no section" case — the whole file's
//! named code, not one section's.
use std::collections::HashSet;
use std::path::Path;

use regex::Regex;
use tokio::process::Command;

use crate::backlog::Item;
use crate::release_notes::Commit;

/// At most this many files across the three parts of "Where to start" (section 6.2).
pub const MAX_FILES: usize = 15;

/// At most this many commits per spec under "Changed since its specs last did".
pub const MAX_CHANGED_COMMITS: usize = 10;

fn is_markdown(path: &str) -> bool {
    path.to_lowercase().ends_with(".md")
}

fn to_forward_slashes(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The item's own spec files (`Item::spec_refs`), markdown only, deduplicated, kept only when they
/// exist under `root`.
pub fn item_spec_files(spec_refs: &[String], root: &Path) -> Vec<String> {
    let mut seen = HashSet::new();
    spec_refs
        .iter()
        .filter(|r| is_markdown(r) && seen.insert(r.as_str()))
        .filter(|r| root.join(r).is_file())
        .cloned()
        .collect()
}

/// A backticked span counts as a path when it has no character a path couldn't have and ends in a
/// short word-character extension (`src/foo.rs`, `Cargo.toml`), ruling out flags (`--max-turns`,
/// no dot), shell examples (`cargo test`, no dot) and calls (`session.save()`, has `(`).
fn looks_like_a_path(candidate: &str) -> bool {
    if candidate.chars().any(|c| "()[]{}<>\"'=,;:!?*|&".contains(c)) {
        return false;
    }
    match candidate.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty() && !ext.is_empty() && ext.len() <= 6 && ext.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// The backticked paths `spec_content` (read from `spec_path`, under `root`) names, resolved from
/// the repo root or, failing that, the spec's own folder, kept only when the file exists (section
/// 6.2: "the code its specs name" / "only files that exist, from the repo root or the spec's
/// folder"). Empty when it names more than [`MAX_FILES`] ("only if it names 15 or fewer").
pub fn named_code_paths(spec_path: &str, spec_content: &str, root: &Path) -> Vec<String> {
    let backticked = Regex::new(r"`([^`\s]+)`").unwrap();
    let spec_dir = Path::new(spec_path).parent();
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for caps in backticked.captures_iter(spec_content) {
        let candidate = &caps[1];
        if !looks_like_a_path(candidate) || !seen.insert(candidate.to_string()) {
            continue;
        }
        if root.join(candidate).is_file() {
            found.push(candidate.to_string());
            continue;
        }
        if let Some(dir) = spec_dir {
            let under_dir = dir.join(candidate);
            if root.join(&under_dir).is_file() {
                found.push(to_forward_slashes(&under_dir));
            }
        }
    }
    if found.len() > MAX_FILES {
        return Vec::new();
    }
    found
}

/// Escapes a string for use inside git's `-E` (extended regex) `--grep`.
fn grep_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The files changed by commits on `base_ref` titled `<item_id>:` (section 6.2: "the files its
/// earlier slices changed"), newest commit first, deduplicated in that order. Empty on any git
/// error (no repo, bad ref) rather than failing the whole build.
pub async fn slice_changed_files(git_bin: &Path, cwd: &Path, base_ref: &str, item_id: &str) -> Vec<String> {
    let grep = format!("--grep=^{}:", grep_escape(item_id));
    let Ok(log) = Command::new(git_bin)
        .args(["log", "--format=%H", "-E", &grep, base_ref])
        .current_dir(cwd)
        .output()
        .await
    else {
        return Vec::new();
    };
    if !log.status.success() {
        return Vec::new();
    }
    let shas: Vec<String> = String::from_utf8_lossy(&log.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for sha in shas {
        let Ok(diff) = Command::new(git_bin)
            .args(["diff-tree", "--no-commit-id", "--name-only", "-r", &sha])
            .current_dir(cwd)
            .output()
            .await
        else {
            continue;
        };
        if !diff.status.success() {
            continue;
        }
        for path in String::from_utf8_lossy(&diff.stdout).lines() {
            let path = path.trim();
            if !path.is_empty() && seen.insert(path.to_string()) {
                files.push(path.to_string());
            }
        }
    }
    files
}

/// The commits on `code_paths`, on `base_ref`, since `spec_path` itself last changed there (section
/// 6.2: "Changed since its specs last did"), newest first, capped at [`MAX_CHANGED_COMMITS`]. Empty
/// when there's no code to check, the spec's own last-changed date can't be found, or git fails.
pub async fn changed_since_spec(
    git_bin: &Path,
    cwd: &Path,
    base_ref: &str,
    spec_path: &str,
    code_paths: &[String],
) -> Vec<Commit> {
    if code_paths.is_empty() {
        return Vec::new();
    }
    let Ok(last_changed) = Command::new(git_bin)
        .args(["log", "-1", "--format=%cI", base_ref, "--", spec_path])
        .current_dir(cwd)
        .output()
        .await
    else {
        return Vec::new();
    };
    let since = String::from_utf8_lossy(&last_changed.stdout).trim().to_string();
    if !last_changed.status.success() || since.is_empty() {
        return Vec::new();
    }

    let mut args = vec![
        "log".to_string(),
        "--format=%H%x09%cI%x09%an%x09%s".to_string(),
        format!("--since={since}"),
        base_ref.to_string(),
        "--".to_string(),
    ];
    args.extend(code_paths.iter().cloned());
    let Ok(log) = Command::new(git_bin).args(&args).current_dir(cwd).output().await else {
        return Vec::new();
    };
    if !log.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&log.stdout)
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .take(MAX_CHANGED_COMMITS)
        .map(|line| {
            let mut parts = line.splitn(4, '\t');
            Commit {
                sha: parts.next().unwrap_or_default().to_string(),
                date: parts.next().unwrap_or_default().to_string(),
                author: parts.next().unwrap_or_default().to_string(),
                subject: parts.next().unwrap_or_default().to_string(),
            }
        })
        .collect()
}

/// One spec's commits since it last changed, for "Changed since its specs last did".
pub struct SpecChanges {
    pub spec: String,
    pub commits: Vec<Commit>,
}

/// "Where to start": at most [`MAX_FILES`] files in all — the item's own spec files, then the files
/// its earlier slices changed, then the code its specs name — plus, for each of the item's specs,
/// the commits on its named code since the spec itself last changed.
pub struct StartFiles {
    pub files: Vec<String>,
    pub changed: Vec<SpecChanges>,
}

/// Builds [`StartFiles`] for `item`, reading its specs from `root` (the lane's worktree, already
/// checked out to the item's branch) and its history from `base_ref` (`origin/<baseBranch>`) with
/// `git_bin`.
pub async fn build(git_bin: &Path, root: &Path, base_ref: &str, item: &Item) -> StartFiles {
    let spec_files = item_spec_files(&item.spec_refs, root);
    let mut files = spec_files.clone();

    for f in slice_changed_files(git_bin, root, base_ref, &item.id).await {
        if files.len() >= MAX_FILES {
            break;
        }
        if !files.contains(&f) {
            files.push(f);
        }
    }

    let mut changed = Vec::new();
    for spec in &spec_files {
        let content = std::fs::read_to_string(root.join(spec)).unwrap_or_default();
        let named = named_code_paths(spec, &content, root);
        for f in &named {
            if files.len() >= MAX_FILES {
                break;
            }
            if !files.contains(f) {
                files.push(f.clone());
            }
        }
        let commits = changed_since_spec(git_bin, root, base_ref, spec, &named).await;
        if !commits.is_empty() {
            changed.push(SpecChanges {
                spec: spec.clone(),
                commits,
            });
        }
    }

    StartFiles { files, changed }
}

/// The "Where to start" prompt section's text (section 6.2), empty when there's nothing to list.
pub fn render(start: &StartFiles) -> String {
    if start.files.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\n## Where to start\n\nRead the ones you need together, several Read calls in one \
         message, before searching.\n\n",
    );
    for f in &start.files {
        out.push_str(&format!("- {f}\n"));
    }
    if !start.changed.is_empty() {
        out.push_str("\n### Changed since its specs last did\n\n");
        for sc in &start.changed {
            out.push_str(&format!("{}:\n", sc.spec));
            for c in &sc.commits {
                let short = &c.sha[..c.sha.len().min(7)];
                out.push_str(&format!("- {short} {} ({})\n", c.subject, c.date));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::backlog::ItemStatus;

    use super::*;

    fn item(id: &str, spec_refs: &[&str]) -> Item {
        Item {
            id: id.to_string(),
            title: "Title".to_string(),
            status: ItemStatus::Building,
            rank: 0,
            do_next: false,
            depends_on: Vec::new(),
            spec_refs: spec_refs.iter().map(|s| s.to_string()).collect(),
            slice: 1,
            remaining: None,
            pr_url: None,
            branch: None,
            kind: None,
            milestone: None,
        }
    }

    /// A fake git binary that prints `fixture` for the first call whose arguments contain `marker`,
    /// in order; a call matching none of them prints nothing (`git_bin::command` isn't used here,
    /// matching `release_notes::gather`'s own git-binary-as-a-parameter tests).
    fn write_branching_fake_binary(path: &Path, cases: &[(&str, &Path)]) {
        if cfg!(windows) {
            let mut script = String::from("@echo off\r\n");
            for (marker, fixture) in cases {
                script.push_str(&format!(
                    "echo %* | findstr /C:\"{marker}\" >nul && type \"{}\" && exit /b 0\r\n",
                    fixture.display()
                ));
            }
            std::fs::write(path, script).unwrap();
        } else {
            let mut script = String::from("#!/bin/sh\n");
            for (marker, fixture) in cases {
                // The marker is quoted inside the glob so a space in it (e.g. "log -1") stays one
                // pattern token instead of becoming a syntax error (two words with no `|` between).
                script.push_str(&format!(
                    "case \"$*\" in *\"{marker}\"*) cat \"{}\"; exit 0;; esac\n",
                    fixture.display()
                ));
            }
            std::fs::write(path, script).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
    }

    fn fake_git_path(dir: &Path) -> PathBuf {
        dir.join(if cfg!(windows) { "fake_git.cmd" } else { "fake_git.sh" })
    }

    // item_spec_files

    #[test]
    fn item_spec_files_keeps_only_existing_markdown_refs_once_each() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs/specs")).unwrap();
        std::fs::write(dir.path().join("docs/specs/a.md"), "# A").unwrap();
        let refs = vec![
            "docs/specs/a.md".to_string(),
            "docs/specs/a.md".to_string(),
            "docs/specs/missing.md".to_string(),
            "src/a.rs".to_string(),
        ];
        assert_eq!(item_spec_files(&refs, dir.path()), vec!["docs/specs/a.md".to_string()]);
    }

    // named_code_paths

    #[test]
    fn named_code_paths_resolves_from_the_repo_root_or_the_specs_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs/specs")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "").unwrap();
        std::fs::write(dir.path().join("docs/specs/local.rs"), "").unwrap();
        let content = "See `src/a.rs` and `local.rs`, but not `--max-turns` or `cargo test` or \
                        `session.save()` or `docs/specs/missing.rs`.";
        let found = named_code_paths("docs/specs/x.md", content, dir.path());
        assert_eq!(found, vec!["src/a.rs".to_string(), "docs/specs/local.rs".to_string()]);
    }

    #[test]
    fn named_code_paths_is_empty_when_a_spec_names_more_than_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut content = String::new();
        for i in 0..(MAX_FILES + 1) {
            let name = format!("f{i}.rs");
            std::fs::write(dir.path().join(&name), "").unwrap();
            content.push_str(&format!("`{name}` "));
        }
        assert!(named_code_paths("x.md", &content, dir.path()).is_empty());
    }

    // slice_changed_files

    #[tokio::test]
    async fn slice_changed_files_lists_each_matching_commits_files_once() {
        let dir = tempfile::tempdir().unwrap();
        let shas = dir.path().join("shas.txt");
        std::fs::write(&shas, "aaa\nbbb\n").unwrap();
        let files = dir.path().join("files.txt");
        std::fs::write(&files, "src/a.rs\nsrc/b.rs\nsrc/a.rs\n").unwrap();
        let fake_git = fake_git_path(dir.path());
        write_branching_fake_binary(&fake_git, &[("diff-tree", &files), ("log", &shas)]);

        let found = slice_changed_files(&fake_git, dir.path(), "origin/main", "7").await;
        assert_eq!(found, vec!["src/a.rs".to_string(), "src/b.rs".to_string()]);
    }

    #[tokio::test]
    async fn slice_changed_files_is_empty_when_git_log_fails() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        let found = slice_changed_files(&missing, dir.path(), "origin/main", "7").await;
        assert!(found.is_empty());
    }

    // changed_since_spec

    #[tokio::test]
    async fn changed_since_spec_reads_the_specs_own_last_changed_date_then_the_codes_log() {
        let dir = tempfile::tempdir().unwrap();
        let date = dir.path().join("date.txt");
        std::fs::write(&date, "2026-10-01T00:00:00Z\n").unwrap();
        let log = dir.path().join("log.txt");
        std::fs::write(&log, "abc123\t2026-10-20T00:00:00Z\tA Dev\tDid a thing\n").unwrap();
        let fake_git = fake_git_path(dir.path());
        write_branching_fake_binary(&fake_git, &[("log -1", &date), ("log", &log)]);

        let commits = changed_since_spec(
            &fake_git,
            dir.path(),
            "origin/main",
            "docs/specs/x.md",
            &["src/a.rs".to_string()],
        )
        .await;
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].sha, "abc123");
        assert_eq!(commits[0].subject, "Did a thing");
    }

    #[tokio::test]
    async fn changed_since_spec_is_empty_with_no_code_paths() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        let commits = changed_since_spec(&missing, dir.path(), "origin/main", "x.md", &[]).await;
        assert!(commits.is_empty());
    }

    // build / render

    #[tokio::test]
    async fn build_combines_spec_files_slice_changes_and_named_code_then_renders_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs/specs")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("docs/specs/x.md"), "Names `src/a.rs`.").unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/b.rs"), "").unwrap();

        let shas = dir.path().join("shas.txt");
        std::fs::write(&shas, "ccc\n").unwrap();
        let slice_files = dir.path().join("slice_files.txt");
        std::fs::write(&slice_files, "src/b.rs\n").unwrap();
        let date = dir.path().join("date.txt");
        std::fs::write(&date, "2026-10-01T00:00:00Z\n").unwrap();
        let since_log = dir.path().join("since_log.txt");
        std::fs::write(&since_log, "ddd\t2026-10-20T00:00:00Z\tA Dev\tChanged a.rs\n").unwrap();

        // Markers are checked in order, so the `--since` call (which also contains "log") is
        // routed before the plain `log --format=%H ...` one that lists matching commits.
        let fake_git = fake_git_path(dir.path());
        write_branching_fake_binary(
            &fake_git,
            &[
                ("diff-tree", &slice_files),
                ("log -1", &date),
                ("--since", &since_log),
                ("log", &shas),
            ],
        );

        let it = item("7", &["docs/specs/x.md"]);
        let start = build(&fake_git, dir.path(), "origin/main", &it).await;

        assert_eq!(
            start.files,
            vec![
                "docs/specs/x.md".to_string(),
                "src/b.rs".to_string(),
                "src/a.rs".to_string()
            ]
        );
        assert_eq!(start.changed.len(), 1);
        assert_eq!(start.changed[0].spec, "docs/specs/x.md");
        assert_eq!(start.changed[0].commits[0].sha, "ddd");

        let rendered = render(&start);
        assert!(rendered.contains("## Where to start"));
        assert!(rendered.contains("- docs/specs/x.md"));
        assert!(rendered.contains("- src/b.rs"));
        assert!(rendered.contains("- src/a.rs"));
        assert!(rendered.contains("### Changed since its specs last did"));
        assert!(rendered.contains("ddd Changed a.rs"));
    }

    #[test]
    fn render_is_empty_with_no_files() {
        assert_eq!(
            render(&StartFiles {
                files: Vec::new(),
                changed: Vec::new()
            }),
            ""
        );
    }
}
