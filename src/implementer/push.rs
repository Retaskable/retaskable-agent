//! Pushing a finished session's branch (`saveLaneWork` and `PushConflictError` in Retaskable.com's
//! `agent/src/git.ts`). A rejected push means the branch moved on the host (its Update branch button, or a
//! commit pushed from another lane or computer): the host's commits are merged in (`git pull --no-rebase`, never a
//! rebase) and the push is tried once more. It is never forced. A merge that conflicts is left as it was and
//! reported as [`PushConflict`], which a session can act on, unlike the same push retried.
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result};

use super::publish::run;

/// The host's copy of a branch has commits the lane's doesn't, and merging them in conflicts with what is committed
/// here: most often because a session rewrote commits that were already pushed (a reset, a rebase or a squash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushConflict {
    pub branch: String,
}

impl fmt::Display for PushConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "couldn't push {}: merging its remote copy in conflicts with the branch",
            self.branch
        )
    }
}

impl std::error::Error for PushConflict {}

impl PushConflict {
    /// What the session is told (`pushConflictMessage` in `steps.ts`).
    pub fn message(&self) -> String {
        let b = &self.branch;
        format!(
            "Retaskable couldn't push `{b}`: its remote copy has commits this one doesn't, and merging them in \
             conflicts with what's committed here. This usually means `{b}` was rebased, reset or squashed after it \
             was already pushed. Run `git fetch origin`, then `git merge origin/{b}` (don't rebase or reset), \
             resolve the conflicts, and commit without rewriting commits already on the branch. Run the tests, \
             then return the result again."
        )
    }
}

fn rejected(error: &str) -> bool {
    ["rejected", "non-fast-forward", "fetch first"]
        .iter()
        .any(|w| error.contains(w))
}

