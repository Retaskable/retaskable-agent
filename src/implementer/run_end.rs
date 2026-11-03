//! How a build or fix run ends on the implementer's `waits_on` and `nothing_left` answers
//! (docs/specs/pieces.md section 4, item `item-waits-and-finishes`). A pure function for the build
//! runner to call; the claim loop's `classify` (`orchestrator/session.rs`) is left as it is.
use serde_json::{Value, json};

use super::model::ImplementerResult;
use crate::backlog::runs::RunOutcome;

/// The run outcome and the `result` JSON for `run_done`, or `None` for a status this doesn't own
/// (the runner handles those itself). `nothing_left` is the run's own outcome. `waits_on` ends the
/// run `failed` with the items it needs as its why, until runs have an outcome of their own for
/// it; `run_done` puts the why into `result.why`, and `waitsOn` carries the ids for the store.
pub fn run_end(r: &ImplementerResult) -> Option<(RunOutcome, Value)> {
    match r.status.as_str() {
        "nothing_left" => Some((RunOutcome::NothingLeft, json!({ "summary": r.summary }))),
        "waits_on" => {
            let needs = if r.waits_on.is_empty() {
                "another item".to_string()
            } else {
                r.waits_on.join(", ")
            };
            let why = format!("Needs {needs} built first: {}", r.summary);
            Some((RunOutcome::Failed(why), json!({ "waitsOn": r.waits_on })))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::Client;
    use crate::backlog::tests::store;

    fn result(status: &str, waits_on: &[&str]) -> ImplementerResult {
        serde_json::from_value(json!({ "status": status, "summary": "the summary", "waitsOn": waits_on })).unwrap()
    }

    #[test]
    fn nothing_left_is_its_own_outcome() {
        let (o, v) = run_end(&result("nothing_left", &[])).unwrap();
        assert_eq!(o, RunOutcome::NothingLeft);
        assert_eq!(v, json!({ "summary": "the summary" }));
    }

    #[test]
    fn waits_on_fails_the_run_naming_the_items() {
        let (o, v) = run_end(&result("waits_on", &["a", "b"])).unwrap();
        let RunOutcome::Failed(why) = o else { panic!("{o:?}") };
        assert_eq!(why, "Needs a, b built first: the summary");
        assert_eq!(v, json!({ "waitsOn": ["a", "b"] }));
    }

    #[test]
    fn an_empty_waits_on_says_another_item() {
        let (o, v) = run_end(&result("waits_on", &[])).unwrap();
        assert_eq!(
            o,
            RunOutcome::Failed("Needs another item built first: the summary".into())
        );
        assert_eq!(v, json!({ "waitsOn": [] }));
    }

    #[test]
    fn other_statuses_are_the_runners() {
        for s in ["pr_opened", "failed", "needs_clarification", "person_steps"] {
            assert!(run_end(&result(s, &[])).is_none(), "{s}");
        }
    }

    #[tokio::test]
    async fn waits_on_goes_through_run_done_as_failed_with_why() {
        let (base, seen) = store(200, json!({ "value": null })).await;
        let (o, v) = run_end(&result("waits_on", &["a", "b"])).unwrap();
        Client::new(base).run_done(1, None, "t", &o, v).await.unwrap();
        let sent = seen.lock().unwrap()[0].clone();
        assert_eq!(sent["args"]["outcome"], "failed");
        assert_eq!(sent["args"]["result"]["why"], "Needs a, b built first: the summary");
        assert_eq!(sent["args"]["result"]["waitsOn"], json!(["a", "b"]));
    }
}
