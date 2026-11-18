//! What a "nothing left to build" answer needs before it is believed (`implement` in the TS agent's `steps.ts`, and
//! `unmergedWork` in `git.ts`): a session that found nothing left while its own branch still holds work the base
//! branch doesn't have (an earlier session's commits, never merged) has that work as its change, opened as one, never
//! "already built" (flag-in-cloud-piece-2, 2026-11-01). What the answer then does is `driver/nothing_left.rs` (a claimed
//! item) and `piece_runs.rs` (a piece's run).
use std::path::Path;

use super::session::Attempt;

/// The comment a change closed for having nothing left is closed with.
pub(super) fn closing_comment(why: &str) -> String {
    format!("Retaskable closed this: nothing is left to build. {why}")
}

/// The files the lane's branch changed that `base_ref` doesn't have as the branch has them: commits still unmerged,
/// squash-merged work excluded (its files match the base). Empty when git can't tell.
pub(super) async fn unmerged_work(cwd: &Path, base_ref: &str) -> Vec<String> {
    let lines = |out: std::io::Result<std::process::Output>| -> Vec<String> {
        match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect(),
            _ => Vec::new(),
        }
    };
    let git = || crate::spawn::tokio_command(crate::git_bin::program());
    let changed = lines(
        git()
            .current_dir(cwd)
            .args([
                "-c",
                "core.quotePath=false",
                "diff",
                "--name-only",
                &format!("{base_ref}...HEAD"),
            ])
            .output()
            .await,
    );
    if changed.is_empty() {
        return changed;
    }
    lines(
        git()
            .current_dir(cwd)
            .args([
                "-c",
                "core.quotePath=false",
                "diff",
                "--name-only",
                base_ref,
                "HEAD",
                "--",
            ])
            .args(&changed)
            .output()
            .await,
    )
}

/// A `nothing_left` answer on a branch that has unmerged work becomes `pr_opened`, so that work is checked, pushed and
/// opened as the change. The timeline's line when it did.
pub(super) async fn keep_unmerged_work(cwd: &Path, base_ref: &str, base: &str, a: &mut Attempt) -> Option<String> {
    let Ok(answer) = a.answer.as_mut() else { return None };
    if answer.status != "nothing_left" || unmerged_work(cwd, base_ref).await.is_empty() {
        return None;
    }
    answer.status = "pr_opened".into();
    Some(format!(
        "The implementer found nothing left, but its branch has work {base} doesn't, so that work is opened as the change"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new(crate::git_bin::program())
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q", "-b", "main"]);
        git(d.path(), &["commit", "-q", "--allow-empty", "-m", "base"]);
        git(d.path(), &["checkout", "-q", "-b", "lane"]);
        d
    }

    fn attempt(status: &str) -> Attempt {
        Attempt {
            answer: Ok(serde_json::from_value(serde_json::json!({ "status": status, "summary": "s" })).unwrap()),
            usage: None,
            context_tokens: None,
        }
    }

    #[tokio::test]
    async fn a_fresh_branch_has_no_unmerged_work_and_a_commit_on_it_has_some() {
        let d = repo();
        assert!(unmerged_work(d.path(), "main").await.is_empty());
        std::fs::write(d.path().join("flags.ts"), "export const flags = {};\n").unwrap();
        git(d.path(), &["add", "flags.ts"]);
        git(d.path(), &["commit", "-q", "-m", "the snapshot"]);
        assert_eq!(unmerged_work(d.path(), "main").await, vec!["flags.ts".to_string()]);
    }

    #[tokio::test]
    async fn unmerged_work_in_a_file_with_a_non_ascii_name_is_still_unmerged() {
        let d = repo();
        std::fs::write(d.path().join("café.txt"), "x\n").unwrap();
        git(d.path(), &["add", "."]);
        git(d.path(), &["commit", "-q", "-m", "work"]);
        assert_eq!(unmerged_work(d.path(), "main").await, vec!["café.txt".to_string()]);
    }

    #[tokio::test]
    async fn work_squash_merged_into_the_base_is_not_unmerged() {
        let d = repo();
        std::fs::write(d.path().join("flags.ts"), "x\n").unwrap();
        git(d.path(), &["add", "flags.ts"]);
        git(d.path(), &["commit", "-q", "-m", "the snapshot"]);
        git(d.path(), &["checkout", "-q", "main"]);
        std::fs::write(d.path().join("flags.ts"), "x\n").unwrap();
        git(d.path(), &["add", "flags.ts"]);
        git(d.path(), &["commit", "-q", "-m", "squashed"]);
        git(d.path(), &["checkout", "-q", "lane"]);
        assert!(unmerged_work(d.path(), "main").await.is_empty());
    }

    #[tokio::test]
    async fn nothing_left_with_unmerged_work_is_a_change_and_only_then() {
        let d = repo();
        let mut a = attempt("nothing_left");
        assert!(keep_unmerged_work(d.path(), "main", "main", &mut a).await.is_none());
        assert_eq!(a.answer.as_ref().unwrap().status, "nothing_left");
        std::fs::write(d.path().join("a.txt"), "a\n").unwrap();
        git(d.path(), &["add", "a.txt"]);
        git(d.path(), &["commit", "-q", "-m", "work"]);
        let mut other = attempt("failed");
        assert!(keep_unmerged_work(d.path(), "main", "main", &mut other).await.is_none());
        let note = keep_unmerged_work(d.path(), "main", "main", &mut a).await.unwrap();
        assert!(note.contains("work main doesn't"), "{note}");
        assert_eq!(a.answer.as_ref().unwrap().status, "pr_opened");
    }

    #[test]
    fn the_closing_comment_says_why() {
        assert_eq!(
            closing_comment("Built by x."),
            "Retaskable closed this: nothing is left to build. Built by x."
        );
    }
}
