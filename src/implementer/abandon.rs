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
        crate::host_call::guard("gh", repo, &["pr", "view", pr])?;
        let mut view = Command::new(gh);
        view.current_dir(repo)
            .args(["pr", "view", pr, "--json", "state", "--jq", ".state"]);
        // Only an open PR is closed: a merged slice stays as it is, and a closed one needs nothing.
        if run(view, None, "gh pr view")?.trim() == "OPEN" {
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
    if !run(listed, None, "git ls-remote")?.trim().is_empty() {
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
    fn an_undefined_pr_is_refused_and_recorded_without_running_gh() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let gh = dir
            .path()
            .join(if cfg!(windows) { "fake_gh.cmd" } else { "fake_gh.sh" });
        if cfg!(windows) {
            std::fs::write(&gh, format!("@echo off\r\necho x > \"{}\"\r\n", marker.display())).unwrap();
        } else {
            crate::test_support::write_executable(&gh, &format!("#!/bin/sh\ntouch \"{}\"\n", marker.display()));
        }
        let _ = std::fs::remove_file(&marker); // writing the script probes it
        crate::host_call::set_on_for(vec![dir.path().to_path_buf()]);
        let err = abandon(Path::new("git"), &gh, dir.path(), None, Some("undefined")).unwrap_err();
        assert!(err.to_string().contains("argument 3 was undefined"), "{err}");
        assert!(!marker.exists());
        let got = crate::host_call::Sink::global().take_for(dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(
            (got[0].cmd.as_str(), got[0].args.as_slice()),
            ("gh", &["pr".to_string(), "view".into(), "undefined".into()][..])
        );
        // With the flag Off the call runs as before.
        crate::host_call::set_on_for(vec![]);
        let _ = abandon(Path::new("git"), &gh, dir.path(), None, Some("undefined"));
        assert!(marker.exists());
    }
}
