//! Catching an integration branch up with main by a rebase (github-workflow.md section 4.2; `agent/src/rebaseMain.ts`), in
//! place of team-branch.md's main sync pull request. Whether it is due, and when, is the store's (`main` job `want: "rebase"`,
//! pools-on-server.md section 17); a person's Sync now asks for it directly. This is the git: it rebases `origin/<base>` onto
//! `origin/<main>` in a worktree of its own and pushes with `--force-with-lease` on the head it read, so work that lands in
//! between is never thrown away. A conflict pushes nothing: the strip says which files, and Retaskable doesn't resolve it.
use std::path::Path;

use anyhow::{Context, Result};

use crate::git_bin::command;
use crate::merge_update::run as git;
use crate::text::truncate;

/// The worktree's folder under the project's `worktrees_dir`, and the local branch the rebase is made on.
pub const WORKTREE: &str = "rebase-main";
const TMP: &str = "retaskable/rebase-onto-main";

/// Whether the project catches its base branch up by a rebase: the integration-branch pattern, once picked and saved
/// (`usesRebase`). A project saved without `github` keeps the main sync until someone picks a pattern.
pub fn uses_rebase(project: &crate::config::ProjectConfig) -> bool {
    project
        .github
        .as_ref()
        .and_then(|g| g.get("pattern"))
        .and_then(serde_json::Value::as_str)
        == Some("integration")
}

/// What a rebase came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebased {
    /// Pushed, or the base branch already had all of main.
    Done,
    /// The rebase conflicts in these files; nothing was pushed.
    Conflict(Vec<String>),
    /// It could not be pushed: why.
    Failed(String),
}

/// The base branch's and main's commits on origin, as the last fetch left them; `None` for one that is not there.
pub fn heads(repo: &Path, base: &str, main: &str) -> (Option<String>, Option<String>) {
    (
        crate::main_look::head_of(repo, base),
        crate::main_look::head_of(repo, main),
    )
}

/// Rebases `origin/<base>` (at `base_sha`) onto `main_sha` in `worktree` and pushes it back with a lease on `base_sha`. The
/// worktree ends detached at the base branch, with no local branch left behind. An `Err` is git failing on the worktree itself.
pub fn rebase_onto_main(
    repo: &Path,
    worktree: &Path,
    base: &str,
    main: &str,
    base_sha: &str,
    main_sha: &str,
) -> Result<Rebased> {
    if worktree.join(".git").exists() {
        git(
            &command,
            worktree,
            &["checkout", "--quiet", "--force", "--detach", base_sha],
        )?;
    } else {
        std::fs::create_dir_all(worktree.parent().context("no worktrees folder")?)?;
        let at = worktree.to_str().context("the rebase worktree's path isn't text")?;
        git(
            &command,
            repo,
            &["worktree", "add", "--quiet", "--detach", at, base_sha],
        )?;
    }
    git(&command, worktree, &["checkout", "--quiet", "-B", TMP, base_sha])?;
    let done = rebase_and_push(worktree, base, main, base_sha, main_sha);
    // The worktree goes back to the base, and no local branch is left behind.
    let origin_base = format!("origin/{base}");
    let _ = git(
        &command,
        worktree,
        &["checkout", "--quiet", "--force", "--detach", &origin_base],
    );
    let _ = git(&command, worktree, &["branch", "-D", TMP]);
    done
}

fn rebase_and_push(worktree: &Path, base: &str, main: &str, base_sha: &str, main_sha: &str) -> Result<Rebased> {
    if git(&command, worktree, &["rebase", main_sha]).is_err() {
        let files = git(&command, worktree, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
        let mut files: Vec<String> = files
            .lines()
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(String::from)
            .collect();
        let _ = git(&command, worktree, &["rebase", "--abort"]);
        if files.is_empty() {
            files.push("(git didn't say which files)".into());
        }
        return Ok(Rebased::Conflict(files));
    }
    let lease = format!("--force-with-lease=refs/heads/{base}:{base_sha}");
    let to = format!("HEAD:refs/heads/{base}");
    match git(&command, worktree, &["push", &lease, "origin", &to]) {
        Ok(_) => Ok(Rebased::Done),
        Err(e) => Ok(Rebased::Failed(format!(
            "Couldn't push {base} rebased onto {main}: {}",
            truncate(format!("{e:#}").trim(), 300)
        ))),
    }
}

#[cfg(test)]
mod tests;
