//! A read model of a project's team branch (its base branch, when that isn't the repo's default branch; the TS
//! agent's `teamBranch.ts`, `readTeamBranch` in `orchestratorSync.ts` and `shipRefusal` in `shared/mergePr.ts`): how far
//! the base and main are apart, the open pull request from the base into main, whether a main sync is open and
//! whether a slice is landing. The Open PR to main, merge control and sync kinds read it; nothing here is wired into
//! the loop yet. Merge pattern only: the integration pattern's rebase (and `shipRefusal`'s rebase-conflict line) isn't
//! ported, and neither is a local-only project.
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::backlog::{Item, ItemStatus};
use crate::git_bin::command;
use crate::merge_update::run as git;

/// Words `needTeamBranch` refuses with (`teamBranchActions.ts`).
pub const NO_TEAM_BRANCH: &str =
    "This project works on the repo's default branch, so there's no dev branch to send to it.";

/// The default branch a team branch goes to, or the refusal. A project has a team branch when its base branch isn't
/// the repo's default branch; with the default branch unknown there is none, as in the TS agent.
pub fn need_team_branch<'a>(base: &str, default_branch: Option<&'a str>) -> Result<&'a str, &'static str> {
    default_branch.filter(|main| *main != base).ok_or(NO_TEAM_BRANCH)
}

/// The open pull request from the base branch into main.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MainPr {
    pub number: u64,
    pub url: String,
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN`, as `gh` says it.
    #[serde(default)]
    pub mergeable: String,
    /// The head commit the pull request was read at, which a merge is matched to.
    #[serde(default, rename = "headRefOid")]
    pub head: String,
}

/// What a fetch last showed of the team branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamBranch {
    pub base: String,
    pub main: String,
    /// Commits the base has that main lacks.
    pub ahead: u32,
    /// Commits main has that the base lacks.
    pub behind: u32,
    pub main_pr: Option<MainPr>,
    /// A `sync_main` item is open (not on main, in production or skipped).
    pub sync_open: bool,
    /// An item is merging: a slice is landing on the base.
    pub merging: bool,
}

/// `git rev-list --left-right --count origin/<main>...origin/<base>`'s answer, main's side first: `(ahead, behind)`.
pub fn parse_ahead_behind(out: &str) -> Option<(u32, u32)> {
    let mut parts = out.split_whitespace();
    let behind = parts.next()?.parse().ok()?;
    let ahead = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((ahead, behind))
}

/// How far `origin/<base>` is ahead of and behind `origin/<main>`, as of the last fetch.
pub fn ahead_behind(repo: &Path, base: &str, main: &str) -> Result<(u32, u32)> {
    let range = format!("origin/{main}...origin/{base}");
    let out = git(&command, repo, &["rev-list", "--left-right", "--count", &range])?;
    parse_ahead_behind(&out).with_context(|| format!("reading git's count for {range}: {out:?}"))
}

/// Whether a main sync is open among the store's items, and whether any item is merging.
pub fn held_in(items: &[Item]) -> (bool, bool) {
    let sync = items.iter().any(|i| {
        i.kind.as_deref() == Some("sync_main")
            && !matches!(
                i.status,
                ItemStatus::OnMain | ItemStatus::InProduction | ItemStatus::Skipped
            )
    });
    (sync, items.iter().any(|i| i.status == ItemStatus::Merging))
}

/// `gh pr list`'s arguments for the open pull request from `head` into `base`.
fn list_args<'a>(repo: &'a str, base: &'a str, head: &'a str) -> [&'a str; 14] {
    [
        "pr",
        "list",
        "--repo",
        repo,
        "--base",
        base,
        "--head",
        head,
        "--state",
        "open",
        "--limit",
        "1",
        "--json",
        "number,url,mergeable,headRefOid",
    ]
}

/// The open pull request from the base branch into main, or none.
pub fn open_main_pr(gh: &Path, dir: &Path, github_repo: &str, base: &str, main: &str) -> Result<Option<MainPr>> {
    let args = list_args(github_repo, main, base);
    crate::host_call::guard("gh", dir, &args)?;
    let mut list = Command::new(gh);
    list.current_dir(dir).args(args);
    let listed = crate::implementer::publish::run(list, None, "gh pr list")?;
    let open: Vec<MainPr> = serde_json::from_str(listed.trim()).context("reading gh pr list's answer")?;
    Ok(open.into_iter().next())
}

/// Reads the team branch after a fetch. `items` are the store's. An error (git or `gh` couldn't answer) is the
/// caller's to keep the last read through; nothing is guessed.
pub fn read(gh: &Path, repo: &Path, github_repo: &str, base: &str, main: &str, items: &[Item]) -> Result<TeamBranch> {
    let (ahead, behind) = ahead_behind(repo, base, main)?;
    let main_pr = open_main_pr(gh, repo, github_repo, base, main)?;
    let (sync_open, merging) = held_in(items);
    Ok(TeamBranch {
        base: base.into(),
        main: main.into(),
        ahead,
        behind,
        main_pr,
        sync_open,
        merging,
    })
}

/// Why the merge control into main is greyed out, or none when it can be pressed (`shipRefusal`; github-workflow.md
/// section 4.2). Only the merge pattern: the integration pattern's "needs a rebase onto main first" line is left out
/// with that pattern.
pub fn ship_refusal(tb: &TeamBranch) -> Option<String> {
    let (base, main) = (&tb.base, &tb.main);
    let Some(pr) = &tb.main_pr else {
        return Some(format!("No pull request from {base} into {main} is open."));
    };
    if tb.sync_open {
        return Some(format!("Wait for {main} to finish coming into {base} first."));
    }
    if pr.mergeable == "CONFLICTING" {
        return Some(format!("It conflicts with {main}: bring {main} into {base} first."));
    }
    if tb.merging {
        return Some(format!("A slice is going into {base}; try again in a moment."));
    }
    None
}

/// GitHub's own words from a failed `gh` call, without gh's GraphQL prefix, at most 300 characters (`ghReason`).
pub fn gh_reason(err: &anyhow::Error) -> String {
    let text = format!("{err:#}");
    let after = text
        .rfind("GraphQL: ")
        .map_or(text.as_str(), |at| &text[at + "GraphQL: ".len()..]);
    after.trim().chars().take(300).collect()
}

pub mod main_pr;

#[cfg(test)]
mod tests;
