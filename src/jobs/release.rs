//! A `release_notes` task (`releaseJob` in Retaskable.com's `agent/src/releaseNotes.ts`, jobs-on-server.md section 6.8):
//! the note's Markdown and what it counted. The store writes the note; this computer only gathers what merged into
//! the base branch in the range, groups it by the backlog item that owns each change ([`crate::release_notes`]),
//! and writes it: no session when nothing merged or only chores did, otherwise one read-only writer session. A
//! range with too much in it is a refusal in words.
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;

use anyhow::Result;
use chrono::DateTime;
use serde::Serialize;

use super::{JobSpec, Outcome, run_failed};
use crate::backlog::{Draft, Item};
use crate::engines::{SessionResult, SessionSpec};
use crate::release_notes::MergedPr;
use crate::release_notes::{
    Counts, GroupInput, Range, Section, Writer, fill_draft_files, gather_commits, gather_merged_prs, group,
    read_base_files, write_note,
};

/// `prompts/release-notes.md`, for [`Writer::template`]: the TypeScript agent's, word for word.
pub const TEMPLATE: &str = include_str!("../../prompts/release-notes.md");
/// The refusal when there is no checkout to look in.
pub const NO_CHECKOUT: &str = "This computer has no checkout of the project to look in.";
/// The refusal when the task names no range.
pub const NO_RANGE: &str = "The release notes task has no range to cover.";

/// What the task names: the range the note covers (`from` and `to` ISO instants) and when it was asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseArgs {
    pub from: String,
    pub to: String,
    pub generated_at: String,
}

impl ReleaseArgs {
    /// `None` when `from` or `to` is missing: the TypeScript agent turns that into the text "undefined" and fails
    /// later inside `gh`; this says so at once.
    pub fn from_job(job: &JobSpec) -> Option<ReleaseArgs> {
        let text = |k: &str| {
            job.arg_str(k)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Some(ReleaseArgs {
            from: text("from")?,
            to: text("to")?,
            generated_at: text("generatedAt").unwrap_or_default(),
        })
    }

    pub fn range(&self) -> Range {
        Range {
            from: self.from.clone(),
            to: self.to.clone(),
            generated_at: self.generated_at.clone(),
        }
    }
}

/// What the grouping found in the range.
#[derive(Debug, Clone)]
pub struct Gathered {
    pub sections: Vec<Section>,
    /// How many PRs merged in the range (`prs.length`).
    pub total_prs: usize,
}

/// Why a range could not be gathered.
#[derive(Debug, Clone, PartialEq)]
pub enum GatherError {
    /// A range with too much in it (the TypeScript `HttpError` 400): a refusal, with its words.
    TooMany(String),
    /// The code host or git failed.
    Failed(String),
}

/// Gathers and groups what merged in a range (`gather`).
pub trait ReleaseGather {
    fn gather(&self, range: &Range) -> impl Future<Output = Result<Gathered, GatherError>> + Send;
}

/// The refusal for a range with more than `max` merged PRs, in the TypeScript agent's words.
pub fn too_many(max: u32) -> String {
    format!("More than {max} PRs merged in this range: pick a shorter one.")
}

/// [`ReleaseGather`] over the project's checkout, `gh` and `git`, with the store's items and drafts.
pub struct RepoGather<'a> {
    pub gh_bin: &'a Path,
    pub git_bin: &'a Path,
    /// The project's checkout (`repoPath`).
    pub repo_path: &'a Path,
    pub github_repo: &'a str,
    pub base_branch: &'a str,
    /// `releaseMaxPrs` (default [`crate::release_notes::DEFAULT_MAX_PRS`]).
    pub max_prs: u32,
    pub items: &'a [Item],
    pub drafts: &'a [Draft],
}

fn instant(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis())
}

/// The PRs merged within [from, to] (`Date.parse(mergedAt) >= from && <= to`): the search is by day, so the
/// edges are ours to cut.
fn within(prs: Vec<MergedPr>, range: &Range) -> Vec<MergedPr> {
    let (Some(from), Some(to)) = (instant(&range.from), instant(&range.to)) else {
        return prs;
    };
    prs.into_iter()
        .filter(|p| instant(&p.merged_at).is_some_and(|m| m >= from && m <= to))
        .collect()
}

