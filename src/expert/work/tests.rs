use serde_json::json;

use super::*;
use crate::test_support::{git, repo_with_origin, run};

fn write(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
}

fn commit(dir: &Path, msg: &str) {
    run(git(dir).args(["add", "."]));
    run(git(dir).args(["commit", "--quiet", "-m", msg]));
}

#[test]
fn the_changed_files_uncommitted_ones_too_and_the_commits_with_the_unpushed_marked() {
    let tmp = tempfile::tempdir().unwrap();
    let lane = repo_with_origin(tmp.path());
    write(&lane, "a.ts", "one\ntwo\n");
    commit(&lane, "base");
    run(git(&lane).args(["push", "--quiet", "origin", "main"]));
    run(git(&lane).args(["checkout", "--quiet", "-b", "retaskable/E1-q"]));
    write(&lane, "a.ts", "one\nTWO\nthree\n");
    commit(&lane, "Change two");
    let branch = "retaskable/E1-q";
    let never = read(&lane, "origin/main", branch).unwrap();
    assert_eq!(
        never.commits.iter().map(|c| c.pushed).collect::<Vec<_>>(),
        [false],
        "never pushed: none of it is"
    );

    run(git(&lane).args(["push", "--quiet", "origin", branch]));
    write(&lane, "b.ts", "b\n");
    commit(&lane, "Add b");
    write(&lane, "a.ts", "one\nTWO\n");

    let work = read(&lane, "origin/main", branch).unwrap();
    let files: Vec<(&str, u64, u64)> = work
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.added, f.removed))
        .collect();
    assert_eq!(
        files,
        [("a.ts", 1, 1), ("b.ts", 1, 0)],
        "a.ts's uncommitted change counts too"
    );
    let commits: Vec<(&str, bool)> = work.commits.iter().map(|c| (c.subject.as_str(), c.pushed)).collect();
    assert_eq!(commits, [("Add b", false), ("Change two", true)]);
    assert_eq!(work.more, 0);
}

#[test]
fn a_hundred_files_are_listed_and_the_rest_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let lane = repo_with_origin(tmp.path());
    run(git(&lane).args(["checkout", "--quiet", "-b", "b"]));
    for i in 0..105 {
        write(&lane, &format!("f{i:03}.txt"), "x\n");
    }
    commit(&lane, "many");
    let work = read(&lane, "origin/main", "b").unwrap();
    assert_eq!((work.files.len(), work.more), (100, 5));
}

fn pr(number: u64, state: &str, rollup: Value, auto: Value) -> Value {
    json!({ "number": number, "title": format!("PR {number}"), "url": format!("https://x/{number}"), "state": state,
        "headRefName": "s", "statusCheckRollup": rollup, "autoMergeRequest": auto })
}

#[test]
fn a_pull_requests_state_checks_and_auto_merge_are_read_from_the_row() {
    let of = |v: Value| pr_of(&v, None).unwrap();
    assert_eq!(of(pr(1, "OPEN", json!([]), Value::Null)).checks, "none");
    assert_eq!(of(pr(1, "OPEN", Value::Null, Value::Null)).checks, "none");
    let passed = json!([{ "status": "COMPLETED", "conclusion": "SUCCESS" }, { "state": "SUCCESS" }]);
    assert_eq!(of(pr(1, "OPEN", passed, Value::Null)).checks, "passed");
    let running = json!([{ "status": "IN_PROGRESS" }, { "status": "COMPLETED", "conclusion": "SUCCESS" }]);
    assert_eq!(of(pr(1, "OPEN", running, Value::Null)).checks, "running");
    let pending = json!([{ "state": "PENDING" }]);
    assert_eq!(of(pr(1, "OPEN", pending, Value::Null)).checks, "running");
    let failed = json!([{ "status": "IN_PROGRESS" }, { "status": "COMPLETED", "conclusion": "FAILURE" }]);
    let p = of(pr(2, "MERGED", failed, json!({ "mergeMethod": "SQUASH" })));
    assert_eq!((p.state, p.checks, p.auto_merge), ("merged", "failed", true));
    assert_eq!(of(pr(3, "CLOSED", json!([]), Value::Null)).state, "closed");
    assert_eq!(pr_of(&json!({ "title": "no number" }), None), None);
}

#[cfg(unix)]
#[test]
fn the_sessions_and_each_helpers_pull_requests_newest_first_helpers_marked_at_most_ten() {
    let tmp = tempfile::tempdir().unwrap();
    let rows = |branch: &str| match branch {
        "s" => json!([
            pr(3, "MERGED", json!([]), Value::Null),
            pr(1, "CLOSED", json!([]), Value::Null)
        ]),
        "s-h2" => json!([pr(4, "OPEN", json!([{ "conclusion": "FAILURE" }]), json!({}))]),
        "many" => json!(
            (1..=12)
                .map(|n| pr(n, "OPEN", json!([]), Value::Null))
                .collect::<Vec<_>>()
        ),
        _ => json!([]),
    };
    let gh = tmp.path().join("gh");
    let mut script = String::from("#!/bin/sh\n[ $# -eq 0 ] && exit 0\necho \"$@\" >> calls.txt\ncase \"$4\" in\n");
    for b in ["s", "s-h1", "s-h2", "many"] {
        script.push_str(&format!("{b}) cat <<'EOF'\n{}\nEOF\n;;\n", rows(b)));
    }
    script.push_str("esac\n");
    // Written and probed by the shared helper, which waits out "Text file busy" (a write racing another test's fork).
    crate::test_support::write_executable(&gh, &script);

    let got = prs(&gh, tmp.path(), "s", 2).unwrap();
    let shown: Vec<(u64, &str, Option<u32>)> = got.iter().map(|p| (p.number, p.state, p.helper)).collect();
    assert_eq!(shown, [(4, "open", Some(2)), (3, "merged", None), (1, "closed", None)]);
    let calls = std::fs::read_to_string(tmp.path().join("calls.txt")).unwrap();
    let heads: Vec<&str> = calls.lines().map(|l| l.split(' ').nth(3).unwrap()).collect();
    assert_eq!(heads, ["s", "s-h1", "s-h2"]);

    let many = prs(&gh, tmp.path(), "many", 0).unwrap();
    assert_eq!(
        many.iter().map(|p| p.number).collect::<Vec<_>>(),
        [12, 11, 10, 9, 8, 7, 6, 5, 4, 3]
    );
}

#[test]
fn the_summary_as_the_snapshot_carries_it() {
    let w = Work {
        files: vec![FileChange {
            path: "a.ts".into(),
            added: 1,
            removed: 2,
        }],
        more: 4,
        commits: vec![Commit {
            sha: "abc".into(),
            subject: "s".into(),
            pushed: true,
        }],
        prs: vec![pr_of(&pr(5, "OPEN", json!([]), Value::Null), Some(1)).unwrap()],
    };
    let j = w.to_json();
    assert_eq!(j["files"][0], json!({ "path": "a.ts", "added": 1, "removed": 2 }));
    assert_eq!(
        (j["more"].as_u64(), j["commits"][0]["pushed"].as_bool()),
        (Some(4), Some(true))
    );
    assert_eq!(j["prs"][0]["helper"], 1);
    assert!(w.has_open_pr());
    assert!(Work::default().to_json().get("prs").is_none() && Work::default().to_json().get("more").is_none());
}
