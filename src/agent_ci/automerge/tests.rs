use std::path::Path;

use super::*;
use crate::agent_ci::report::Kind;
use crate::agent_ci::store::tests::FakeStore;
use crate::agent_ci::timing::CiTiming;
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

/// An `Automerge` that has read the store's timing (the defaults) for the host's repo, `o/r`, and, from its answer, `requests`.
async fn am(requests: Vec<Value>) -> (Automerge, Arc<FakeStore>) {
    let store = FakeStore::winning();
    store.ask(requests);
    let am = Automerge::new(store.clone());
    am.refresh_requests("o/r", json!([]), &[]).await;
    (am, store)
}

fn ask(pr: u64, sha: &str, by: &str, merge: bool) -> Value {
    json!({ "pr": pr, "sha": sha, "by": by, "merge": merge })
}

#[tokio::test]
async fn a_requested_pull_request_is_squash_merged_pinned_to_its_head_and_the_store_is_told() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    pull(d.path(), "open", "h7", "fix", &[]);
    let (am, store) = am(vec![ask(7, "h7", "automerge", true)]).await;
    assert_eq!(am.run(&h, 7, "h7").await, Ended::Merged);
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
async fn nothing_merges_without_the_stores_request_whatever_the_pull_request_carries() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    // The label is on it, but the runner holds no label rule: the store did not ask.
    pull(d.path(), "open", "h7", "fix", &["automerge"]);
    let (am, store) = am(vec![ask(8, "h8", "automerge", true), ask(7, "other", "ana", true)]).await;
    assert_eq!(am.run(&h, 7, "h7").await, Ended::NotAsked);
    assert!(merges(d.path()).is_empty() && store.args("ci_merged").is_empty());
    assert_eq!(am.request(7, "h7"), None);
}

#[tokio::test]
async fn a_request_with_merge_false_waits_until_the_store_says_merge_and_the_runner_holds_no_wait_of_its_own() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    pull(d.path(), "open", "h7", "fix", &[]);
    let store = FakeStore::winning();
    let am = Automerge::new(store.clone());
    store.ask(vec![
        json!({ "pr": 7, "sha": "h7", "by": "ana", "merge": false, "holdUntil": 123_456 }),
    ]);
    am.refresh_requests("o/r", json!([]), &[3]).await;
    assert_eq!(am.run(&h, 7, "h7").await, Ended::Held(Some(123_456)));
    assert!(merges(d.path()).is_empty());
    // The turn is over: the store answers merge, and only then it is merged.
    store.ask(vec![ask(7, "h7", "ana", true)]);
    am.refresh_requests("o/r", json!([]), &[]).await;
    assert_eq!(am.run(&h, 7, "h7").await, Ended::Merged);
    assert_eq!(merges(d.path()).len(), 1);
}

#[tokio::test]
async fn a_poll_tells_the_store_the_facts_and_the_turn() {
    let store = FakeStore::winning();
    let am = Automerge::new(store.clone());
    let facts = json!([{ "pr": 7, "sha": "h7", "labels": ["automerge"], "approvals": 1 }]);
    am.refresh_requests("o/r", facts.clone(), &[3, 4]).await;
    assert_eq!(
        store.args("ci_merge_requests"),
        [json!({ "prs": facts, "turn": [3, 4] })]
    );
}

#[tokio::test]
async fn a_request_for_a_closed_pull_request_or_a_new_head_is_not_merged() {
    for (state, sha) in [("closed", "h7"), ("open", "other")] {
        let d = tempfile::tempdir().unwrap();
        let h = host(d.path());
        pull(d.path(), state, sha, "fix", &[]);
        let (am, store) = am(vec![ask(7, "h7", "automerge", true)]).await;
        assert_eq!(am.run(&h, 7, "h7").await, Ended::NotAsked, "{state} {sha}");
        assert!(merges(d.path()).is_empty());
        assert!(store.args("ci_merged").is_empty());
    }
}

