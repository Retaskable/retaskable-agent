//! docs/specs/docker.md section 10: the heavy ignored folders a lane grows (`target` and `node_modules`, at the lane's
//! root and one folder down, as a monorepo has them), which a build puts back. A free lane that has sat idle gets them
//! cleared, and the checkout stays for the next item.
use std::path::Path;

use super::{git_output_in_worktree, is_worktree};

const BUILD_OUTPUT: [&str; 2] = ["target", "node_modules"];

/// Deletes a free lane's `target` and `node_modules`. Only folders of exactly those names, only inside the lane (a
/// worktree), never a link and never one with a tracked file in it. Returns the bytes freed; whatever can't be
/// removed is left (a locked file on Windows).
pub fn clear_build_output(dir: &Path) -> u64 {
    if !is_worktree(dir) {
        return 0;
    }
    let mut places = vec![dir.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if e.file_type().is_ok_and(|t| t.is_dir()) && !name.starts_with('.') && !BUILD_OUTPUT.contains(&&*name) {
                places.push(e.path());
            }
        }
    }
    let mut freed = 0;
    for place in places {
        for name in BUILD_OUTPUT {
            let target = place.join(name);
            let Ok(meta) = std::fs::symlink_metadata(&target) else {
                continue;
            };
            if !meta.is_dir() || meta.file_type().is_symlink() {
                continue;
            }
            let rel = target
                .strip_prefix(dir)
                .unwrap_or(&target)
                .to_string_lossy()
                .replace('\\', "/");
            // A listing git can't give is as good as a tracked file: leave it.
            let untracked =
                git_output_in_worktree(dir, &["ls-files", "--", &rel]).is_ok_and(|out| out.trim().is_empty());
            if !untracked {
                continue;
            }
            let size = crate::lane_pool::dir_size(&target);
            if std::fs::remove_dir_all(&target).is_ok() {
                freed += size;
            }
        }
    }
    freed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanes::{ensure, lane_path};

    fn git(dir: &Path, args: &[&str]) {
        let ok = crate::git_bin::command(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    fn repo_with_lane() -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        git(repo.path(), &["config", "user.email", "t@t"]);
        git(repo.path(), &["config", "user.name", "t"]);
        std::fs::write(repo.path().join(".gitignore"), "target\nnode_modules\n").unwrap();
        std::fs::write(repo.path().join("README.md"), "x").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "init"]);
        let worktrees = tempfile::tempdir().unwrap();
        ensure(repo.path(), worktrees.path(), 1).unwrap();
        let lane = lane_path(worktrees.path(), 1);
        (repo, worktrees, lane)
    }

    #[test]
    fn it_clears_target_and_node_modules_and_leaves_the_checkout() {
        let (_repo, _wt, lane) = repo_with_lane();
        for d in ["target/debug", "node_modules/x", "app/node_modules/y", "app/target"] {
            std::fs::create_dir_all(lane.join(d)).unwrap();
        }
        std::fs::write(lane.join("target/debug/big"), vec![0u8; 1000]).unwrap();
        std::fs::write(lane.join("app/node_modules/y/f"), vec![0u8; 500]).unwrap();
        let freed = clear_build_output(&lane);
        assert_eq!(freed, 1500);
        for d in ["target", "node_modules", "app/node_modules", "app/target"] {
            assert!(!lane.join(d).exists(), "{d}");
        }
        assert!(lane.join("README.md").exists());
        assert!(is_worktree(&lane));
    }

    #[test]
    fn it_leaves_a_folder_git_tracks() {
        let (_repo, _wt, lane) = repo_with_lane();
        std::fs::create_dir_all(lane.join("vendor")).unwrap();
        std::fs::create_dir_all(lane.join("vendor/target")).unwrap();
        std::fs::write(lane.join("vendor/target/keep.txt"), "tracked").unwrap();
        git(&lane, &["add", "-f", "vendor/target/keep.txt"]);
        clear_build_output(&lane);
        assert!(lane.join("vendor/target/keep.txt").exists());
    }

    #[test]
    fn it_does_nothing_outside_a_worktree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        assert_eq!(clear_build_output(dir.path()), 0);
        assert!(dir.path().join("target").exists());
    }

    #[cfg(unix)]
    #[test]
    fn it_never_follows_a_link() {
        let (_repo, _wt, lane) = repo_with_lane();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("precious"), "x").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), lane.join("node_modules")).unwrap();
        clear_build_output(&lane);
        assert!(elsewhere.path().join("precious").exists());
    }
}
