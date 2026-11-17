//! A `deprecations` task, the computer's half (`readDeprecations`/`deprecationsJob`, deprecations.ts and jobTasks.ts;
//! jobs-on-server.md section 6.6): reads the specs on `origin/<default branch>`, parses the `Deprecated:` lines and
//! answers the list and the problems. It adds no item and keeps no record: the day, the removal item, the reminders
//! and the board's list are the store's. [`crate::deprecations`] parses the lines.
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::Outcome;
use crate::deprecations::{Deprecation, DeprecationProblem, read_deprecations};
use crate::spec_items::SPECS_DIR;

/// What a `deprecations` task answers (`DeprecationsRaw`): the specs it read and the lines it could not.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeprecationsResult {
    pub deprecations: Vec<Deprecation>,
    pub problems: Vec<DeprecationProblem>,
}

/// The refusal while the default branch's specs can't be read.
pub const CANT_READ: &str = "The default branch's specs can't be read yet.";

/// The default branch's top-level specs (path, text), or `None` when they can't be read.
pub trait SpecSource {
    fn read(&self) -> Option<Vec<(String, String)>>;
}

/// The specs on `origin/<default_branch>` in the project's checkout. Blocking: run it off the async threads.
pub struct GitSpecs {
    pub repo_path: PathBuf,
    /// The repository's default branch, when known (`defaultBranch()`).
    pub default_branch: Option<String>,
}

impl SpecSource for GitSpecs {
    fn read(&self) -> Option<Vec<(String, String)>> {
        let branch = self.default_branch.as_deref().filter(|b| !b.is_empty())?;
        specs_at(&self.repo_path, &format!("origin/{branch}"))
    }
}

/// `specsAt`: the top-level `docs/specs/*.md` on `at` (a split spec's Items and Deprecated lines stay in its
/// index, agent.md section 6.10). `None` when the ref or the folder is missing or any file can't be shown.
pub fn specs_at(repo_path: &Path, at: &str) -> Option<Vec<(String, String)>> {
    let git = |args: &[&str]| crate::lanes::git_output_in_worktree(repo_path, args).ok();
    git(&["rev-parse", &format!("{at}:{SPECS_DIR}")])?;
    let listed = git(&["ls-tree", "--name-only", at, "--", &format!("{SPECS_DIR}/")])?;
    let mut files = Vec::new();
    for path in listed.lines().map(str::trim).filter(|l| l.ends_with(".md")) {
        files.push((path.to_string(), git(&["show", &format!("{at}:{path}")])?));
    }
    Some(files)
}

/// The `deprecations` job: what the default branch's specs say, read and parsed; the store decides what is due.
pub fn deprecations_job(specs: &dyn SpecSource) -> Outcome<DeprecationsResult> {
    let Some(files) = specs.read() else {
        return Outcome::refused(CANT_READ);
    };
    let (deprecations, problems) = read_deprecations(&files);
    Outcome::done(DeprecationsResult { deprecations, problems })
}

#[cfg(test)]
mod tests;
