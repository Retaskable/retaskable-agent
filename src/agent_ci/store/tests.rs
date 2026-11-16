use std::sync::Mutex;

use super::*;

type Answer = Box<dyn Fn(&str, &Value) -> Result<Value, StoreError> + Send + Sync>;

/// A store that logs every op and answers with `answer`; by default a claim is won and the rest are ok.
pub(crate) struct FakeStore {
    pub calls: Mutex<Vec<(String, Value)>>,
    pub answer: Answer,
}

impl FakeStore {
    pub fn winning() -> Arc<Self> {
        Self::answering(|op, _| {
            Ok(match op {
                "ci_claim" => json!({ "won": true }),
                "ci_merge_requests" => json!({ "requests": [] }),
                _ => json!({ "ok": true }),
            })
        })
    }

    pub fn answering(f: impl Fn(&str, &Value) -> Result<Value, StoreError> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::default(),
            answer: Box::new(f),
        })
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
            let r = (self.answer)(op, &args);
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
    assert_eq!(read_claim(&json!({ "won": true })), Claimed::Won);
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
fn merge_requests_are_read_as_pairs_and_junk_is_skipped() {
    let v = json!({ "requests": [{ "pr": 3, "sha": "aa" }, { "pr": "x" }, { "pr": 4, "sha": "bb" }] });
    assert_eq!(read_requests(&v), vec![(3, "aa".to_string()), (4, "bb".to_string())]);
    assert!(read_requests(&Value::Null).is_empty());
}
