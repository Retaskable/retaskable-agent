//! The plain-code steps of a main sync (docs/specs/agent/06.7-chores-never-idle.md, "A main sync is a
//! chore too"; team-branch.md section 4): is one due, what name it uses, and the git merge of `origin/<main>`
//! into a branch off `origin/<base>`. Nothing here is wired into the loop.
use std::path::Path;

use anyhow::Result;

use crate::backlog::runs::short_sha;
use crate::config::SyncMain;
use crate::git_bin::command;
use crate::merge_update::run as git;

/// Whether a main sync should start: the base isn't the default branch, `syncMain` isn't off, the base is
/// behind main, and no `sync_main` item is open.
pub fn sync_due(base: &str, default_branch: &str, behind: u32, setting: SyncMain, open_sync_item: bool) -> bool {
    base != default_branch && setting != SyncMain::Off && behind > 0 && !open_sync_item
}

/// How many commits `origin/<main>` has that `origin/<base>` lacks.
pub fn behind_count(repo: &Path, base: &str, main: &str) -> Result<u32> {
    let range = format!("origin/{base}..origin/{main}");
    Ok(git(&command, repo, &["rev-list", "--count", &range])?.parse()?)
}

pub fn sync_branch_name(main_sha: &str) -> String {
    format!("retaskable/sync-main-{}", short_sha(main_sha))
}

pub fn pr_title(base: &str, n: usize) -> String {
    format!("Sync: main into {base} ({n} commits)")
}

pub fn pr_body(commits: &[String]) -> String {
    let list: Vec<String> = commits.iter().map(|c| format!("- {c}")).collect();
    format!(
        "Merges main into the team branch. Commits from main:\n\n{}\n",
        list.join("\n")
    )
}

