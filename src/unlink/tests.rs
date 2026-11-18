use std::path::Path;
use std::process::Command;

use serde_json::json;

use super::*;

fn config(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap()).unwrap()
}

#[test]
fn the_projects_entry_goes_and_the_rest_of_the_file_stays() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.json"),
        json!({ "version": 2, "lanes": 3, "projects": [{ "id": "one", "repoPath": "/a" }, { "id": "two", "repoPath": "/b" }] })
            .to_string(),
    )
    .unwrap();
    remove_from_config(dir.path(), "one").unwrap();
    let kept = config(dir.path());
    assert_eq!(kept["lanes"], 3);
    assert_eq!(kept["projects"], json!([{ "id": "two", "repoPath": "/b" }]));
    let again = remove_from_config(dir.path(), "one").unwrap_err();
    assert!(again.to_string().contains("one isn't in config.json"), "{again}");
}

#[test]
fn the_state_folder_goes_and_a_missing_one_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::paths::project_dir(dir.path(), "one");
    std::fs::create_dir_all(state.join("runs")).unwrap();
    std::fs::write(state.join("state.json"), "{}").unwrap();
    remove_state(dir.path(), "one").unwrap();
    assert!(!state.exists());
    remove_state(dir.path(), "one").unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").current_dir(dir).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn every_lane_and_checkout_goes_with_the_worktrees_folder_and_the_repo_stays() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "first",
        ],
    );
    let wt = root.path().join("worktrees");
    lanes::ensure(&repo, &wt, 2).unwrap();
    // A folder git never tracked, as a failed remove leaves one, and the base checkout.
    std::fs::create_dir_all(wt.join("merge-p1")).unwrap();
    std::fs::write(wt.join("merge-p1").join("left.txt"), "x").unwrap();
    git(
        &repo,
        &["worktree", "add", "--detach", wt.join("_planner").to_str().unwrap()],
    );
    remove_lanes(&repo, &wt).unwrap();
    assert!(!wt.exists(), "{:?}", std::fs::read_dir(&wt).map(|d| d.count()));
    assert!(repo.join(".git").exists());
    let listed = Command::new("git")
        .current_dir(&repo)
        .args(["worktree", "list"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&listed.stdout).lines().count(), 1);
    // Nothing there to remove is fine.
    remove_lanes(&repo, &wt).unwrap();
}

#[test]
fn something_else_in_the_worktrees_folder_is_left_and_so_is_the_folder() {
    let root = tempfile::tempdir().unwrap();
    let wt = root.path().join("worktrees");
    std::fs::create_dir_all(wt.join("notes")).unwrap();
    remove_lanes(root.path(), &wt).unwrap();
    assert!(wt.join("notes").exists());
}
