//! What the CI watcher reads from, and does to, the code host (docs/specs/ci-watch-through-store.md section 3,
//! item 2): the open pull requests, a head's checks, starting a pull request's check again, the signed-in login, the
//! fetched base's sha and merging the base into a pull request's branch. `commitChecks` and `list` of the TS agent's
//! `CodeHost`, with `ciRuns` and `labels` for the rerun. A trait, so the pass has no `gh` in it: [`GhCiHost`] is
//! GitHub's; GitLab's is a second implementation of the same calls (`glab`, its pipelines for the checks, a pipeline
//! retry for the rerun) picked in [`host_for`], and nothing else changes.
//! Every call blocks (it runs a program), so the pass calls them on a blocking thread.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Result, bail};

use crate::backlog::ci_result::CiChecks;
use crate::ci_checks::{actions_run_id, read_ci_checks};
use crate::ci_fix::FixTree;
use crate::ci_label::READY_LABEL;
use crate::ci_pulls::{OpenPull, open_pulls};
use crate::ci_watch::{CheckState, Reg, gh_out, parse_json};
use crate::config::ProjectConfig;
use crate::lanes::{fetch_is_stale, fetch_latest_with};
use crate::merge_update::{Update, run as git_run, update_from_base};

/// What merging the base into a pull request's branch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The branch now holds the merge, pushed.
    Pushed,
    /// The merge conflicts in these files; nothing was pushed and the branch is as it was.
    Conflict { base: String, files: Vec<String> },
    /// Any other failure; nothing was pushed.
    Failed,
}

pub trait CiHost: Send + Sync {
    /// Whether the host can answer a head's checks. The TS agent's `ciWatchPass` and Fix button do nothing without
    /// `host.commitChecks`, which only GitHub's host has (GitLab's has none): the pass ends with no finding and the
    /// button refuses.
    fn reads_commit_checks(&self) -> bool {
        true
    }
    /// The open pull requests into the project's base branch, at most 100.
    fn open_pulls(&self) -> Result<Vec<OpenPull>>;
    /// A head's checks as the facts `ci_result` takes. `Err` is the host's reason when it can't say.
    fn checks(&self, sha: &str) -> Result<CiChecks>;
    /// Starts the pull request's check again, with a hung run's pending checks cancelled first when `hung`.
    /// False when the host couldn't.
    fn rerun(&self, pr: u64, hung: bool) -> bool;
    /// The account the host's CLI is signed in as; `None` when it can't say (a failed read, or an empty one).
    fn login(&self) -> Option<String>;
    /// The sha of `origin/<base>` after a fetch (a failed fetch reads the ref as it was); `None` when it can't be read.
    fn base_head(&self) -> Option<String>;
    /// Merges the base into the pull request's branch in a throwaway worktree and pushes it plainly (never forced,
    /// never a rebase). A conflict is left as it was and named, for a session to resolve.
    fn merge_main(&self, pr: u64, branch: &str) -> MergeOutcome;
    /// A throwaway worktree on the branch's pushed head for a fix session (fetched first), removed when dropped.
    fn fix_tree(&self, pr: u64, branch: &str, sha: &str) -> Result<FixTree>;
}

/// How to start git in a directory.
pub type GitCommand = Arc<dyn Fn(&Path) -> Command + Send + Sync>;

/// GitHub, through `gh` run in the project's checkout.
pub struct GhCiHost {
    pub gh: PathBuf,
    pub dir: PathBuf,
    pub repo: String,
    pub base: String,
    /// `requireCi` with `ciWhenReady`: CI runs on the `ci-ready` label, so a rerun is taking it off and putting it back.
    pub ready_label: bool,
    /// Where the merge's and the fix's throwaway worktrees go (the project's `worktreesDir`).
    pub worktrees: PathBuf,
    /// How git is started.
    pub git: GitCommand,
}

/// The code host of `project`: GitHub's, or for any other (GitLab's `codeHost`, as TS) one that reads no checks, so
/// the pass and the Fix button do nothing and no `gh` call is made.
pub fn host_for(project: &ProjectConfig, gh: PathBuf) -> Arc<dyn CiHost> {
    if !project.on_github() {
        return Arc::new(NoCommitChecks);
    }
    Arc::new(GhCiHost {
        gh,
        dir: PathBuf::from(&project.repo_path),
        repo: project.github_repo.clone(),
        base: project.base_branch.clone(),
        ready_label: project.ci_when_ready_active(),
        worktrees: PathBuf::from(&project.worktrees_dir),
        git: Arc::new(crate::git_bin::command),
    })
}