impl ReleaseGather for RepoGather<'_> {
    async fn gather(&self, range: &Range) -> Result<Gathered, GatherError> {
        let fail = |e: anyhow::Error| GatherError::Failed(format!("{e:#}"));
        // `fetch(cfg)`: the base branch as the host has it now. Best effort, as the work is on `origin/<base>`.
        let dir = self.repo_path.to_path_buf();
        let _ =
            tokio::task::spawn_blocking(move || crate::git_bin::command(&dir).args(["fetch", "origin"]).output()).await;
        let base_ref = format!("origin/{}", self.base_branch);
        let all = gather_merged_prs(
            self.gh_bin,
            self.repo_path,
            self.github_repo,
            self.base_branch,
            &range.from,
            &range.to,
            self.max_prs + 1,
        )
        .await
        .map_err(fail)?;
        if all.len() as u32 > self.max_prs {
            return Err(GatherError::TooMany(too_many(self.max_prs)));
        }
        let mut prs = within(all, range);
        fill_draft_files(self.gh_bin, self.repo_path, self.github_repo, &mut prs, self.drafts)
            .await
            .map_err(fail)?;
        let commits = gather_commits(self.git_bin, self.repo_path, &base_ref, &range.from, &range.to)
            .await
            .map_err(fail)?;
        // The specs every item names and every spec a draft's change touches, from the base branch, for titles.
        let mut paths: Vec<String> = Vec::new();
        for p in self
            .items
            .iter()
            .flat_map(|i| i.spec_refs.iter())
            .chain(prs.iter().flat_map(|p| p.files.iter()))
        {
            if p.to_lowercase().ends_with(".md") && !paths.contains(p) {
                paths.push(p.clone());
            }
        }
        let files: HashMap<String, Option<String>> =
            read_base_files(self.git_bin, self.repo_path, &base_ref, &paths).await;
        let sections = group(&GroupInput {
            prs: &prs,
            commits: &commits,
            items: self.items,
            drafts: self.drafts,
            files: &files,
            github_repo: self.github_repo,
            base_branch: self.base_branch,
        });
        Ok(Gathered {
            sections,
            total_prs: prs.len(),
        })
    }
}

/// What a `release_notes` task answers (`Written & { counts }`): the note's Markdown, the model that wrote it
/// (empty when none ran), what it cost, why it is the plain list when it is, and what it counted.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseResult {
    pub markdown: String,
    pub model: String,
    pub cost_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub counts: CountsOut,
}

/// `counts`: the PRs, items and chores in the range.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CountsOut {
    pub prs: usize,
    pub items: usize,
    pub chores: usize,
}

impl From<Counts> for CountsOut {
    fn from(c: Counts) -> Self {
        CountsOut {
            prs: c.prs,
            items: c.items,
            chores: c.chores,
        }
    }
}

/// The `release_notes` job. `run` is the caller's [`crate::engines::run_session`]; `writer` its session's role.
pub async fn release_job<G, F, Fut>(
    job: &JobSpec,
    has_checkout: bool,
    gatherer: &G,
    writer: &Writer<'_>,
    run: F,
) -> Outcome<ReleaseResult>
where
    G: ReleaseGather,
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    if !has_checkout {
        return Outcome::refused(NO_CHECKOUT);
    }
    let Some(args) = ReleaseArgs::from_job(job) else {
        return Outcome::refused(NO_RANGE);
    };
    let range = args.range();
    let gathered = match gatherer.gather(&range).await {
        Ok(g) => g,
        Err(GatherError::TooMany(why)) => return Outcome::refused(why),
        Err(GatherError::Failed(e)) => return run_failed(&job.kind, &e),
    };
    let w = write_note(&gathered.sections, gathered.total_prs, &range, writer, run).await;
    Outcome::done(ReleaseResult {
        markdown: w.markdown,
        model: w.model,
        cost_usd: w.cost_usd,
        error: w.error,
        counts: w.counts.into(),
    })
}

#[cfg(test)]
mod tests;
