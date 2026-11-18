//! The team branch read model (`team_branch.rs`): pure refusals and parsing, then a temp repo with a bare origin and a
//! fake `gh`.
use super::*;
use crate::test_support::{git, repo_with_origin, run as run_git, write_executable};
use std::path::PathBuf;

fn tb() -> TeamBranch {
    TeamBranch {
        base: "dev".into(),
        main: "main".into(),
        ahead: 2,
        behind: 0,
        main_pr: Some(MainPr {
            number: 5,
            url: "https://github.com/o/r/pull/5".into(),
            mergeable: "MERGEABLE".into(),
            head: "abc".into(),
        }),
        sync_open: false,
        merging: false,
    }
}

#[test]
fn a_project_on_the_default_branch_has_no_team_branch() {
    assert_eq!(need_team_branch("main", Some("main")), Err(NO_TEAM_BRANCH));
    assert_eq!(need_team_branch("dev", None), Err(NO_TEAM_BRANCH));
    assert_eq!(need_team_branch("dev", Some("main")), Ok("main"));
    assert_eq!(
        NO_TEAM_BRANCH,
        "This project works on the repo's default branch, so there's no dev branch to send to it."
    );
}

#[test]
fn git_counts_put_mains_side_first() {
    assert_eq!(parse_ahead_behind("3\t2\n"), Some((2, 3)));
    assert_eq!(parse_ahead_behind("0 0"), Some((0, 0)));
    assert_eq!(parse_ahead_behind(""), None);
    assert_eq!(parse_ahead_behind("1 2 3"), None);
    assert_eq!(parse_ahead_behind("a b"), None);
}

#[test]
fn nothing_refuses_a_ready_pull_request() {
    assert_eq!(ship_refusal(&tb()), None);
}

#[test]
fn the_refusals_come_in_the_ts_order_with_its_words() {
    let mut t = tb();
    t.main_pr = None;
    t.sync_open = true;
    assert_eq!(
        ship_refusal(&t).as_deref(),
        Some("No pull request from dev into main is open.")
    );
    t = tb();
    t.sync_open = true;
    t.merging = true;
    t.main_pr.as_mut().unwrap().mergeable = "CONFLICTING".into();
    assert_eq!(
        ship_refusal(&t).as_deref(),
        Some("Wait for main to finish coming into dev first.")
    );
    t.sync_open = false;
    assert_eq!(
        ship_refusal(&t).as_deref(),
        Some("It conflicts with main: bring main into dev first.")
    );
    t.main_pr.as_mut().unwrap().mergeable = "UNKNOWN".into();
    assert_eq!(
        ship_refusal(&t).as_deref(),
        Some("A slice is going into dev; try again in a moment.")
    );
}

#[test]
fn gh_reason_drops_the_graphql_prefix_and_cuts_at_300() {
    let err = anyhow::anyhow!("gh pr merge 4 failed: GraphQL: Pull request is not mergeable (mergePullRequest)");
    assert_eq!(gh_reason(&err), "Pull request is not mergeable (mergePullRequest)");
    let long = anyhow::anyhow!("x".repeat(400));
    assert_eq!(gh_reason(&long).chars().count(), 300);
}

#[test]
fn items_say_whether_a_sync_is_open_and_whether_one_is_merging() {
    let item = |kind: Option<&str>, status| Item {
        kind: kind.map(str::to_string),
        status,
        ..Item::default()
    };
    assert_eq!(held_in(&[]), (false, false));
    let items = [
        item(Some("sync_main"), ItemStatus::OnMain),
        item(Some("sync_main"), ItemStatus::Skipped),
        item(None, ItemStatus::Todo),
    ];
    assert_eq!(held_in(&items), (false, false));
    let items = [
        item(Some("sync_main"), ItemStatus::InReview),
        item(None, ItemStatus::Merging),
    ];
    assert_eq!(held_in(&items), (true, true));
    // A slice (no chore kind) that is in review is not a sync.
    assert_eq!(held_in(&[item(None, ItemStatus::InReview)]), (false, false));
}

