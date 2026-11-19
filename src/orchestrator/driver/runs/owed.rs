//! The ends of finished runs the store hasn't taken, and the state file that keeps them across a restart
//! (`crate::runs::owed`): listed before the first send, sent again at once after a restart and before `runs_drop`
//! (which would end the run as `interrupted` and so make its result untakeable), and dropped when the store answers.
use std::time::Instant;

use serde_json::json;

use super::*;
use crate::runs::OwedEnd;

impl RunLoop {
    /// A loop that sends again the ends kept in the state file, those not past a day old, at once.
    pub fn restoring(project: &str, kept: Vec<OwedEnd>, now_secs: i64) -> Self {
        let mut me = Self::new(project);
        let due = Instant::now();
        let ends = kept.iter().filter_map(|k| k.restore(now_secs, due)).collect();
        me.runs.restore(ends);
        me.saved_ends = kept;
        me
    }
}

impl ProjectLoop {
    /// Writes the ends owed to the state file when they differ from what it holds, so one is on disk before it is sent.
    pub(super) fn save_owed_ends(&mut self) {
        let now = self.now_secs();
        let owed: Vec<OwedEnd> = (self.run_loop.runs.owed().iter())
            .map(|e| {
                let since = (self.run_loop.saved_ends.iter())
                    .find(|k| k.token == e.token)
                    .map_or(now, |k| k.since);
                OwedEnd::of(e, since)
            })
            .collect();
        if owed == self.run_loop.saved_ends {
            return;
        }
        self.store.set_run_ends_owed(&owed);
        if let Err(e) = self.store.save() {
            tracing::warn!("{}: saving the run ends owed to the store failed: {e:#}", self.id);
        }
        self.run_loop.saved_ends = owed;
    }

    /// Sends every end that is due, saves what is still owed, and says what the store answered.
    pub(super) async fn send_due_ends(&mut self, repo: u64, token: Option<&str>, now: Instant) {
        self.save_owed_ends();
        let due: Vec<_> = self.run_loop.runs.due_ends(now).into_iter().cloned().collect();
        for mut end in due {
            // The questions the run answered itself (one-tap-or-none.md): kept on the end, never sent to the store with it.
            let auto = end
                .result
                .as_object_mut()
                .and_then(|o| o.remove(crate::orchestrator::piece_ask::AUTO_ANSWER));
            if let RunOutcome::Failed(why) = &end.outcome
                && crate::runs::is_outside(why)
            {
                if !end.result.is_object() {
                    end.result = json!({});
                }
                end.result["outside"] = json!(true);
            }
            let answer = if end.merge {
                let outcome = MergeOutcome::of_run(&end.outcome, &end.result);
                match self.client.merge_result(repo, token, &end.token, &outcome).await {
                    Ok(answer) => {
                        if !answer.held && !matches!(outcome, MergeOutcome::Merged { .. }) {
                            tracing::info!(turn = %end.token, "the store dropped a merge result: the turn was no longer held");
                        }
                        Ok(())
                    }
                    Err(e) => {
                        self.store_error("merge_result", &e);
                        Err(RunDoneError::Other(e))
                    }
                }
            } else {
                let answer = self
                    .client
                    .run_done(repo, token, &end.token, &end.outcome, end.result)
                    .await;
                if let Err(RunDoneError::Other(e)) = &answer {
                    self.store_error("run_done", e);
                }
                if answer.is_ok()
                    && let Some(auto) = auto
                {
                    self.answer_for_person(repo, token, &auto).await;
                }
                answer
            };
            self.run_loop.runs.end_sent(&end.token, answer, now);
        }
        self.save_owed_ends();
    }
}

impl ProjectLoop {
    /// Answers the piece whose run just ended in questions, with the first options its session marked obvious, through the same
    /// store op a person's **Go with the recommended** uses. A store that refuses or can't be reached leaves the piece waiting
    /// for a person: nothing is lost, and nothing else is tried.
    async fn answer_for_person(&mut self, repo: u64, token: Option<&str>, auto: &serde_json::Value) {
        let text = |k: &str| auto.get(k).and_then(|v| v.as_str()).unwrap_or_default();
        for line in auto
            .get("went")
            .and_then(|w| w.as_array())
            .into_iter()
            .flatten()
            .filter_map(|l| l.as_str())
        {
            tracing::info!("{}: {} ({})", self.id, line, text("piece"));
        }
        if let Err(e) = self
            .client
            .piece_answer(repo, token, text("piece"), text("answers"))
            .await
        {
            self.store_failure("piece_answer", text("piece"), &e).await;
        }
    }
}
