//! Landing an approved draft (docs/specs/agent/06.5-spec-drafts.md step 3, github-workflow.md section 9):
//! who merges under the project's GitHub workflow, and the `gh` merge and `git` rebase it takes.
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::ProjectConfig;
use crate::implementer::publish::run;

/// What the project's `github` settings say about landing a draft's pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workflow {
    /// `land` is `retaskable` (the default): **Approve** merges the pull request itself.
    pub we_merge: bool,
    /// The protected branch a person's **Merge into main** squash-merges into: only when `land` is `person` and
    /// the base branch is the protected one.
    pub person_merges_into: Option<String>,
    /// `gh pr merge`'s flag for the project's merge method (squash unless it says `merge` or `rebase`).
    pub method_flag: &'static str,
    pub delete_branch: bool,
}

pub fn workflow(project: &ProjectConfig) -> Workflow {
    let text = |key: &str| {
        let value = project.github.as_ref()?.get(key)?.as_str()?.trim();
        (!value.is_empty()).then_some(value)
    };
    let land = text("land").unwrap_or("retaskable");
    let protected = text("protectedBranch").unwrap_or("main");
    Workflow {
        we_merge: land == "retaskable",
        person_merges_into: (land == "person" && project.base_branch == protected).then(|| protected.to_string()),
        method_flag: match text("mergeMethod") {
            Some("merge") => "--merge",
            Some("rebase") => "--rebase",
            _ => "--squash",
        },
        delete_branch: project
            .github
            .as_ref()
            .and_then(|g| g.get("deleteBranch")?.as_bool())
            .unwrap_or(true),
    }
}

/// The pull request number at the end of its URL.
pub fn pr_number(url: &str) -> Option<u64> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

/// A draft's own branch (`retaskable/…`) is the only one a merge deletes.
pub fn deletes_head(branch: &str) -> bool {
    branch.starts_with("retaskable/")
}

/// `gh pr merge <pr> --squash [--delete-branch]`, run in the project's checkout.
pub fn merge_pr(gh: &Path, dir: &Path, pr: u64, method_flag: &str, delete_branch: bool) -> Result<()> {
    let number = pr.to_string();
    crate::host_call::guard("gh", dir, &["pr", "merge", &number, method_flag])?;
    let mut cmd = Command::new(gh);
    cmd.current_dir(dir).args(["pr", "merge", &number, method_flag]);
    if delete_branch {
        cmd.arg("--delete-branch");
    }
    run(cmd, None, &format!("gh pr merge {pr}")).map(|_| ())
}

/// Force-pushes (with lease) the branch checked out in `dir`, after a rebase rewrote it.
pub fn push_rebased(git: &Path, dir: &Path, branch: &str) -> Result<()> {
    let mut cmd = Command::new(git);
    cmd.current_dir(dir).args([
        "push",
        "--force-with-lease",
        "origin",
        &format!("HEAD:refs/heads/{branch}"),
    ]);
    run(cmd, None, "git push").map(|_| ())
}

/// `gh pr close <pr> --delete-branch`, run in the project's checkout.
pub fn close_pr(gh: &Path, dir: &Path, pr: u64) -> Result<()> {
    let number = pr.to_string();
    let mut cmd = Command::new(gh);
    cmd.current_dir(dir).args(["pr", "close", &number, "--delete-branch"]);
    run(cmd, None, &format!("gh pr close {pr}")).map(|_| ())
}

/// Rebases the draft's branch (checked out in `dir`) onto `origin/<base>`, and force-pushes it with lease.
/// False, with nothing changed, when the rebase conflicts.
pub fn rebase(git: &Path, dir: &Path, branch: &str, base: &str) -> Result<bool> {
    let git_in = |args: &[&str]| {
        let mut cmd = Command::new(git);
        cmd.current_dir(dir).args(args);
        cmd
    };
    run(git_in(&["fetch", "origin"]), None, "git fetch")?;
    run(
        git_in(&["reset", "--hard", &format!("origin/{branch}")]),
        None,
        "git reset",
    )?;
    let mut rebase = git_in(&["rebase", &format!("origin/{base}")]);
    let out = rebase.output().context("running git rebase")?;
    if !out.status.success() {
        if git_in(&["rebase", "--abort"]).output().is_err() {
            bail!("git rebase --abort didn't run");
        }
        return Ok(false);
    }
    run(
        git_in(&[
            "push",
            "--force-with-lease",
            "origin",
            &format!("HEAD:refs/heads/{branch}"),
        ]),
        None,
        "git push",
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project(github: Option<serde_json::Value>) -> ProjectConfig {
        ProjectConfig {
            base_branch: "main".into(),
            github,
            ..Default::default()
        }
    }

    #[test]
    fn no_settings_means_retaskable_merges_and_squashes() {
        let w = workflow(&project(None));
        assert!(w.we_merge && w.delete_branch);
        assert_eq!((w.method_flag, w.person_merges_into), ("--squash", None));
    }

    #[test]
    fn a_person_lands_it_on_the_protected_base_only() {
        let g = json!({ "land": "person", "protectedBranch": "main" });
        let w = workflow(&project(Some(g.clone())));
        assert!(!w.we_merge);
        assert_eq!(w.person_merges_into.as_deref(), Some("main"));
        let mut other = project(Some(g));
        other.base_branch = "develop".into();
        assert_eq!(workflow(&other).person_merges_into, None);
    }

    #[test]
    fn a_rebased_branch_is_force_pushed_with_lease() {
        use crate::test_support::{git, repo_with_origin, run};
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        run(git(&repo).args(["checkout", "--quiet", "-b", "b"]));
        std::fs::write(repo.join("a.txt"), "1").unwrap();
        run(git(&repo).args(["add", "-A"]));
        run(git(&repo).args(["commit", "--quiet", "-m", "one"]));
        run(git(&repo).args(["push", "--quiet", "origin", "b"]));
        run(git(&repo).args(["fetch", "--quiet", "origin"]));
        // Rewrite the pushed commit: a plain push would be rejected.
        run(git(&repo).args(["commit", "--quiet", "--amend", "-m", "rewritten"]));
        push_rebased(Path::new("git"), &repo, "b").unwrap();
    }

    #[test]
    fn a_merge_with_an_undefined_argument_is_refused_and_recorded_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let (gh, ran) = crate::test_support::fake_gh_that_marks(dir.path());
        let err = merge_pr(&gh, dir.path(), 7, "", true).unwrap_err();
        assert!(err.to_string().contains("was not run"), "{err}");
        assert!(!ran.exists());
        let got = crate::host_call::refusals_in(dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].cmd, "gh");
        assert_eq!(got[0].args[..3], ["pr", "merge", "7"]);
    }

    #[test]
    fn the_pr_number_ends_the_url() {
        assert_eq!(pr_number("https://github.com/o/r/pull/12"), Some(12));
        assert_eq!(pr_number("https://github.com/o/r"), None);
    }
}