fn commit(repo: &Path, name: &str) {
    std::fs::write(repo.join(name), name).unwrap();
    run_git(git(repo).args(["add", name]));
    run_git(git(repo).args(["commit", "--quiet", "-m", name]));
}

/// origin's `dev` is two commits ahead of and one behind `main`.
fn repo() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    run_git(git(&repo).args(["checkout", "--quiet", "-b", "dev"]));
    commit(&repo, "a.txt");
    commit(&repo, "b.txt");
    run_git(git(&repo).args(["push", "--quiet", "origin", "dev"]));
    run_git(git(&repo).args(["checkout", "--quiet", "main"]));
    commit(&repo, "m.txt");
    run_git(git(&repo).args(["push", "--quiet", "origin", "main"]));
    (root, repo)
}

/// A fake `gh` that logs its arguments and answers `pr list` with `list`.
fn fake_gh(root: &Path, list: &str) -> PathBuf {
    let path = root.join("gh");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> \"{}\"\ncase \"$*\" in \"pr list\"*) echo '{list}';; *) echo broken >&2; exit 1;; esac\n",
        root.join("gh.log").display()
    );
    write_executable(&path, &script);
    path
}

#[test]
fn ahead_and_behind_are_counted_from_origins_branches() {
    let (_root, repo) = repo();
    assert_eq!(ahead_behind(&repo, "dev", "main").unwrap(), (2, 1));
    assert_eq!(ahead_behind(&repo, "main", "dev").unwrap(), (1, 2));
    assert!(ahead_behind(&repo, "nope", "main").is_err());
}

#[test]
fn the_read_has_the_open_pull_request_and_asks_gh_for_it_from_base_into_main() {
    let (root, repo) = repo();
    let gh = fake_gh(
        root.path(),
        r#"[{"number":7,"url":"https://github.com/o/r/pull/7","mergeable":"CONFLICTING","headRefOid":"deadbeef"}]"#,
    );
    let items = [Item {
        kind: Some("sync_main".into()),
        status: ItemStatus::InReview,
        ..Item::default()
    }];
    let got = read(&gh, &repo, "o/r", "dev", "main", &items).unwrap();
    assert_eq!((got.ahead, got.behind), (2, 1));
    assert!(got.sync_open && !got.merging);
    let pr = got.main_pr.unwrap();
    assert_eq!(
        (pr.number, pr.mergeable.as_str(), pr.head.as_str()),
        (7, "CONFLICTING", "deadbeef")
    );
    let log = std::fs::read_to_string(root.path().join("gh.log")).unwrap();
    assert_eq!(
        log.trim(),
        "pr list --repo o/r --base main --head dev --state open --limit 1 --json number,url,mergeable,headRefOid"
    );
}

#[test]
fn no_open_pull_request_reads_as_none_and_a_gh_failure_is_an_error() {
    let (root, repo) = repo();
    let gh = fake_gh(root.path(), "[]");
    assert_eq!(read(&gh, &repo, "o/r", "dev", "main", &[]).unwrap().main_pr, None);
    let failing = root.path().join("fails");
    write_executable(&failing, "#!/bin/sh\necho no >&2\nexit 1\n");
    assert!(read(&failing, &repo, "o/r", "dev", "main", &[]).is_err());
}

#[test]
fn a_sync_or_a_merge_turn_held_here_refuses_the_merge_control_in_the_ts_words() {
    assert_eq!(held_refusal(false, false, "dev", "main"), None);
    assert_eq!(
        held_refusal(true, false, "dev", "main").as_deref(),
        Some("Wait for main to finish coming into dev first.")
    );
    assert_eq!(
        held_refusal(false, true, "dev", "main").as_deref(),
        Some("A slice is going into dev; try again in a moment.")
    );
    // A sync wins, as in `ship_refusal`.
    assert_eq!(
        held_refusal(true, true, "dev", "main"),
        held_refusal(true, false, "dev", "main")
    );
}
