//! Start over's first step (agent/09-web-ui.md section 9.3): lets go of the slice's old work so the new attempt
//! starts clean. With a PR, `gh pr close` (the branch stays, with its history); with none, the old branch is dropped,
//! from `origin` when it was pushed and from the repo here. A PR that was merged is never touched.
use std::path::Path;
use std::process::Command;

use anyhow::Result;

use super::publish::run;

/// Closes `pr_url`, or drops `branch` when there is no PR. `git` and `gh` are parameters so tests can fake them.
pub fn abandon(git: &Path, gh: &Path, repo: &Path, branch: Option<&str>, pr_url: Option<&str>) -> Result<()> {
    if let Some(pr) = pr_url {
        let args = ["pr", "view", pr, "--json", "state", "--jq", ".state"];
        crate::host_call::guard("gh", repo, &args)?;
        let mut view = Command::new(gh);
        view.current_dir(repo).args(args);
        // Only an open PR is closed: a merged slice stays as it is, and a closed one needs nothing.
        if run(view, None, "gh pr view")?.trim() == "OPEN" {
            crate::host_call::guard("gh", repo, &["pr", "close", pr])?;
            let mut close = Command::new(gh);
            close.current_dir(repo).args(["pr", "close", pr]);
            run(close, None, "gh pr close")?;
        }
        return Ok(());
    }
    let Some(branch) = branch else { return Ok(()) };
    let mut listed = Command::new(git);
    listed
        .current_dir(repo)
        .args(["ls-remote", "--heads", "origin", &format!("refs/heads/{branch}")]);
    if !crate::text::is_blank(&run(listed, None, "git ls-remote")?) {
        let mut delete = Command::new(git);
        delete.current_dir(repo).args(["push", "origin", "--delete", branch]);
        run(delete, None, "git push --delete")?;
    }
    // A lane still on the branch keeps it; the new attempt has its own name, so that only leaves a stale ref.
    let mut local = Command::new(git);
    local.current_dir(repo).args(["branch", "-D", branch]);
    let _ = run(local, None, "git branch -D");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_undefined_pr_is_refused_and_recorded_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let (gh, ran) = crate::test_support::fake_gh_that_marks(dir.path());
        let err = abandon(Path::new("git"), &gh, dir.path(), None, Some("undefined")).unwrap_err();
        assert!(err.to_string().contains("was not run"), "{err}");
        assert!(!ran.exists());
        let got = crate::host_call::refusals_in(dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].cmd, "gh");
        assert_eq!(got[0].args[..3], ["pr", "view", "undefined"]);
    }
}
