use std::path::Path;

use super::*;
use crate::agent_ci::report::Kind;
use crate::agent_ci::store::tests::FakeStore;
use crate::test_support::write_executable;

/// A `gh` that logs each call and answers from files in its directory: `pull.json` (the pull request),
/// `open.json` (the open list), `compare.json`, `closed.json` and `<sha>.json` (statuses); `merge.fail` makes
/// the merge fail.
fn host(dir: &Path) -> Arc<Host> {
    let program = dir.join("fake");
    write_executable(
        &program,
        "#!/bin/sh\n[ -n \"$1\" ] || exit 0\nprintf 'CALL %s\\n' \"$*\" >> calls.log\n\
         case \"$4\" in\n\
         *pulls/*/merge) [ -e merge.fail ] && { echo 'Head branch was modified' >&2; exit 1; }; echo '{}';;\n\
         *compare/*) cat compare.json;;\n\
         *pulls?state=open*) cat open.json;;\n\
         *pulls?state=closed*) cat closed.json;;\n\
         *pulls/*) cat pull.json;;\n\
         *commits/*/statuses*) sha=${4#*commits/}; sha=${sha%%/*}; cat \"$sha.json\" 2>/dev/null || echo '[]';;\n\
         esac\n",
    );
    for (name, body) in [
        ("compare.json", r#"{"behind_by":0}"#),
        ("open.json", "[]"),
        ("closed.json", "[]"),
    ] {
        if !dir.join(name).exists() {
            std::fs::write(dir.join(name), body).unwrap();
        }
    }
    Arc::new(Host {
        program,
        kind: Kind::Github,
        repo: "o/r".into(),
        dir: dir.to_path_buf(),
    })
}

fn pull(dir: &Path, state: &str, sha: &str, branch: &str, labels: &[&str]) {
    let labels: Vec<_> = labels.iter().map(|n| json!({ "name": n })).collect();
    let v = json!({ "state": state, "head": { "sha": sha, "ref": branch }, "labels": labels });
    std::fs::write(dir.join("pull.json"), v.to_string()).unwrap();
}

fn calls(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("calls.log")).unwrap_or_default()
}

fn merges(dir: &Path) -> Vec<String> {
    calls(dir)
        .lines()
        .filter(|l| l.contains("/merge"))
        .map(String::from)
        .collect()
}

fn am() -> (Automerge, Arc<FakeStore>) {
    let store = FakeStore::winning();
    (Automerge::new(store.clone()), store)
}

#[tokio::test]
async fn a_labelled_pull_request_is_squash_merged_pinned_to_its_head_and_the_store_is_told() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    pull(d.path(), "open", "h7", "fix", &["automerge"]);
    let (am, store) = am();
    assert_eq!(am.run(&h, "main", 7, "h7").await, Ended::Merged);
    let m = merges(d.path());
    assert_eq!(m.len(), 1);
    assert!(m[0].contains("-X PUT repos/o/r/pulls/7/merge"), "{m:?}");
    assert!(m[0].contains("-f merge_method=squash") && m[0].contains("-f sha=h7"));
    assert_eq!(
        store.args("ci_merged"),
        [json!({ "sha": "h7", "context": "ci/agent/pr/ci", "pr": 7 })]
    );
}

#[tokio::test]
async fn a_persons_merge_request_counts_like_the_label() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    pull(d.path(), "open", "h7", "fix", &[]);
    let store = FakeStore::answering(|op, _| {
        Ok(match op {
            "ci_merge_requests" => json!({ "requests": [{ "pr": 7, "sha": "h7" }] }),
            _ => json!({ "ok": true }),
        })
    });
    let am = Automerge::new(store);
    assert_eq!(am.run(&h, "main", 7, "h7").await, Ended::NotAsked);
    am.refresh_asked("o/r").await;
    assert!(am.is_asked(7, "h7") && !am.is_asked(7, "other"));
    assert_eq!(am.run(&h, "main", 7, "h7").await, Ended::Merged);
}

#[tokio::test]
async fn a_pull_request_that_no_longer_asks_is_not_merged() {
    for (state, sha, labels) in [
        ("open", "h7", &["ci-ready"][..]),
        ("open", "other", &["automerge"]),
        ("closed", "h7", &["automerge"]),
    ] {
        let d = tempfile::tempdir().unwrap();
        let h = host(d.path());
        pull(d.path(), state, sha, "fix", labels);
        let (am, store) = am();
        assert_eq!(am.run(&h, "main", 7, "h7").await, Ended::NotAsked, "{state} {sha}");
        assert!(merges(d.path()).is_empty());
        assert!(store.args("ci_merged").is_empty());
    }
}

