//! Open PR to main (`teamBranchActions.openMainPr` and `teamBranch.draftMainPr` in the TS agent): the refusals that
//! come before it, the pull request's title and body drafted from the slices merged into the base branch, and the
//! `gh` calls that read those and open it. GitHub's words only (`pull request`, `#12`); the pure parts are tested
//! without `gh`.
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{MainPr, TeamBranch};
use crate::main_sync::sync_branch_name;

/// A pull request merged into a branch, as `gh pr list --state merged` says it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Merged {
    pub number: u64,
    pub title: String,
    #[serde(rename = "headRefName")]
    pub branch: String,
    /// When it merged (ISO 8601), or empty.
    #[serde(default)]
    pub merged_at: Option<String>,
}

/// What Open PR to main would send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    pub body: String,
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// The pull request's title and body: the slice pull requests merged into the base branch since its last pull request
/// into main merged (`since`), oldest first, without the main syncs, which only bring main's own commits (`mainPrDraft`).
pub fn main_pr_draft(base: &str, main: &str, merged: &[Merged], since: Option<&str>) -> Draft {
    let at = |p: &Merged| p.merged_at.clone().unwrap_or_default();
    let sync = sync_branch_name("");
    let mut slices: Vec<&Merged> = merged
        .iter()
        .filter(|p| since.is_none_or(|s| at(p).as_str() > s) && !p.branch.starts_with(&sync))
        .collect();
    slices.sort_by_key(|p| at(p));
    let since_note = if since.is_some() {
        format!(" since its last pull request into `{main}`")
    } else {
        String::new()
    };
    let mut lines = vec![
        format!("Everything merged into `{base}`{since_note}, for the team to review."),
        String::new(),
        format!(
            "When it's ready, merge it from Retaskable: it squashes this into `{main}` as one change, then starts `{base}` again from `{main}`."
        ),
        String::new(),
        "### Slices".to_string(),
    ];
    if slices.is_empty() {
        lines.push("- (none found: only main syncs, or work merged without a pull request)".into());
    } else {
        lines.extend(slices.iter().map(|p| format!("- {} (#{})", p.title, p.number)));
    }
    Draft {
        title: format!("{base}: {}", plural(slices.len(), "slice")),
        body: lines.join("\n"),
    }
}

/// The ref a pull request goes by in a sentence (`changeRef`).
fn pr_ref(number: u64) -> String {
    format!("PR #{number}")
}

fn already_open(pr: &MainPr, base: &str, main: &str) -> String {
    format!("{} from {base} into {main} is already open.", pr_ref(pr.number))
}

/// Why Open PR to main says no before reading `gh` again, in the TS agent's order, or none.
pub fn open_refusal(tb: &TeamBranch) -> Option<String> {
    let (base, main) = (&tb.base, &tb.main);
    if let Some(pr) = &tb.main_pr {
        return Some(already_open(pr, base, main));
    }
    if tb.behind > 0 && tb.sync_open {
        return Some(format!("Sync {main} first: its sync is in flight."));
    }
    if tb.ahead == 0 {
        return Some(format!("{base} has nothing {main} doesn't."));
    }
    None
}

/// The refusal for a pull request found open when `gh` is asked again, just before opening one.
pub fn found_open(pr: &MainPr, tb: &TeamBranch) -> String {
    already_open(pr, &tb.base, &tb.main)
}

/// `gh pr list` of the pull requests merged into `base` (from `head`, when named), newest first.
fn merged_into(gh: &Path, dir: &Path, repo: &str, base: &str, head: Option<&str>, limit: usize) -> Result<Vec<Merged>> {
    let limit = limit.to_string();
    let mut args = vec!["pr", "list", "--repo", repo, "--base", base];
    if let Some(head) = head {
        args.extend(["--head", head]);
    }
    args.extend([
        "--state",
        "merged",
        "--limit",
        &limit,
        "--json",
        "number,title,headRefName,mergedAt",
    ]);
    crate::host_call::guard("gh", dir, &args)?;
    let mut list = Command::new(gh);
    list.current_dir(dir).args(&args);
    let listed = crate::implementer::publish::run(list, None, "gh pr list")?;
    serde_json::from_str(listed.trim()).context("reading gh pr list's answer")
}

/// Reads what Open PR to main would send (`draftMainPr`): the last pull request from the base into main sets `since`.
pub fn draft_main_pr(gh: &Path, dir: &Path, repo: &str, base: &str, main: &str) -> Result<Draft> {
    let to_main = merged_into(gh, dir, repo, main, Some(base), 20)?;
    let since = to_main
        .iter()
        .filter_map(|p| p.merged_at.clone())
        .filter(|at| !at.is_empty())
        .max();
    let merged = merged_into(gh, dir, repo, base, None, 200)?;
    Ok(main_pr_draft(base, main, &merged, since.as_deref()))
}

/// `gh pr create` from `base` into `main`, the body on stdin; its URL (gh's last line).
pub fn create(gh: &Path, dir: &Path, repo: &str, base: &str, main: &str, draft: &Draft) -> Result<String> {
    // The title and body are free text, so only the branches are checked.
    crate::host_call::guard(
        "gh",
        dir,
        &["pr", "create", "--repo", repo, "--base", main, "--head", base],
    )?;
    let mut create = Command::new(gh);
    create.current_dir(dir).args([
        "pr",
        "create",
        "--repo",
        repo,
        "--base",
        main,
        "--head",
        base,
        "--title",
        &draft.title,
        "--body-file",
        "-",
    ]);
    let made = crate::implementer::publish::run(create, Some(&draft.body), "gh pr create")?;
    made.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_string)
        .context("gh pr create printed no URL")
}

#[cfg(test)]
mod tests;
