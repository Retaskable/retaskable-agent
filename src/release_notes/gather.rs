//! Runs `gh`/`git` to collect what merged into the base branch in a range: `gh pr list` for merged
//! PRs, `git log --first-parent` for commits, and `git show` for the content of named spec files.
use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::process::Command;

use super::group::clip;
use super::model::{Commit, MergedPr};

/// `releaseMaxPrs`'s default (shared/tuning.ts): more than this many merged PRs in one range is
/// refused rather than guessed at.
pub const DEFAULT_MAX_PRS: u32 = 200;

/// Reads each named spec file at the base branch (`git show <ref>:<path>`), for titling its section.
/// A file that can't be read (moved, deleted, bad ref) maps to `None` rather than failing the whole
/// gather — [`super::group::spec_title`] falls back to the file name.
pub async fn read_base_files(
    git_bin: &Path,
    cwd: &Path,
    base_ref: &str,
    paths: &[String],
) -> HashMap<String, Option<String>> {
    let mut out = HashMap::new();
    for path in paths {
        let output = Command::new(git_bin)
            .args(["show", &format!("{base_ref}:{path}")])
            .current_dir(cwd)
            .output()
            .await;
        let content = match output {
            Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).into_owned()),
            _ => None,
        };
        out.insert(path.clone(), content);
    }
    out
}

/// `second()` (agent/src/codeHost/gh.ts): `gh`'s `--search merged:` range drops a trailing
/// millisecond fraction before the `Z`.
fn trim_millis(t: &str) -> String {
    let Some(without_z) = t.strip_suffix('Z') else {
        return t.to_string();
    };
    let Some(dot) = without_z.rfind('.') else {
        return t.to_string();
    };
    let fraction = &without_z[dot + 1..];
    if fraction.is_empty() || !fraction.chars().all(|c| c.is_ascii_digit()) {
        return t.to_string();
    }
    format!("{}Z", &without_z[..dot])
}

/// `mergedPrArgs()` (agent/src/codeHost/gh.ts): the `gh pr list` arguments for the PRs merged into
/// `base_branch` between `from` and `to`.
pub fn merged_pr_args(repo: &str, base_branch: &str, from: &str, to: &str, limit: u32) -> Vec<String> {
    vec![
        "pr".to_string(),
        "list".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--base".to_string(),
        base_branch.to_string(),
        "--state".to_string(),
        "merged".to_string(),
        "--search".to_string(),
        format!("merged:{}..{}", trim_millis(from), trim_millis(to)),
        "--limit".to_string(),
        limit.to_string(),
        "--json".to_string(),
        "number,title,url,body,mergedAt,headRefName,mergeCommit".to_string(),
    ]
}

#[derive(Deserialize)]
struct RawMergeCommit {
    oid: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMergedPr {
    number: u64,
    title: String,
    url: String,
    #[serde(default)]
    body: String,
    merged_at: String,
    head_ref_name: String,
    #[serde(default)]
    merge_commit: Option<RawMergeCommit>,
}

impl From<RawMergedPr> for MergedPr {
    fn from(r: RawMergedPr) -> Self {
        MergedPr {
            number: r.number,
            title: r.title,
            url: r.url,
            body: r.body,
            merged_at: r.merged_at,
            head_ref_name: r.head_ref_name,
            merge_commit_oid: r.merge_commit.map(|m| m.oid),
        }
    }
}

/// Runs `gh pr list` for the PRs merged into `base_branch` between `from` and `to`, up to `limit`.
pub async fn gather_merged_prs(
    gh_bin: &Path,
    cwd: &Path,
    repo: &str,
    base_branch: &str,
    from: &str,
    to: &str,
    limit: u32,
) -> Result<Vec<MergedPr>> {
    let args = merged_pr_args(repo, base_branch, from, to, limit);
    let output = Command::new(gh_bin)
        .args(&args)
        .current_dir(cwd)
        .output()
        .await
        .context("running gh pr list")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!("gh pr list failed: {}", clip(&stderr, 300));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let raw: Vec<RawMergedPr> = if stdout.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(&stdout).context("parsing gh pr list output")?
    };
    Ok(raw.into_iter().map(MergedPr::from).collect())
}

/// `commitLogArgs()`-equivalent (agent/src/git.ts's merged-commit gathering): `git log` arguments for
/// the first-parent history of `base_ref` between `from` and `to`.
pub fn commit_log_args(base_ref: &str, from: &str, to: &str) -> Vec<String> {
    vec![
        "log".to_string(),
        "--first-parent".to_string(),
        "--format=%H%x09%cI%x09%an%x09%s".to_string(),
        format!("--since={from}"),
        format!("--until={to}"),
        base_ref.to_string(),
    ]
}

