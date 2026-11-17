use std::sync::Mutex;

use super::*;
use crate::agent_ci::timing::CiTiming;

type Answer = Box<dyn Fn(&str, &Value) -> Result<Value, StoreError> + Send + Sync>;

/// A store that logs every op and answers with `answer`; by default a claim is won and the rest are ok.
pub(crate) struct FakeStore {
    pub calls: Mutex<Vec<(String, Value)>>,
    pub answer: Answer,
    /// The `requests` `ci_merge_requests` answers (what the store asks the runner to merge), when set: the rest of that answer is `answer`'s.
    pub requests: Mutex<Option<Vec<Value>>>,
}

impl FakeStore {
    pub fn winning() -> Arc<Self> {
        Self::answering(|op, _| {
            Ok(match op {
                "ci_claim" => json!({ "won": true, "ci": CiTiming::test_json() }),
                "ci_merge_requests" => json!({ "requests": [], "ci": CiTiming::test_json() }),
                _ => json!({ "ok": true }),
            })
        })
    }

    pub fn answering(f: impl Fn(&str, &Value) -> Result<Value, StoreError> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::default(),
            answer: Box::new(f),
            requests: Mutex::default(),
        })
    }

    /// The store asks the runner to merge these from now on (each `{ pr, sha, by, merge, holdUntil? }`).
    pub fn ask(&self, requests: Vec<Value>) {
        *self.requests.lock().unwrap() = Some(requests);
    }

    /// The args of each call of `op`, in order.
    pub fn args(&self, op: &str) -> Vec<Value> {
        let calls = self.calls.lock().unwrap();
        calls.iter().filter(|(o, _)| o == op).map(|(_, a)| a.clone()).collect()
    }
}

impl CiStore for FakeStore {
    fn op<'a>(&'a self, _: &'a str, op: &'static str, args: Value) -> BoxFuture<'a, Result<Value, StoreError>> {
        Box::pin(async move {
            let mut r = (self.answer)(op, &args);
            if let (Ok(v), "ci_merge_requests", Some(asked)) = (&mut r, op, &*self.requests.lock().unwrap()) {
                v["requests"] = json!(asked);
            }
            self.calls.lock().unwrap().push((op.to_string(), args));
            r
        })
    }
}

fn claim() -> Claim<'static> {
    Claim {
        sha: "abc",
        context: "ci/agent/pr/ci",
        event: "pull_request",
        pr: 7,
        branch: "feat",
        title: "",
        author: "me",
        computer: "pc",
        slots: 0,
    }
}

#[test]
fn a_claim_sends_what_it_has_and_a_push_sends_no_pr() {
    let a = claim_args(&claim());
    assert_eq!(
        a,
        json!({ "sha": "abc", "context": "ci/agent/pr/ci", "event": "pull_request", "computer": "pc",
                "pr": 7, "branch": "feat", "author": "me" })
    );
    let push = claim_args(&Claim { pr: 0, ..claim() });
    assert!(push.get("pr").is_none());
}

#[test]
fn the_runners_slots_go_with_a_claim_and_a_heartbeat_and_0_sends_none() {
    assert_eq!(claim_args(&Claim { slots: 4, ..claim() })["slots"], 4);
    assert!(claim_args(&claim()).get("slots").is_none());
    assert_eq!(
        heartbeat_args("abc", "ci/agent/pr/ci", 4),
        json!({ "sha": "abc", "context": "ci/agent/pr/ci", "slots": 4 })
    );
    assert_eq!(
        heartbeat_args("abc", "ci/agent/pr/ci", 0),
        json!({ "sha": "abc", "context": "ci/agent/pr/ci" })
    );
}

#[test]
fn a_claim_answer_is_won_finished_or_held() {
    assert_eq!(
        read_claim(&json!({ "won": true })),
        Claimed::Won(None),
        "an older store sends no timing"
    );
    assert_eq!(
        read_claim(&json!({ "won": true, "ci": CiTiming::test_json() })),
        Claimed::Won(Some(CiTiming::test()))
    );
    assert_eq!(
        read_claim(&json!({ "won": false, "finished": { "result": "passed" } })),
        Claimed::Finished
    );
    assert_eq!(
        read_claim(&json!({ "won": false, "holder": { "computer": "other" } })),
        Claimed::Held(Some("other".into()))
    );
    assert_eq!(read_claim(&json!({ "won": false })), Claimed::Held(None));
}

#[test]
fn a_heartbeat_that_says_not_ok_lost_the_lease() {
    assert!(lease_lost(&json!({ "ok": false, "holder": { "computer": "x" } })));
    assert!(!lease_lost(&json!({ "ok": true })));
    assert!(!lease_lost(&Value::Null));
}

#[test]
fn merge_requests_are_read_whole_and_junk_is_skipped_and_a_request_that_does_not_say_merge_waits() {
    let v = json!({ "requests": [
        { "pr": 3, "sha": "aa", "by": "automerge", "merge": true },
        { "pr": "x" },
        { "pr": 4, "sha": "bb", "by": "ana", "merge": false, "holdUntil": 99 },
        { "pr": 5, "sha": "cc" }
    ] });
    let want = |pr, sha: &str, by: &str, merge, held_until| MergeRequest {
        pr,
        sha: sha.into(),
        by: by.into(),
        merge,
        held_until,
    };
    assert_eq!(
        read_requests(&v),
        vec![
            want(3, "aa", "automerge", true, None),
            want(4, "bb", "ana", false, Some(99)),
            want(5, "cc", "", false, None)
        ]
    );
    assert!(read_requests(&Value::Null).is_empty());
}

#[test]
fn a_refusal_is_a_coded_answer_under_500_but_not_a_401() {
    let e = |status: u16, code: Option<&str>| StoreError {
        message: "m".into(),
        status: Some(status),
        code: code.map(str::to_string),
    };
    assert!(e(400, Some("bad_request")).is_refusal());
    assert!(
        !e(401, Some("unauthorized")).is_refusal(),
        "unlinked: the record waits for the link"
    );
    assert!(!e(503, Some("down")).is_refusal());
    assert!(!e(400, None).is_refusal());
}
