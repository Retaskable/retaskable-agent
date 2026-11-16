//! A project entry with no `baseBranch` (a hand-edited or partial `config.json`) would make the agent read
//! `origin/` and fail every git step on it. The base branch is GitHub's default branch (the owner's decision,
//! 2026-11-16), so at start an empty one is resolved from the checkout once: `gh repo view` (or the plain
//! remote's `HEAD`), else the branch `refs/remotes/origin/HEAD` points at. One that can't be found is left
//! empty and logged, as before.
use std::path::Path;

use super::ConfigFile;

/// Fills each project's empty `base_branch` from `resolve(repo_path)`.
pub fn resolve_base_branches(cfg: &mut ConfigFile, resolve: impl Fn(&Path) -> Option<String>) {
    for project in cfg.projects.iter_mut().filter(|p| p.base_branch.trim().is_empty()) {
        match resolve(Path::new(&project.repo_path)) {
            Some(branch) => {
                tracing::info!(
                    "{} has no baseBranch in config.json: using {branch}, its repo's default branch.",
                    project.id_or_derived()
                );
                project.base_branch = branch;
            }
            None => tracing::warn!(
                "{} has no baseBranch in config.json and its repo's default branch couldn't be read: set baseBranch.",
                project.id_or_derived()
            ),
        }
    }
}

/// The checkout's default branch: its code host's answer, else what `origin/HEAD` points at.
pub fn checkout_default_branch(repo: &Path) -> Option<String> {
    crate::repo::repo_info(repo)
        .default_branch
        .filter(|b| !b.trim().is_empty())
        .or_else(|| origin_head(repo))
}

fn origin_head(repo: &Path) -> Option<String> {
    let out = crate::git_bin::command(repo)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let branch = text.trim().strip_prefix("origin/")?;
    (!branch.is_empty()).then(|| branch.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProjectConfig;

    fn cfg(branches: &[&str]) -> ConfigFile {
        ConfigFile {
            projects: branches
                .iter()
                .enumerate()
                .map(|(i, b)| ProjectConfig {
                    id: Some(format!("p{i}")),
                    repo_path: format!("/code/p{i}"),
                    base_branch: b.to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn an_empty_base_branch_becomes_the_repos_default_and_a_set_one_is_left_alone() {
        let mut c = cfg(&["", "develop", "  "]);
        resolve_base_branches(&mut c, |path| {
            assert_ne!(
                path,
                Path::new("/code/p1"),
                "a project with a base branch is not asked about"
            );
            Some("trunk".into())
        });
        let got: Vec<&str> = c.projects.iter().map(|p| p.base_branch.as_str()).collect();
        assert_eq!(got, ["trunk", "develop", "trunk"]);
    }

    #[test]
    fn a_branch_that_cant_be_found_leaves_it_empty() {
        let mut c = cfg(&[""]);
        resolve_base_branches(&mut c, |_| None);
        assert_eq!(c.projects[0].base_branch, "");
    }

    #[test]
    fn a_checkout_with_no_remote_answers_from_its_own_branch_and_origin_head_is_read_when_gh_cant() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = crate::git_bin::command(dir.path()).args(args).output().unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q", "-b", "trunk"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ]);
        assert_eq!(checkout_default_branch(dir.path()).as_deref(), Some("trunk"));
        // origin/HEAD, as `git clone` leaves it, for a remote gh can't read.
        run(&["remote", "add", "origin", "https://example.invalid/x.git"]);
        run(&["update-ref", "refs/remotes/origin/develop", "HEAD"]);
        run(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/develop",
        ]);
        assert_eq!(origin_head(dir.path()).as_deref(), Some("develop"));
    }
}