/// Runs `git log --first-parent` for `base_ref` between `from` and `to`. A failing `git log` (bad
/// ref, no repo) reads as no commits rather than an error — the PRs gathered separately still stand.
pub async fn gather_commits(git_bin: &Path, cwd: &Path, base_ref: &str, from: &str, to: &str) -> Result<Vec<Commit>> {
    let args = commit_log_args(base_ref, from, to);
    let output = Command::new(git_bin)
        .args(&args)
        .current_dir(cwd)
        .output()
        .await
        .context("running git log")?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .map(|line| {
            let mut parts = line.splitn(4, '\t');
            Commit {
                sha: parts.next().unwrap_or_default().to_string(),
                date: parts.next().unwrap_or_default().to_string(),
                author: parts.next().unwrap_or_default().to_string(),
                subject: parts.next().unwrap_or_default().to_string(),
            }
        })
        .collect())
}

/// The inputs [`gather`] needs, grouped so the function itself takes one argument instead of nine.
pub struct GatherArgs<'a> {
    pub gh_bin: &'a Path,
    pub git_bin: &'a Path,
    pub cwd: &'a Path,
    pub github_repo: &'a str,
    pub base_branch: &'a str,
    pub base_ref: &'a str,
    pub from: &'a str,
    pub to: &'a str,
    pub max_prs: u32,
}

/// Gathers the merged PRs and commits for a range, refusing a range with more than `max_prs` merged
/// PRs rather than guessing which to drop (asks for one more than `max_prs` to tell "exactly at the
/// limit" apart from "over it" in a single `gh` call).
pub async fn gather(args: GatherArgs<'_>) -> Result<(Vec<MergedPr>, Vec<Commit>)> {
    let prs = gather_merged_prs(
        args.gh_bin,
        args.cwd,
        args.github_repo,
        args.base_branch,
        args.from,
        args.to,
        args.max_prs + 1,
    )
    .await?;
    if prs.len() as u32 > args.max_prs {
        anyhow::bail!(
            "more than {} PRs merged in this range: pick a shorter one",
            args.max_prs
        );
    }
    let commits = gather_commits(args.git_bin, args.cwd, args.base_ref, args.from, args.to).await?;
    Ok((prs, commits))
}

#[cfg(test)]
mod tests {
    use crate::release_notes::test_support::write_fake_binary_printing;

    use super::*;

    // merged_pr_args / commit_log_args

    #[test]
    fn merged_pr_args_builds_the_documented_gh_search() {
        let args = merged_pr_args("x/y", "main", "2026-09-23T00:00:00.000Z", "2026-09-30T00:00:00.000Z", 5);
        assert_eq!(args[0], "pr");
        assert_eq!(args[1], "list");
        let search_idx = args.iter().position(|a| a == "--search").unwrap();
        assert_eq!(
            args[search_idx + 1],
            "merged:2026-09-23T00:00:00Z..2026-09-30T00:00:00Z"
        );
    }

    #[test]
    fn commit_log_args_uses_first_parent_and_the_tab_separated_format() {
        let args = commit_log_args("origin/main", "2026-09-23T00:00:00Z", "2026-09-30T00:00:00Z");
        assert!(args.contains(&"--first-parent".to_string()));
        assert!(args.iter().any(|a| a == "--format=%H%x09%cI%x09%an%x09%s"));
        assert_eq!(args.last().unwrap(), "origin/main");
    }

    // gather_merged_prs / gather_commits (a fake `gh`/`git` binary that ignores its arguments and
    // just prints a fixture file, like session.rs's own fake-session tests do for `claude`)

    #[tokio::test]
    async fn gather_merged_prs_parses_ghs_json_output() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("prs.json");
        std::fs::write(
            &out,
            r#"[{"number":1,"title":"Add charts","url":"https://github.com/x/y/pull/1","body":"body","mergedAt":"2026-09-24T00:00:00Z","headRefName":"retaskable/1-charts","mergeCommit":{"oid":"abc123"}}]"#,
        )
        .unwrap();
        let fake_gh = dir
            .path()
            .join(if cfg!(windows) { "fake_gh.cmd" } else { "fake_gh.sh" });
        write_fake_binary_printing(&fake_gh, &out);
        let prs = gather_merged_prs(
            &fake_gh,
            dir.path(),
            "x/y",
            "main",
            "2026-09-23T00:00:00Z",
            "2026-09-30T00:00:00Z",
            10,
        )
        .await
        .unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 1);
        assert_eq!(prs[0].merge_commit_oid, Some("abc123".to_string()));
    }

    #[tokio::test]
    async fn gather_commits_parses_gits_tab_separated_output() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("log.txt");
        std::fs::write(&out, "abc123\t2026-09-24T00:00:00Z\tA Dev\tTidy things up\n").unwrap();
        let fake_git = dir
            .path()
            .join(if cfg!(windows) { "fake_git.cmd" } else { "fake_git.sh" });
        write_fake_binary_printing(&fake_git, &out);
        let commits = gather_commits(
            &fake_git,
            dir.path(),
            "origin/main",
            "2026-09-23T00:00:00Z",
            "2026-09-30T00:00:00Z",
        )
        .await
        .unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].sha, "abc123");
        assert_eq!(commits[0].subject, "Tidy things up");
    }
}