/// Puts `ci-ready` on pull request `pr` when the project runs CI only once a change is ready (`ciWhenReady`, with CI
/// required), as the TS agent's `startCi` does once a lane's work is pushed. The runner skips a `retaskable/` branch
/// without it, so a piece's change with no label gets no CI at all. Adding a label that is already on is harmless. A
/// no-op (`Ok(false)`) for a project that doesn't use the label, or isn't on GitHub. Blocks: run it on a blocking thread.
pub fn ensure_ready_label(project: &ProjectConfig, gh: PathBuf, pr: u64) -> Result<bool> {
    let project = with_agent_ci(project);
    if !project.ci_when_ready_active() || !project.on_github() {
        return Ok(false);
    }
    let host = GhCiHost {
        gh,
        dir: PathBuf::from(&project.repo_path),
        repo: project.github_repo.clone(),
        base: project.base_branch.clone(),
        ready_label: true,
        worktrees: PathBuf::from(&project.worktrees_dir),
        git: Arc::new(crate::git_bin::command),
    };
    host.add_label(pr)?;
    Ok(true)
}

/// `project` with `agent_ci` read now. `ProjectConfig::agent_ci` is laid by the loop's look at the base, which a run's own
/// copy of the config may lack (and which is unset until the first look): read `.agent-ci/` on the base, as the TS
/// agent's `agentCiOf` does.
fn with_agent_ci(project: &ProjectConfig) -> ProjectConfig {
    let mut project = project.clone();
    if !project.agent_ci {
        let base = format!("origin/{}", project.base_branch);
        let tree = crate::ci_coverage::workflows_tree(Path::new(&project.repo_path), &base);
        project.agent_ci = crate::ci_coverage::agent_ci_in(tree.as_deref());
    }
    project
}

/// Starts CI again on pull request `pr`'s current head, forced (the store's `change_actions` rerun): with the `ci-ready`
/// label, taking it off and putting it back; otherwise every failed or cancelled GitHub Actions run is re-run
/// (`gh run rerun --failed`). False when the host couldn't. Blocks: run it on a blocking thread.
pub fn rerun_forced(project: &ProjectConfig, gh: PathBuf, pr: u64) -> bool {
    let project = with_agent_ci(project);
    let host = GhCiHost {
        gh,
        dir: PathBuf::from(&project.repo_path),
        repo: project.github_repo.clone(),
        base: project.base_branch.clone(),
        ready_label: project.ci_when_ready_active(),
        worktrees: PathBuf::from(&project.worktrees_dir),
        git: Arc::new(crate::git_bin::command),
    };
    if host.ready_label {
        return host.rerun(pr, false);
    }
    host.each_run(
        &host.runs_in(pr, &[CheckState::Fail, CheckState::Cancel]),
        &["rerun", "--failed"],
    )
}

/// A code host with no `commitChecks` (GitLab, as in the TS agent). Every call but the question answers nothing and
/// runs no program; the pass asks [`CiHost::reads_commit_checks`] first.
pub struct NoCommitChecks;

impl CiHost for NoCommitChecks {
    fn reads_commit_checks(&self) -> bool {
        false
    }

    fn open_pulls(&self) -> Result<Vec<OpenPull>> {
        Ok(Vec::new())
    }

    fn checks(&self, _: &str) -> Result<CiChecks> {
        bail!("this code host reads no commit checks")
    }

    fn rerun(&self, _: u64, _: bool) -> bool {
        false
    }

    fn login(&self) -> Option<String> {
        None
    }

    fn base_head(&self) -> Option<String> {
        None
    }

    fn merge_main(&self, _: u64, _: &str) -> MergeOutcome {
        MergeOutcome::Failed
    }

    fn fix_tree(&self, _: u64, _: &str, _: &str) -> Result<FixTree> {
        bail!("this code host reads no commit checks")
    }
}

const NONE: Reg<'static> = &|_| {};

impl GhCiHost {
    fn gh(&self, args: &[&str]) -> Result<(String, String, bool)> {
        gh_out(&self.gh, &self.dir, args, NONE)
    }

    /// `ci-ready` on the pull request, made first if the repository has no such label.
    pub(crate) fn add_label(&self, pr: u64) -> Result<()> {
        let path = format!("repos/{}/issues/{pr}/labels", self.repo);
        let field = format!("labels[]={READY_LABEL}");
        let add = || self.gh(&["api", "-X", "POST", &path, "-f", &field]);
        let (_, err, ok) = add()?;
        if ok {
            return Ok(());
        }
        let said = err.to_lowercase();
        if !(said.contains("not found") || said.contains("422") || said.contains("unprocessable")) {
            bail!("gh api labels failed: {}", err.trim());
        }
        self.gh(&[
            "label",
            "create",
            READY_LABEL,
            "--repo",
            &self.repo,
            "--force",
            "--description",
            "Retaskable: approved, so CI runs",
        ])?;
        let (_, err, ok) = add()?;
        if !ok {
            bail!("gh api labels failed: {}", err.trim());
        }
        Ok(())
    }

    /// `ci-ready` off the pull request; a label that isn't on it is as good as off.
    pub(crate) fn remove_label(&self, pr: u64) -> Result<()> {
        let path = format!("repos/{}/issues/{pr}/labels/{READY_LABEL}", self.repo);
        let (_, err, ok) = self.gh(&["api", "-X", "DELETE", &path])?;
        let said = err.to_lowercase();
        if ok || said.contains("not found") || said.contains("404") {
            return Ok(());
        }
        bail!("gh api labels failed: {}", err.trim());
    }