#[tokio::test]
async fn a_failed_merge_says_why_and_tells_the_store_nothing() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    pull(d.path(), "open", "h7", "fix", &[]);
    std::fs::write(d.path().join("merge.fail"), "").unwrap();
    let (am, store) = am(vec![ask(7, "h7", "automerge", true)]).await;
    assert!(matches!(am.run(&h, 7, "h7").await, Ended::Failed(w) if w.contains("Head branch was modified")));
    assert!(store.args("ci_merged").is_empty());
}

fn agent_pr(dir: &Path, behind: u64, ci: &str) {
    std::fs::write(dir.join("compare.json"), json!({ "behind_by": behind }).to_string()).unwrap();
    let s = json!([{ "state": ci, "context": "ci/agent/pr/ci", "description": "agent-ci x: running" }]);
    std::fs::write(dir.join("agent.json"), s.to_string()).unwrap();
}

#[tokio::test]
async fn the_turn_is_the_up_to_date_agent_pull_requests_whose_check_is_pending() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    let (am, _) = am(vec![]).await;
    let agents = [(3, "agent".to_string())];
    agent_pr(d.path(), 0, "pending");
    assert_eq!(am.turn(&h, "main", &agents).await, [3]);
    assert!(calls(d.path()).contains("repos/o/r/compare/main...agent"));
    // Its check is done, or it is behind main: it holds no one.
    agent_pr(d.path(), 0, "success");
    assert!(am.turn(&h, "main", &agents).await.is_empty());
    agent_pr(d.path(), 2, "pending");
    assert!(am.turn(&h, "main", &agents).await.is_empty());
    assert!(am.turn(&h, "main", &[]).await.is_empty());
}

#[tokio::test]
async fn an_unreadable_store_leaves_no_request_and_no_timing_so_nothing_merges() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path());
    pull(d.path(), "open", "h7", "fix", &[]);
    let down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d2 = down.clone();
    let store = FakeStore::answering(move |op, _| {
        if d2.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("down".into());
        }
        Ok(match op {
            "ci_merge_requests" => {
                json!({ "requests": [ask(7, "h7", "automerge", true)], "ci": CiTiming::test_json() })
            }
            _ => json!({ "ok": true }),
        })
    });
    let am = Automerge::new(store.clone());
    am.refresh_requests("o/r", json!([]), &[]).await;
    assert!(am.request(7, "h7").is_some() && am.timing("o/r").is_some());
    down.store(true, std::sync::atomic::Ordering::SeqCst);
    am.refresh_requests("o/r", json!([]), &[]).await;
    assert_eq!(
        am.run(&h, 7, "h7").await,
        Ended::NotAsked,
        "the last answer is not trusted"
    );
    assert!(am.timing("o/r").is_none() && merges(d.path()).is_empty());
}

#[tokio::test]
async fn the_timing_follows_the_stores_answer_each_poll_and_an_unreadable_store_makes_it_unknown() {
    let said = Arc::new(Mutex::new(json!({ "requests": [], "ci": CiTiming::test_json() })));
    let (a, down) = (said.clone(), Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let d = down.clone();
    let store = FakeStore::answering(move |_, _| {
        if d.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("down".into());
        }
        Ok(a.lock().unwrap().clone())
    });
    let am = Automerge::new(store);
    am.refresh_requests("o/r", json!([]), &[]).await;
    assert_eq!(am.timing("o/r"), Some(CiTiming::test()));
    said.lock().unwrap()["ci"]["ciPollSeconds"] = json!(30);
    am.refresh_requests("o/r", json!([]), &[]).await;
    assert_eq!(
        am.timing("o/r").unwrap().poll_seconds,
        30,
        "a change on the site applies at the next poll"
    );
    down.store(true, std::sync::atomic::Ordering::SeqCst);
    am.refresh_requests("o/r", json!([]), &[]).await;
    assert_eq!(am.timing("o/r"), None);
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
    let (am, store) = am(vec![]).await;
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