pub fn item_title(base: &str) -> String {
    format!("Sync main into {base}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    /// The merge went through: `branch` holds it at `head` in the worktree's repo, ready to [`push`].
    Clean {
        branch: String,
        head: String,
        commits: Vec<String>,
    },
    /// The merge was aborted; these files conflict.
    Conflict { files: Vec<String> },
}

/// Merges `origin/<main>` into a new sync branch off `origin/<base>` in `worktree`. The worktree ends detached
/// at `origin/<base>` either way; the local branch is deleted unless the merge was clean (the caller pushes it).
pub fn attempt(worktree: &Path, base: &str, main: &str) -> Result<Attempt> {
    let origin_base = format!("origin/{base}");
    let origin_main = format!("origin/{main}");
    let branch = sync_branch_name(&git(&command, worktree, &["rev-parse", &origin_main])?);
    let commits: Vec<String> = git(
        &command,
        worktree,
        &["log", "--oneline", &format!("{origin_base}..{origin_main}")],
    )?
    .lines()
    .map(str::to_string)
    .collect();
    git(
        &command,
        worktree,
        &["checkout", "--quiet", "-B", &branch, &origin_base],
    )?;
    let message = format!("Merge main into {base}");
    let merged = git(&command, worktree, &["merge", "--no-ff", &origin_main, "-m", &message]);
    let result = match merged {
        Ok(_) => {
            let head = git(&command, worktree, &["rev-parse", "HEAD"])?;
            Attempt::Clean {
                branch: branch.clone(),
                head,
                commits,
            }
        }
        Err(e) => {
            let files = git(&command, worktree, &["diff", "--name-only", "--diff-filter=U"])?;
            let files: Vec<String> = files.lines().map(str::to_string).collect();
            let _ = git(&command, worktree, &["merge", "--abort"]);
            if files.is_empty() {
                let _ = reset(worktree, &origin_base, &branch);
                return Err(e);
            }
            Attempt::Conflict { files }
        }
    };
    git(&command, worktree, &["checkout", "--quiet", "--detach", &origin_base])?;
    if matches!(result, Attempt::Conflict { .. }) {
        git(&command, worktree, &["branch", "-D", &branch])?;
    }
    Ok(result)
}

fn reset(worktree: &Path, origin_base: &str, branch: &str) -> Result<()> {
    git(&command, worktree, &["checkout", "--quiet", "--detach", origin_base])?;
    git(&command, worktree, &["branch", "-D", branch])?;
    Ok(())
}

/// Force-pushes the clean sync to `branch`, which only ever holds this sync.
pub fn push(worktree: &Path, head: &str, branch: &str) -> Result<()> {
    git(
        &command,
        worktree,
        &["push", "--force", "origin", &format!("{head}:refs/heads/{branch}")],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git as tgit, repo_with_origin, run as trun};

    #[test]
    fn sync_due_covers_every_case() {
        use SyncMain::{Auto, Off};
        assert!(sync_due("team", "main", 3, Auto, false));
        assert!(!sync_due("main", "main", 3, Auto, false));
        assert!(!sync_due("team", "main", 3, Off, false));
        assert!(!sync_due("team", "main", 0, Auto, false));
        assert!(!sync_due("team", "main", 3, Auto, true));
    }

    #[test]
    fn names_and_pr_words() {
        assert_eq!(sync_branch_name("0123456789abcdef"), "retaskable/sync-main-0123456");
        assert_eq!(pr_title("team", 2), "Sync: main into team (2 commits)");
        assert_eq!(item_title("team"), "Sync main into team");
        let body = pr_body(&["abc one".into(), "def two".into()]);
        assert!(body.contains("- abc one\n- def two"));
    }

    fn commit(repo: &Path, name: &str, text: &str) {
        std::fs::write(repo.join(name), text).unwrap();
        trun(tgit(repo).args(["add", name]));
        trun(tgit(repo).args(["commit", "--quiet", "-m", name]));
    }

    /// origin has `main` and `team` (one extra commit); the clone sits detached at origin/team.
    fn setup(root: &Path) -> std::path::PathBuf {
        let repo = repo_with_origin(root);
        trun(tgit(&repo).args(["checkout", "--quiet", "-b", "team"]));
        commit(&repo, "team.txt", "team");
        trun(tgit(&repo).args(["push", "--quiet", "origin", "team"]));
        trun(tgit(&repo).args(["checkout", "--quiet", "main"]));
        repo
    }

    fn out(repo: &Path, args: &[&str]) -> String {
        String::from_utf8(tgit(repo).args(args).output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_string()
    }

    fn assert_resting(repo: &Path) {
        assert_eq!(
            out(repo, &["rev-parse", "HEAD"]),
            out(repo, &["rev-parse", "origin/team"])
        );
        assert_eq!(out(repo, &["status", "--porcelain"]), "");
        assert_eq!(out(repo, &["branch", "--list", "retaskable/*"]), "");
    }

    #[test]
    fn a_clean_main_gives_a_merge_commit() {
        let root = tempfile::tempdir().unwrap();
        let repo = setup(root.path());
        commit(&repo, "m.txt", "m");
        trun(tgit(&repo).args(["push", "--quiet", "origin", "main"]));
        trun(tgit(&repo).args(["fetch", "--quiet", "origin"]));
        trun(tgit(&repo).args(["checkout", "--quiet", "--detach", "origin/team"]));
        assert_eq!(behind_count(&repo, "team", "main").unwrap(), 1);
        let Attempt::Clean { branch, head, commits } = attempt(&repo, "team", "main").unwrap() else {
            panic!("expected clean")
        };
        assert_eq!(commits.len(), 1);
        assert!(commits[0].ends_with("m.txt"));
        assert_eq!(
            out(&repo, &["rev-list", "--parents", "-n1", &head]).split(' ').count(),
            3
        );
        assert_eq!(
            out(&repo, &["rev-parse", &format!("{head}^2")]),
            out(&repo, &["rev-parse", "origin/main"])
        );
        assert_eq!(out(&repo, &["log", "-1", "--format=%s", &head]), "Merge main into team");
        // The worktree is back at origin/team; the branch is kept for the push.
        assert_eq!(
            out(&repo, &["rev-parse", "HEAD"]),
            out(&repo, &["rev-parse", "origin/team"])
        );
        assert_eq!(out(&repo, &["status", "--porcelain"]), "");
        push(&repo, &head, &branch).unwrap();
        assert_eq!(
            out(&repo, &["ls-remote", "origin", &format!("refs/heads/{branch}")])
                .split('\t')
                .next()
                .unwrap(),
            head
        );
    }

    #[test]
    fn a_conflicting_main_gives_the_files_and_pushes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let repo = setup(root.path());
        trun(tgit(&repo).args(["checkout", "--quiet", "team"]));
        commit(&repo, "c.txt", "team side");
        trun(tgit(&repo).args(["push", "--quiet", "origin", "team"]));
        trun(tgit(&repo).args(["checkout", "--quiet", "main"]));
        commit(&repo, "c.txt", "main side");
        trun(tgit(&repo).args(["push", "--quiet", "origin", "main"]));
        trun(tgit(&repo).args(["fetch", "--quiet", "origin"]));
        trun(tgit(&repo).args(["checkout", "--quiet", "--detach", "origin/team"]));
        let got = attempt(&repo, "team", "main").unwrap();
        assert_eq!(
            got,
            Attempt::Conflict {
                files: vec!["c.txt".into()]
            }
        );
        assert_resting(&repo);
        assert_eq!(out(&repo, &["ls-remote", "origin", "refs/heads/retaskable/*"]), "");
    }
}