    /// The GitHub Actions runs behind the pull request's checks in `states` (another CI's checks have no run), each once.
    fn runs_in(&self, pr: u64, states: &[CheckState]) -> Vec<String> {
        let number = pr.to_string();
        let args = [
            "pr",
            "checks",
            &number,
            "--repo",
            &self.repo,
            "--json",
            "name,bucket,link",
        ];
        let Ok((out, _, _)) = self.gh(&args) else {
            return Vec::new();
        };
        let mut ids: Vec<String> = Vec::new();
        for check in parse_json(&out).unwrap_or_default() {
            let Some(id) = actions_run_id(&check.link) else {
                continue;
            };
            if states.contains(&check.state) && !ids.iter().any(|i| i == id) {
                ids.push(id.to_string());
            }
        }
        ids
    }

    /// Cancels the pull request's pending runs, so the next read sees them cancelled and runs them again (`ciRuns.cancelPending`).
    /// True when one was cancelled.
    pub(crate) fn cancel_pending(&self, pr: u64) -> bool {
        self.each_run(&self.runs_in(pr, &[CheckState::Pending]), &["cancel"])
    }

    /// `gh run <verb> <id>` for each run: true when one went.
    fn each_run(&self, ids: &[String], verb: &[&str]) -> bool {
        // Every run is tried, so no short circuit.
        let went: Vec<bool> = ids
            .iter()
            .map(|id| {
                let mut args = vec!["run", verb[0], id.as_str()];
                args.extend_from_slice(&verb[1..]);
                args.extend(["--repo", &self.repo]);
                matches!(self.gh(&args), Ok((_, _, true)))
            })
            .collect();
        went.contains(&true)
    }
}

impl CiHost for GhCiHost {
    fn open_pulls(&self) -> Result<Vec<OpenPull>> {
        let pulls = open_pulls(&self.gh, &self.dir, &self.repo, &self.base, NONE)?;
        // Kept for `health`'s `openChanges` (the store closes the agent's pull requests nobody owns).
        crate::open_changes::record(&self.repo, &pulls);
        Ok(pulls)
    }

    fn checks(&self, sha: &str) -> Result<CiChecks> {
        read_ci_checks(&self.gh, &self.dir, &self.repo, sha, NONE)
    }

    fn rerun(&self, pr: u64, hung: bool) -> bool {
        if self.ready_label {
            return self.remove_label(pr).and_then(|()| self.add_label(pr)).is_ok();
        }
        if hung {
            // The pending runs are cancelled so they read as cancelled and run again.
            self.each_run(&self.runs_in(pr, &[CheckState::Pending]), &["cancel"]);
        }
        // Only the runs behind cancelled checks, as `rerunCancelledRuns` in the TS agent's gh.ts does
        // (`actionsRuns(rows, "cancel")`, then `gh run rerun --failed`). A plain failed run is never rerun, so a
        // force rerun of an ordinary red answers `ok: false`. That is suspected to be a TS bug: it is to be fixed in
        // Retaskable.com first and then copied here.
        let ids = self.runs_in(pr, &[CheckState::Cancel]);
        self.each_run(&ids, &["rerun", "--failed"])
    }

    fn login(&self) -> Option<String> {
        let (out, _, ok) = self.gh(&["api", "user", "--jq", ".login"]).ok()?;
        let login = out.trim();
        (ok && !login.is_empty()).then(|| login.to_string())
    }

    fn base_head(&self) -> Option<String> {
        // Fresh, not the last fetch's: a head is told to wait for main to move by this sha. A failed fetch still reads.
        fetch_is_stale(&self.dir);
        if let Err(e) = fetch_latest_with(&*self.git, &self.dir) {
            tracing::info!("ci watch: fetching {} failed: {e:#}", self.base);
        }
        let sha = git_run(&*self.git, &self.dir, &["rev-parse", &format!("origin/{}", self.base)]).ok()?;
        (!sha.is_empty()).then_some(sha)
    }

    fn merge_main(&self, pr: u64, branch: &str) -> MergeOutcome {
        // `merge-ci-<pr>` under the worktrees folder, as the TS agent's `updateFromBase(cfg, "ci-<pr>", branch)`.
        let piece = format!("ci-{pr}");
        match update_from_base(&*self.git, &self.dir, &self.worktrees, &piece, branch, &self.base) {
            Ok(Update::Updated { .. }) => MergeOutcome::Pushed,
            Ok(Update::Conflict { files }) => {
                tracing::info!(
                    "ci watch: merging {} into pull request #{pr} conflicts in {}: handing it to a session",
                    self.base,
                    files.join(", ")
                );
                MergeOutcome::Conflict {
                    base: self.base.clone(),
                    files,
                }
            }
            Err(e) => {
                tracing::info!("ci watch: merging {} into pull request #{pr} failed: {e:#}", self.base);
                MergeOutcome::Failed
            }
        }
    }

    fn fix_tree(&self, pr: u64, branch: &str, sha: &str) -> Result<FixTree> {
        FixTree::open(&self.git, &self.dir, &self.worktrees, pr, branch, sha)
    }
}