#[tokio::test]
async fn a_failed_merge_says_why_and_tells_the_store_nothing() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    pull(d.path(), "open", "h7", "fix", &["automerge"]);
    std::fs::write(d.path().join("merge.fail"), "").unwrap();
    let (am, store) = am();
    assert!(matches!(am.run(&h, "main", 7, "h7").await, Ended::Failed(w) if w.contains("Head branch was modified")));
    assert!(store.args("ci_merged").is_empty());
}

fn agent_pr(dir: &Path, behind: u64, ci: &str) {
    let open = json!([
        { "number": 3, "head": { "sha": "agent", "ref": "retaskable/x" } },
        { "number": 7, "head": { "sha": "h7", "ref": "fix" } }
    ]);
    std::fs::write(dir.join("open.json"), open.to_string()).unwrap();
    std::fs::write(dir.join("compare.json"), json!({ "behind_by": behind }).to_string()).unwrap();
    let s = json!([{ "state": ci, "context": "ci/agent/pr/ci", "description": "agent-ci x: running" }]);
    std::fs::write(dir.join("agent.json"), s.to_string()).unwrap();
}

#[tokio::test]
async fn a_persons_pull_request_waits_while_an_up_to_date_agent_pr_is_checking() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    pull(d.path(), "open", "h7", "fix", &["automerge"]);
    agent_pr(d.path(), 0, "pending");
    let (am, store) = am();
    assert_eq!(am.run(&h, "main", 7, "h7").await, Ended::Held(3));
    assert!(merges(d.path()).is_empty());
    assert!(calls(d.path()).contains("repos/o/r/compare/main...agent"));
    // The agent's check is done: the next poll merges.
    agent_pr(d.path(), 0, "success");
    assert_eq!(am.run(&h, "main", 7, "h7").await, Ended::Merged);
    assert_eq!(store.args("ci_merged").len(), 1);
}

#[tokio::test]
async fn an_agent_pr_behind_main_holds_no_one_and_an_agent_pr_never_waits() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    pull(d.path(), "open", "h7", "fix", &["automerge"]);
    agent_pr(d.path(), 2, "pending");
    assert_eq!(am().0.run(&h, "main", 7, "h7").await, Ended::Merged);
    // A retaskable/ pull request itself is never held.
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    pull(d.path(), "open", "h7", "retaskable/y", &["automerge"]);
    agent_pr(d.path(), 0, "pending");
    assert_eq!(am().0.run(&h, "main", 7, "h7").await, Ended::Merged);
}

#[tokio::test]
async fn the_wait_ends_after_the_hold() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    pull(d.path(), "open", "h7", "fix", &["automerge"]);
    agent_pr(d.path(), 0, "pending");
    let (mut am, _) = am();
    am.hold = Duration::from_millis(30);
    assert_eq!(am.run(&h, "main", 7, "h7").await, Ended::Held(3));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(am.run(&h, "main", 7, "h7").await, Ended::Merged);
}

#[tokio::test]
async fn closed_pull_requests_are_told_once_and_forks_never() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    let closed = json!([
        { "number": 4, "merged_at": "2026-01-01T00:00:00Z", "head": { "repo": { "full_name": "o/r" } } },
        { "number": 5, "merged_at": null, "head": { "repo": { "full_name": "o/r" } } },
        { "number": 6, "merged_at": null, "head": { "repo": { "full_name": "fork/r" } } }
    ]);
    std::fs::write(d.path().join("closed.json"), closed.to_string()).unwrap();
    let (am, store) = am();
    am.tell_closed(&h).await;
    am.tell_closed(&h).await;
    assert_eq!(
        store.args("ci_pr_closed"),
        [json!({ "pr": 4, "merged": true }), json!({ "pr": 5, "merged": false })]
    );
}

#[tokio::test]
async fn a_closed_pull_request_the_store_refused_is_told_again_next_time() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    let closed = json!([{ "number": 4, "merged_at": null, "head": { "repo": { "full_name": "o/r" } } }]);
    std::fs::write(d.path().join("closed.json"), closed.to_string()).unwrap();
    let store = FakeStore::answering(|_, _| Err("down".into()));
    let am = Automerge::new(store.clone());
    am.tell_closed(&h).await;
    am.tell_closed(&h).await;
    assert_eq!(store.args("ci_pr_closed").len(), 2);
}
