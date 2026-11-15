//! The merge control into main (`team_branch/ship.rs`) against a temp repo with a REAL bare origin and a fake `gh`.
//! The fake `gh` answers from files in the test's folder and runs a step script on a chosen call, which is how a test
//! makes the world change at the moment that matters (the dev branch deleted or moved once the merge went in). Nothing
//! sleeps: a hook on origin's upload-pack moves the dev branch after the restart's `ls-remote` and before its push.
use super::*;
use crate::backlog::ItemStatus;
use crate::test_support::{git, repo_with_origin, run as run_git, write_executable};
use std::path::PathBuf;

struct Fx {
    root: tempfile::TempDir,
    repo: PathBuf,
    origin: PathBuf,
    /// origin's `dev` when the pull request was read, its `main`, and a commit on top of dev that lands later.
    dev: String,
    main: String,
    spare: String,
}

fn commit(repo: &Path, name: &str) {
    std::fs::write(repo.join(name), name).unwrap();
    run_git(git(repo).args(["add", name]));
    run_git(git(repo).args(["commit", "--quiet", "-m", name]));
}

fn sha_in(dir: &Path, rev: &str) -> String {
    let out = git(dir).args(["rev-parse", rev]).output().unwrap();
    assert!(out.status.success(), "rev-parse {rev}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

impl Fx {
    fn at(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.at(name), text).unwrap();
    }

    /// Where origin has `branch` now, or none.
    fn origin_has(&self, branch: &str) -> Option<String> {
        let out = git(&self.origin)
            .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8(out.stdout).unwrap().trim().to_string())
    }

    fn log(&self) -> Vec<String> {
        let text = std::fs::read_to_string(self.at("gh.log")).unwrap_or_default();
        text.lines().filter(|l| !l.is_empty()).map(str::to_string).collect()
    }

    fn ran(&self, verb: &str) -> bool {
        self.log().iter().any(|l| l.starts_with(verb))
    }

    /// A step the fake `gh` runs (with `sh`) on a call, before it answers.
    fn step(&self, name: &str, script: &str) {
        self.write(name, script);
    }

    fn ship(&self, items: &[Item]) -> Result<Option<String>, Stop> {
        ship(&self.at("gh"), &self.repo, "o/r", "dev", "main", items)
    }

    /// The pull request's JSON, read at `head`.
    fn list(&self, mergeable: &str, head: &str) {
        let head = if head.is_empty() {
            String::new()
        } else {
            format!(r#","headRefOid":"{head}""#)
        };
        self.write(
            "list.json",
            &format!(r#"[{{"number":7,"url":"https://github.com/o/r/pull/7","mergeable":"{mergeable}"{head}}}]"#),
        );
    }
}

/// origin's `dev` is two commits ahead of and one behind `main`, with a pull request 7 open from dev into main.
fn fx() -> Fx {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    run_git(git(&repo).args(["checkout", "--quiet", "-b", "dev"]));
    commit(&repo, "a.txt");
    commit(&repo, "b.txt");
    run_git(git(&repo).args(["push", "--quiet", "origin", "dev"]));
    commit(&repo, "late.txt");
    run_git(git(&repo).args(["push", "--quiet", "origin", "dev:spare"]));
    run_git(git(&repo).args(["reset", "--quiet", "--hard", "HEAD~1"]));
    run_git(git(&repo).args(["checkout", "--quiet", "main"]));
    commit(&repo, "m.txt");
    run_git(git(&repo).args(["push", "--quiet", "origin", "main"]));
    let origin = root.path().join("origin.git");
    let (dev, main, spare) = (
        sha_in(&origin, "refs/heads/dev"),
        sha_in(&origin, "refs/heads/main"),
        sha_in(&origin, "refs/heads/spare"),
    );
    let fx = Fx {
        root,
        repo,
        origin,
        dev: dev.clone(),
        main,
        spare,
    };
    fx.list("MERGEABLE", &dev);
    fx.write("view.json", &format!(r#"{{"state":"OPEN","headRefOid":"{dev}"}}"#));
    fx.write("view2.json", &format!(r#"{{"state":"MERGED","headRefOid":"{dev}"}}"#));
    let script = format!(
        r#"#!/bin/sh
R='{root}'
echo "$*" >> "$R/gh.log"
case "$*" in
"pr list"*"--state merged"*) cat "$R/merged.json" 2>/dev/null || echo '[]';;
"pr list"*) cat "$R/list.json";;
"pr view"*)
  n=$(cat "$R/views" 2>/dev/null || echo 0); n=$((n+1)); echo $n > "$R/views"
  [ -f "$R/view$n.sh" ] && sh "$R/view$n.sh"
  if [ -f "$R/view$n.fail" ]; then echo gone >&2; exit 1; fi
  cat "$R/view$n.json" 2>/dev/null || cat "$R/view.json";;
"pr merge"*)
  [ -f "$R/merge.sh" ] && sh "$R/merge.sh"
  if [ -f "$R/merge.fail" ]; then cat "$R/merge.fail" >&2; exit 1; fi;;
*) echo "unexpected: $*" >&2; exit 1;;
esac
"#,
        root = fx.root.path().display()
    );
    write_executable(&fx.at("gh"), &script);
    fx
}

fn item(kind: Option<&str>, status: ItemStatus) -> Item {
    Item {
        kind: kind.map(str::to_string),
        status,
        ..Item::default()
    }
}

fn refused(r: Result<Option<String>, Stop>) -> String {
    match r {
        Err(Stop::Refused(why)) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn says(r: Result<Option<String>, Stop>) -> String {
    match r {
        Ok(Some(says)) => says,
        other => panic!("expected a warning, got {other:?}"),
    }
}

#[test]
fn it_squashes_the_pull_request_as_read_then_starts_dev_again_from_main() {
    let fx = fx();
    assert_eq!(fx.ship(&[]).unwrap(), None);
    // The dev branch now is main, on origin; main itself was never written.
    assert_eq!(fx.origin_has("dev"), Some(fx.main.clone()));
    assert_eq!(fx.origin_has("main"), Some(fx.main.clone()));
    let head = &fx.dev;
    assert_eq!(
        fx.log(),
        [
            "pr list --repo o/r --base main --head dev --state open --limit 1 --json number,url,mergeable,headRefOid"
                .to_string(),
            "pr view 7 --repo o/r --json state,headRefOid".into(),
            format!("pr merge 7 --repo o/r --squash --match-head-commit {head}"),
            "pr view 7 --repo o/r --json state,headRefOid".into(),
        ]
    );
    assert!(!fx.log().iter().any(|l| l.contains("--delete-branch")));
}

#[test]
fn a_branch_github_deleted_is_created_again() {
    let fx = fx();
    // GitHub's "delete head branches" took it when the pull request merged.
    fx.step(
        "merge.sh",
        &format!("git --git-dir='{}' update-ref -d refs/heads/dev\n", fx.origin.display()),
    );
    assert_eq!(fx.ship(&[]).unwrap(), None);
    assert_eq!(fx.origin_has("dev"), Some(fx.main.clone()));
}

#[test]
fn work_pushed_to_dev_after_the_read_is_left_alone() {
    let fx = fx();
    fx.step(
        "merge.sh",
        &format!(
            "git --git-dir='{}' update-ref refs/heads/dev {}\n",
            fx.origin.display(),
            fx.spare
        ),
    );
    let said = says(fx.ship(&[]));
    assert_eq!(
        said,
        "dev has new work since its pull request was read, so it wasn't started again from main."
    );
    assert_eq!(fx.origin_has("dev"), Some(fx.spare.clone()), "no push");
    assert_eq!(fx.origin_has("main"), Some(fx.main.clone()));
}

#[test]
fn a_lease_that_fails_because_dev_moved_between_the_read_and_the_push_leaves_dev_untouched() {
    let fx = fx();
    // After the merge's own look, the next two upload-pack runs on origin are the restart's fetch and its ls-remote;
    // origin's dev moves as that second one ends, so the push's lease names a sha that is no longer there.
    let up = fx.at("up.sh");
    write_executable(
        &up,
        &format!(
            r#"#!/bin/sh
R='{root}'
git upload-pack "$@"
rc=$?
if [ -f "$R/arm" ]; then
  echo x >> "$R/seen"
  if [ "$(wc -l < "$R/seen")" -ge 2 ]; then
    git --git-dir='{origin}' update-ref refs/heads/dev {spare}
    rm "$R/arm"
  fi
fi
exit $rc
"#,
            root = fx.root.path().display(),
            origin = fx.origin.display(),
            spare = fx.spare
        ),
    );
    run_git(git(&fx.repo).args(["config", "remote.origin.uploadpack", up.to_str().unwrap()]));
    fx.step("view2.sh", &format!("touch '{}'\n", fx.at("arm").display()));
    let said = says(fx.ship(&[]));
    assert!(said.starts_with("Couldn't start dev again from main: "), "{said}");
    assert!(said.contains("stale info"), "{said}");
    assert_eq!(
        fx.origin_has("dev"),
        Some(fx.spare.clone()),
        "dev is as the other push left it"
    );
    assert_eq!(fx.origin_has("main"), Some(fx.main.clone()));
}

#[test]
fn a_merge_that_was_only_queued_leaves_dev_alone() {
    let fx = fx();
    fx.write(
        "view2.json",
        &format!(r#"{{"state":"OPEN","headRefOid":"{}"}}"#, fx.dev),
    );
    assert_eq!(
        says(fx.ship(&[])),
        "PR #7 didn't go in straight away (GitHub may have queued it), so dev was left as it is."
    );
    assert_eq!(fx.origin_has("dev"), Some(fx.dev.clone()));
}

#[test]
fn a_look_after_the_merge_that_fails_twice_reads_as_not_merged() {
    let fx = fx();
    fx.write("view2.fail", "");
    fx.write("view3.fail", "");
    assert!(says(fx.ship(&[])).starts_with("PR #7 didn't go in straight away"));
    assert_eq!(fx.origin_has("dev"), Some(fx.dev.clone()));
}

#[test]
fn a_look_after_the_merge_that_fails_once_is_asked_again() {
    let fx = fx();
    fx.write("view2.fail", "");
    fx.write(
        "view3.json",
        &format!(r#"{{"state":"MERGED","headRefOid":"{}"}}"#, fx.dev),
    );
    assert_eq!(fx.ship(&[]).unwrap(), None);
    assert_eq!(fx.origin_has("dev"), Some(fx.main.clone()));
}

/// `gh pr list --state merged` says pull request 7 from dev into main merged with `head`.
fn merged_at(fx: &Fx, head: &str) {
    fx.write(
        "merged.json",
        &format!(r#"[{{"number":6,"mergedAt":"2026-01-01T00:00:00Z","headRefOid":"old"}},{{"number":7,"mergedAt":"2026-02-01T00:00:00Z","headRefOid":"{head}"}}]"#),
    );
}

fn unfinished(fx: &Fx) -> Option<(u64, String)> {
    crate::lanes::fetch_latest(&fx.repo).unwrap();
    unfinished_restart(&fx.at("gh"), &fx.repo, "o/r", "dev", "main").unwrap()
}

#[test]
fn a_squashed_dev_branch_that_was_never_restarted_is_found_and_restarted_once() {
    let fx = fx();
    merged_at(&fx, &fx.dev);
    let (number, head) = unfinished(&fx).expect("dev still stands at the head that merged");
    assert_eq!((number, head.as_str()), (7, fx.dev.as_str()));
    assert_eq!(start_again_from_main(&fx.repo, "dev", "main", &head), None);
    assert_eq!(fx.origin_has("dev"), Some(fx.main.clone()));
    assert_eq!(fx.origin_has("main"), Some(fx.main.clone()), "main was never written");
    // Done: nothing more to finish, and `gh` isn't even asked.
    let asked = fx.log().len();
    assert_eq!(unfinished(&fx), None);
    assert_eq!(fx.log().len(), asked);
}

#[test]
fn a_dev_branch_that_moved_on_since_the_merge_is_not_a_restart_to_finish() {
    let fx = fx();
    merged_at(&fx, "somethingelse");
    assert_eq!(unfinished(&fx), None);
    assert_eq!(fx.origin_has("dev"), Some(fx.dev.clone()));
    // No merged pull request at all is nothing to finish either.
    let fx = self::fx();
    assert_eq!(unfinished(&fx), None);
}

#[test]
fn a_fetch_that_fails_says_so_and_pushes_nothing() {
    let fx = fx();
    fx.step(
        "merge.sh",
        &format!(
            "git -C '{}' remote set-url origin '{}'\n",
            fx.repo.display(),
            fx.at("nowhere").display()
        ),
    );
    let said = says(fx.ship(&[]));
    assert!(said.starts_with("Couldn't fetch main to start dev again: "), "{said}");
    assert_eq!(fx.origin_has("dev"), Some(fx.dev.clone()));
}

#[test]
fn each_refusal_comes_with_the_ts_words_before_anything_is_merged() {
    let sync = [item(Some("sync_main"), ItemStatus::InReview)];
    let merging = [item(None, ItemStatus::Merging)];
    let cases: [(&str, &str, &[Item], &str); 4] = [
        ("[]", "", &[], "No pull request from dev into main is open."),
        ("", "MERGEABLE", &sync, "Wait for main to finish coming into dev first."),
        (
            "",
            "CONFLICTING",
            &[],
            "It conflicts with main: bring main into dev first.",
        ),
        (
            "",
            "MERGEABLE",
            &merging,
            "A slice is going into dev; try again in a moment.",
        ),
    ];
    for (list, mergeable, items, why) in cases {
        let fx = fx();
        if !list.is_empty() {
            fx.write("list.json", list);
        } else {
            fx.list(mergeable, &fx.dev);
        }
        assert_eq!(refused(fx.ship(items)), why);
        assert!(!fx.ran("pr view") && !fx.ran("pr merge"), "{:?}", fx.log());
        assert_eq!(fx.origin_has("dev"), Some(fx.dev.clone()));
    }
}

#[test]
fn a_pull_request_that_is_not_open_any_more_is_refused() {
    let fx = fx();
    fx.write(
        "view.json",
        &format!(r#"{{"state":"CLOSED","headRefOid":"{}"}}"#, fx.dev),
    );
    assert_eq!(refused(fx.ship(&[])), "PR #7 isn't open any more.");
    assert!(!fx.ran("pr merge"));
}

#[test]
fn a_gh_merge_failure_is_refused_in_githubs_words() {
    let fx = fx();
    fx.write(
        "merge.fail",
        "GraphQL: Head branch was modified. Review and try the merge again. (mergePullRequest)",
    );
    assert_eq!(
        refused(fx.ship(&[])),
        "GitHub didn't merge PR #7: Head branch was modified. Review and try the merge again. (mergePullRequest)"
    );
    assert_eq!(
        fx.log().iter().filter(|l| l.starts_with("pr view")).count(),
        1,
        "no look after"
    );
    assert_eq!(fx.origin_has("dev"), Some(fx.dev.clone()));
}

#[test]
fn a_gh_that_cannot_answer_is_a_failure_not_a_refusal() {
    let fx = fx();
    write_executable(&fx.at("broken"), "#!/bin/sh\necho no >&2\nexit 1\n");
    let out = ship(&fx.at("broken"), &fx.repo, "o/r", "dev", "main", &[]);
    assert!(matches!(out, Err(Stop::Failed(_))), "{out:?}");
}

#[test]
fn a_head_missing_from_the_list_falls_back_to_the_one_the_view_read() {
    let fx = fx();
    fx.list("MERGEABLE", "");
    assert_eq!(fx.ship(&[]).unwrap(), None);
    assert!(fx.log().contains(&format!(
        "pr merge 7 --repo o/r --squash --match-head-commit {}",
        fx.dev
    )));
}

#[test]
fn the_restart_pushes_only_the_base_branch_leased_on_what_was_read() {
    assert_eq!(
        push_args("dev", "main", Some("abc123")),
        [
            "push",
            "--force-with-lease=refs/heads/dev:abc123",
            "origin",
            "refs/remotes/origin/main:refs/heads/dev"
        ]
    );
    // Creating leases on nothing, so a branch that appeared since is not overwritten.
    assert_eq!(push_args("dev", "main", None)[1], "--force-with-lease=refs/heads/dev:");
    for args in [push_args("dev", "main", None), push_args("dev", "main", Some("a"))] {
        assert!(!args.iter().any(|a| a == "--force" || a == "-f"));
        assert!(
            args.last().unwrap().ends_with(":refs/heads/dev"),
            "never main as the destination"
        );
    }
}

#[test]
fn the_restart_plan_and_the_not_merged_note() {
    assert_eq!(restart_plan(None, "a"), Restart::Create);
    assert_eq!(restart_plan(Some("a"), "a"), Restart::Reset);
    assert_eq!(restart_plan(Some("b"), "a"), Restart::Moved);
    assert_eq!(not_merged_note("MERGED", 7, "dev"), None);
    assert!(not_merged_note("UNKNOWN", 7, "dev").is_some());
}
