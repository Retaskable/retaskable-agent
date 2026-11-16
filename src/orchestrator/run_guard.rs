//! A run that panics still ends (docs/specs/pieces.md section 3.1): the run's work goes in a task of its own that is
//! joined, so a panic comes back as an error here and is sent as one `failed` end, never as silence, which would keep
//! the run held and beaten until its ceiling.
use std::future::Future;

use serde_json::{Value, json};

use super::driver::{RunEnd, RunEndSender};
use crate::backlog::runs::RunOutcome;

/// Runs `work` and sends its end under `token`; `None` is a run that ends with nothing to say (it was stopped or its
/// turn was lost). A panic ends `failed` with `what` ("merge", "watch") in the words.
pub(super) fn spawn_ended<W>(ends: RunEndSender, token: String, what: &'static str, work: W)
where
    W: Future<Output = Option<(RunOutcome, Value)>> + Send + 'static,
{
    tokio::spawn(async move {
        let end = match tokio::spawn(work).await {
            Ok(end) => end,
            Err(e) => {
                tracing::error!("a {what} run panicked: {e}");
                let why = format!("The {what} run stopped unexpectedly: {e}");
                Some((RunOutcome::Failed(why), json!({})))
            }
        };
        if let Some((outcome, result)) = end {
            ends.send(RunEnd { token, outcome, result });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_panic_in_the_work_still_sends_one_failed_end() {
        let (ends, mut rx) = RunEndSender::channel();
        spawn_ended(ends, "t1".into(), "merge", async { panic!("boom") });
        let end = rx.recv().await.unwrap();
        assert_eq!(end.token, "t1");
        let RunOutcome::Failed(why) = end.outcome else {
            panic!("not failed")
        };
        assert!(why.starts_with("The merge run stopped unexpectedly: "), "{why}");
        assert!(rx.recv().await.is_none(), "exactly one end");
    }

    #[tokio::test]
    async fn the_works_own_end_is_sent_and_nothing_is_sent_for_none() {
        let (ends, mut rx) = RunEndSender::channel();
        spawn_ended(ends.clone(), "a".into(), "watch", async {
            Some((RunOutcome::Merged, json!({})))
        });
        spawn_ended(ends, "b".into(), "watch", async { None });
        let end = rx.recv().await.unwrap();
        assert_eq!((end.token.as_str(), end.outcome), ("a", RunOutcome::Merged));
        assert!(rx.recv().await.is_none());
    }
}