/// Pushes `dir`'s `HEAD` to `origin/<branch>`. A rejection brings the host's copy in and pushes again; a merge
/// that conflicts is aborted and answers a [`PushConflict`] (`downcast_ref` it). Any other failure is the push's own.
pub fn push_branch(git: &Path, dir: &Path, branch: &str) -> Result<()> {
    let push = || {
        let mut push = crate::spawn::command(git);
        push.current_dir(dir)
            .args(["push", "-u", "origin", &format!("HEAD:refs/heads/{branch}")]);
        run(push, None, &format!("git push of {branch}"))
    };
    let first = match push() {
        Ok(_) => return Ok(()),
        Err(e) => e,
    };
    if !rejected(&format!("{first:#}")) {
        return Err(first);
    }
    let mut pull = crate::spawn::command(git);
    // The merge makes a commit, which needs an identity: the lane's own when it has one, else the agent's.
    let named = crate::spawn::command(git)
        .current_dir(dir)
        .args(["config", "user.email"])
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().is_empty());
    if !named {
        pull.args(["-c", "user.name=Retaskable", "-c", "user.email=agent@retaskable.com"]);
    }
    pull.current_dir(dir)
        .args(["pull", "--no-rebase", "--no-edit", "origin", branch]);
    if let Err(e) = run(pull, None, &format!("git pull of {branch}")) {
        let mut abort = crate::spawn::command(git);
        abort.current_dir(dir).args(["merge", "--abort"]);
        let _ = run(abort, None, "git merge --abort");
        // The branch is gone from the host: nothing to merge in, so the rejection stands as it was.
        if format!("{e:#}").contains("couldn't find remote ref") {
            return Err(first);
        }
        return Err(anyhow::Error::new(PushConflict {
            branch: branch.to_string(),
        }));
    }
    push()
        .map(|_| ())
        .context("pushing again after merging the host's copy in")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.email=t@example.com", "-c", "user.name=T"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Two clones of one bare origin: `a` is the lane, `b` is where the host's copy of `feat` is moved on.
    fn rig() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let origin = root.path().join("origin.git");
        git(root.path(), &["init", "--quiet", "--bare", "-b", "main", "origin.git"]);
        let (a, b) = (root.path().join("a"), root.path().join("b"));
        for dir in [&a, &b] {
            git(
                root.path(),
                &["clone", "--quiet", origin.to_str().unwrap(), dir.to_str().unwrap()],
            );
        }
        std::fs::write(a.join("f.txt"), "one\n").unwrap();
        git(&a, &["add", "."]);
        git(&a, &["commit", "--quiet", "-m", "base"]);
        git(&a, &["push", "--quiet", "origin", "HEAD:refs/heads/main"]);
        git(&a, &["checkout", "--quiet", "-b", "feat"]);
        git(&a, &["push", "--quiet", "-u", "origin", "feat"]);
        git(&b, &["fetch", "--quiet", "origin"]);
        git(&b, &["checkout", "--quiet", "-b", "feat", "origin/feat"]);
        // Nothing here may depend on the machine's own git configuration.
        for dir in [&a, &b] {
            git(dir, &["config", "user.name", "T"]);
            git(dir, &["config", "user.email", "t@example.com"]);
            git(dir, &["config", "pull.rebase", "false"]);
            git(dir, &["config", "commit.gpgsign", "false"]);
        }
        (root, origin, a, b)
    }

    fn commit(dir: &Path, file: &str, text: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "--quiet", "-m", text.trim()]);
    }

    #[test]
    fn a_plain_push_just_pushes() {
        let (_root, origin, a, _b) = rig();
        commit(&a, "f.txt", "two\n");
        push_branch(Path::new("git"), &a, "feat").unwrap();
        assert_eq!(git(&origin, &["rev-parse", "feat"]), git(&a, &["rev-parse", "HEAD"]));
    }

    #[test]
    fn a_branch_that_moved_on_the_host_is_merged_in_and_pushed_not_forced() {
        let (_root, origin, a, b) = rig();
        commit(&b, "other.txt", "from the host\n");
        git(&b, &["push", "--quiet", "origin", "feat"]);
        let host = git(&b, &["rev-parse", "HEAD"]);
        commit(&a, "f.txt", "two\n");

        push_branch(Path::new("git"), &a, "feat").unwrap();

        let pushed = git(&origin, &["rev-parse", "feat"]);
        assert_eq!(pushed, git(&a, &["rev-parse", "HEAD"]));
        git(&origin, &["merge-base", "--is-ancestor", &host, &pushed]);
        assert!(a.join("other.txt").exists() && a.join("f.txt").exists());
    }

    #[test]
    fn a_lane_with_no_git_identity_still_merges_the_host_copy_in() {
        let (_root, origin, a, b) = rig();
        git(&a, &["config", "user.name", ""]);
        git(&a, &["config", "user.email", ""]);
        commit(&b, "other.txt", "from the host\n");
        git(&b, &["push", "--quiet", "origin", "feat"]);
        std::fs::write(a.join("f.txt"), "two\n").unwrap();
        git(&a, &["add", "."]);
        git(
            &a,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.com",
                "commit",
                "--quiet",
                "-m",
                "two",
            ],
        );

        push_branch(Path::new("git"), &a, "feat").unwrap();

        assert_eq!(git(&origin, &["rev-parse", "feat"]), git(&a, &["rev-parse", "HEAD"]));
    }

    #[test]
    fn a_conflict_with_the_host_copy_is_left_as_it_was_and_named() {
        let (_root, origin, a, b) = rig();
        commit(&b, "f.txt", "host\n");
        git(&b, &["push", "--quiet", "origin", "feat"]);
        let host = git(&origin, &["rev-parse", "feat"]);
        commit(&a, "f.txt", "lane\n");
        let head = git(&a, &["rev-parse", "HEAD"]);

        let err = push_branch(Path::new("git"), &a, "feat").unwrap_err();

        assert_eq!(
            err.downcast_ref::<PushConflict>(),
            Some(&PushConflict { branch: "feat".into() })
        );
        assert_eq!(git(&origin, &["rev-parse", "feat"]), host, "never forced");
        assert_eq!(git(&a, &["rev-parse", "HEAD"]), head, "the merge is aborted");
        assert_eq!(git(&a, &["status", "--porcelain"]), "");
    }

    #[test]
    fn a_push_that_fails_for_another_reason_is_not_a_conflict() {
        let (_root, _origin, a, _b) = rig();
        git(&a, &["remote", "set-url", "--push", "origin", "/nowhere/at/all.git"]);
        commit(&a, "f.txt", "two\n");
        let err = push_branch(Path::new("git"), &a, "feat").unwrap_err();
        assert!(err.downcast_ref::<PushConflict>().is_none(), "{err:#}");
    }
}
