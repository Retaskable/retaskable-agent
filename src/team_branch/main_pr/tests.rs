//! Open PR to main's refusals, draft and `gh` calls (`team_branch/main_pr.rs`): pure tests, then a fake `gh`.
use super::*;
use crate::test_support::write_executable;
use std::path::PathBuf;

fn merged(number: u64, title: &str, branch: &str, at: Option<&str>) -> Merged {
    Merged {
        number,
        title: title.into(),
        branch: branch.into(),
        merged_at: at.map(str::to_string),
    }
}

fn tb() -> TeamBranch {
    TeamBranch {
        base: "dev".into(),
        main: "main".into(),
        ahead: 2,
        behind: 0,
        main_pr: None,
        sync_open: false,
        merging: false,
    }
}

#[test]
fn a_draft_lists_the_slices_oldest_first_without_the_main_syncs() {
    let all = [
        merged(12, "Later slice", "retaskable/b", Some("2026-02-02T00:00:00Z")),
        merged(
            9,
            "A sync",
            "retaskable/sync-main-abc1234",
            Some("2026-02-01T12:00:00Z"),
        ),
        merged(10, "Earlier slice", "retaskable/a", Some("2026-02-01T00:00:00Z")),
    ];
    let d = main_pr_draft("dev", "main", &all, None);
    assert_eq!(d.title, "dev: 2 slices");
    assert_eq!(
        d.body,
        "Everything merged into `dev`, for the team to review.\n\
         \n\
         When it's ready, merge it from Retaskable: it squashes this into `main` as one change, then starts `dev` again from `main`.\n\
         \n\
         ### Slices\n\
         - Earlier slice (#10)\n\
         - Later slice (#12)"
    );
}

#[test]
fn only_what_merged_after_the_last_pull_request_into_main_is_listed() {
    let all = [
        merged(1, "Old", "retaskable/a", Some("2026-01-01T00:00:00Z")),
        merged(2, "At it", "retaskable/b", Some("2026-02-01T00:00:00Z")),
        merged(3, "New", "retaskable/c", Some("2026-02-03T00:00:00Z")),
    ];
    let d = main_pr_draft("dev", "main", &all, Some("2026-02-01T00:00:00Z"));
    assert_eq!(d.title, "dev: 1 slice");
    assert!(d.body.starts_with(
        "Everything merged into `dev` since its last pull request into `main`, for the team to review.\n"
    ));
    assert!(d.body.ends_with("### Slices\n- New (#3)"), "{}", d.body);
}

#[test]
fn a_draft_with_no_slices_says_so() {
    let only_sync = [merged(
        9,
        "A sync",
        "retaskable/sync-main-abc1234",
        Some("2026-02-01T00:00:00Z"),
    )];
    let d = main_pr_draft("dev", "main", &only_sync, None);
    assert_eq!(d.title, "dev: 0 slices");
    assert!(
        d.body
            .ends_with("### Slices\n- (none found: only main syncs, or work merged without a pull request)")
    );
}

#[test]
fn the_refusals_come_in_the_ts_order_with_its_words() {
    let mut t = tb();
    assert_eq!(open_refusal(&t), None);
    t.ahead = 0;
    t.behind = 3;
    t.sync_open = true;
    t.main_pr = Some(MainPr {
        number: 7,
        url: "u".into(),
        mergeable: "MERGEABLE".into(),
        head: "h".into(),
    });
    assert_eq!(
        open_refusal(&t).as_deref(),
        Some("PR #7 from dev into main is already open.")
    );
    t.main_pr = None;
    assert_eq!(
        open_refusal(&t).as_deref(),
        Some("Sync main first: its sync is in flight.")
    );
    // A sync open with nothing behind isn't in the way.
    t.behind = 0;
    assert_eq!(open_refusal(&t).as_deref(), Some("dev has nothing main doesn't."));
}

/// A fake `gh`: logs its arguments (and what `pr create` read on stdin), and answers by the base branch asked for.
fn fake_gh(root: &Path) -> PathBuf {
    let path = root.join("gh");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> \"{log}\"\n\
         case \"$*\" in\n\
         *\"--base main --head dev --state merged\"*) echo '[{{\"number\":5,\"title\":\"Last\",\"headRefName\":\"dev\",\"mergedAt\":\"2026-02-01T00:00:00Z\"}},{{\"number\":4,\"title\":\"Older\",\"headRefName\":\"dev\",\"mergedAt\":\"2026-01-01T00:00:00Z\"}}]';;\n\
         *\"--base dev --state merged\"*) echo '[{{\"number\":3,\"title\":\"Before\",\"headRefName\":\"retaskable/a\",\"mergedAt\":\"2026-01-15T00:00:00Z\"}},{{\"number\":6,\"title\":\"After\",\"headRefName\":\"retaskable/b\",\"mergedAt\":\"2026-02-02T00:00:00Z\"}}]';;\n\
         \"pr create\"*) cat > \"{body}\"; echo 'Creating pull request'; echo https://github.com/o/r/pull/9;;\n\
         *) echo broken >&2; exit 1;;\n\
         esac\n",
        log = root.join("gh.log").display(),
        body = root.join("body.txt").display(),
    );
    write_executable(&path, &script);
    path
}

#[test]
fn the_draft_is_read_since_the_last_pull_request_into_main() {
    let root = tempfile::tempdir().unwrap();
    let gh = fake_gh(root.path());
    let d = draft_main_pr(&gh, root.path(), "o/r", "dev", "main").unwrap();
    assert_eq!(d.title, "dev: 1 slice");
    assert!(d.body.ends_with("### Slices\n- After (#6)"), "{}", d.body);
    let log = std::fs::read_to_string(root.path().join("gh.log")).unwrap();
    let lines: Vec<&str> = log.lines().filter(|l| !l.is_empty()).collect(); // the helper ran the script once
    assert_eq!(
        lines,
        [
            "pr list --repo o/r --base main --head dev --state merged --limit 20 --json number,title,headRefName,mergedAt",
            "pr list --repo o/r --base dev --state merged --limit 200 --json number,title,headRefName,mergedAt",
        ]
    );
}

#[test]
fn creating_sends_the_body_on_stdin_and_answers_the_last_line() {
    let root = tempfile::tempdir().unwrap();
    let gh = fake_gh(root.path());
    let draft = Draft {
        title: "dev: 1 slice".into(),
        body: "the body\n- x (#1)".into(),
    };
    let url = create(&gh, root.path(), "o/r", "dev", "main", &draft).unwrap();
    assert_eq!(url, "https://github.com/o/r/pull/9");
    let log = std::fs::read_to_string(root.path().join("gh.log")).unwrap();
    assert_eq!(
        log.trim(),
        "pr create --repo o/r --base main --head dev --title dev: 1 slice --body-file -"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("body.txt")).unwrap(),
        draft.body
    );
}

#[test]
fn a_gh_failure_is_an_error() {
    let root = tempfile::tempdir().unwrap();
    let gh = root.path().join("gh");
    write_executable(&gh, "#!/bin/sh\necho nope >&2\nexit 1\n");
    assert!(
        create(
            &gh,
            root.path(),
            "o/r",
            "dev",
            "main",
            &Draft {
                title: "t".into(),
                body: "b".into()
            }
        )
        .is_err()
    );
    assert!(draft_main_pr(&gh, root.path(), "o/r", "dev", "main").is_err());
}
