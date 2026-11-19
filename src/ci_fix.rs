//! The CI watcher's fix session (docs/specs/ci-watch-through-store.md section 3, `fixRed` in the TS agent's
//! `ciWatchFix.ts` and `fixOnBranch` in its `git.ts`): a write session that commits a fix for a pull request's own
//! red in a throwaway worktree detached at `origin/<branch>`, then one plain push of what it committed. The session
//! is the implementer lane's own ([`implementer::session_spec`]: the implementer's schema, no read-only, the
//! project's sandbox setting), so its engine permissions are the lane's. This file is the prompt, the session's
//! spec and the worktree around it; the store decides when a fix is run, and the pass sends the `did`.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::ProjectConfig;
use crate::engines::SessionSpec;
use crate::engines::claude::sandbox_enabled;
use crate::engines::engine::{CursorOptions, EngineId, LocalTarget};
use crate::implementer::{Session, Start, session_spec as implementer_spec};
use crate::lanes::{fetch_is_stale, fetch_latest_with};
use crate::merge_update::{clear_worktree, run, worktree_path};
use crate::prompt::render_prompt;

pub const TEMPLATE: &str = include_str!("../prompts/ci-fix.md");
/// The brief for a pull request that conflicts with its base.
pub const MERGE_TEMPLATE: &str = include_str!("../prompts/ci-merge.md");

/// What a fix session is asked to do on the pull request's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixTask {
    /// The pull request's own red: the failed checks' note.
    Red(String),
    /// The automatic merge of the base conflicted: [`crate::merge_update::conflict_instruction`]'s words.
    Merge(String),
}

impl FixTask {
    /// The note or instruction itself.
    pub fn text(&self) -> &str {
        match self {
            Self::Red(t) | Self::Merge(t) => t,
        }
    }

    fn template(&self) -> &'static str {
        match self {
            Self::Red(_) => TEMPLATE,
            Self::Merge(_) => MERGE_TEMPLATE,
        }
    }
}

/// What the session runs with besides the pull request's own words.
pub struct FixSession<'a> {
    pub project: &'a ProjectConfig,
    /// The throwaway worktree.
    pub cwd: &'a Path,
    pub model: String,
    pub max_turns: u32,
    pub engine: Option<EngineId>,
    pub cursor: Option<CursorOptions>,
    pub local: Option<LocalTarget>,
}

/// The fix session for a failing or conflicting pull request; `task` says which. A fresh implementer session with
/// the prompt as its whole brief ([`Start::Continuing`]): no item, no template of the lane's.
pub fn session_spec(pr: u64, title: &str, branch: &str, task: &FixTask, s: &FixSession<'_>) -> Result<SessionSpec> {
    let vars = [
        ("pr", pr.to_string()),
        ("title", title.to_string()),
        ("branch", branch.to_string()),
        ("what", task.text().to_string()),
    ];
    let prompt = render_prompt(task.template(), &vars)?;
    let p = s.project;
    implementer_spec(
        &Session {
            template: "",
            project_type: p.project_type,
            item: &Default::default(),
            branch,
            base_branch: &p.base_branch,
            github_repo: &p.github_repo,
            checks: &p.checks,
            code_gates: p.code_gates,
            where_to_start: "",
            notes: None,
            items: &[],
            cwd: s.cwd,
            model: s.model.clone(),
            budget_usd: p.budget.implementer_usd,
            max_turns: Some(s.max_turns),
            engine: s.engine,
            cursor: s.cursor.clone(),
            local: s.local.clone(),
            sandbox: sandbox_enabled(p.sandbox, cfg!(windows)),
        },
        Start::Continuing {
            follow_up: prompt.trim_end(),
        },
    )
}

/// `<worktreesDir>/merge-fix-<pr>-<first 8 of the head's sha>`. The TS agent's `mergeDir(cfg, "fix-<id>")` has no sha, so
/// a press and a pass on two heads of one pull request clear each other's worktree; here each head has its own.
pub fn fix_dir(worktrees: &Path, pr: u64, sha: &str) -> PathBuf {
    let short: String = sha.chars().take(8).collect();
    worktree_path(worktrees, &format!("fix-{pr}-{short}"))
}

/// The branch's pushed head in a throwaway worktree, removed when dropped (pushed, left alone, failed or cut).
pub struct FixTree {
    git: crate::ci_pass::host::GitCommand,
    repo: PathBuf,
    pub dir: PathBuf,
    before: String,
}

impl FixTree {
    /// Fetches origin, then adds the worktree detached at `origin/<branch>` (what is left of an earlier one goes first).
    /// The store judged `sha`: if the branch has moved since (the author pushed), this is an error and the worktree
    /// goes, since a fix would be for code the store never saw. (The TS agent fixes whatever the branch is now.)
    pub fn open(
        git: &crate::ci_pass::host::GitCommand,
        repo: &Path,
        worktrees: &Path,
        pr: u64,
        branch: &str,
        sha: &str,
    ) -> Result<Self> {
        let dir = fix_dir(worktrees, pr, sha);
        clear_worktree(&**git, repo, &dir);
        fetch_is_stale(repo);
        fetch_latest_with(&**git, repo)?;
        std::fs::create_dir_all(worktrees).with_context(|| format!("creating {}", worktrees.display()))?;
        run(
            &**git,
            repo,
            &[
                "worktree",
                "add",
                "--detach",
                &dir.to_string_lossy(),
                &format!("origin/{branch}"),
            ],
        )?;
        let mut tree = Self {
            git: git.clone(),
            repo: repo.to_path_buf(),
            dir,
            before: String::new(),
        };
        tree.before = run(&*tree.git, &tree.dir, &["rev-parse", "HEAD"])?;
        if tree.before != sha {
            // Dropping `tree` removes the worktree.
            anyhow::bail!("the branch moved on from {sha} to {}", tree.before);
        }
        Ok(tree)
    }

    /// After a session that went `ok`: pushes what it committed to `branch`, plainly (never forced, so a branch that
    /// moved meanwhile refuses it). False, with nothing pushed, for a failed session, uncommitted work (not pushed:
    /// the work isn't the commit) or no new commit.
    pub fn push(&self, branch: &str, ok: bool) -> Result<bool> {
        if !ok || !run(&*self.git, &self.dir, &["status", "--porcelain", "-uall"])?.is_empty() {
            return Ok(false);
        }
        if run(&*self.git, &self.dir, &["rev-parse", "HEAD"])? == self.before {
            return Ok(false);
        }
        run(
            &*self.git,
            &self.dir,
            &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        )?;
        fetch_is_stale(&self.repo);
        Ok(true)
    }
}

impl Drop for FixTree {
    fn drop(&mut self) {
        clear_worktree(&*self.git, &self.repo, &self.dir);
    }
}

#[cfg(test)]
mod tests;
