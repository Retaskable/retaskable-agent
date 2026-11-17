//! The vet job's own dependencies as the computer has them (`realDeps` in `vetChore.ts`): the project's hub through
//! its cloud link for the suggestions and the verdicts (`BugLink::ask`), the store's lists for the titles a duplicate
//! must name, and a read-only writer session for the judging.
use std::sync::Arc;

use serde_json::{Value, json};

use super::host::JobSession;
use super::store::StoreAccess;
use super::vet::{
    Known, SuggestionToVet, SuggestionVerdict, SuggestionsAnswer, Verdict, VerdictAnswer, VetDeps, judge_with,
};
use crate::connector::bug_link::BugLink;

pub struct VetWires<'a> {
    /// The project's hub link; none when it has none (every ask is then "couldn't reach").
    pub hub: Option<Arc<BugLink>>,
    pub store: Option<StoreAccess>,
    pub session: &'a dyn JobSession,
}

impl VetWires<'_> {
    async fn ask_hub(&self, msg: Value) -> Option<Value> {
        self.hub.as_ref()?.ask(msg).await
    }
}

impl VetDeps for VetWires<'_> {
    async fn ask(&self) -> Option<SuggestionsAnswer> {
        let answer = self.ask_hub(json!({ "type": "suggestions_ask" })).await?;
        if answer.get("type").and_then(Value::as_str) != Some("suggestions") {
            return None;
        }
        let list = answer.get("list").and_then(Value::as_array).map(|l| {
            l.iter()
                .filter_map(|s| serde_json::from_value::<SuggestionToVet>(s.clone()).ok())
                .collect()
        });
        Some(SuggestionsAnswer {
            list,
            reason: answer.get("reason").and_then(Value::as_str).map(str::to_string),
        })
    }

    async fn verdict(&self, v: &Verdict) -> Option<VerdictAnswer> {
        let mut msg = serde_json::to_value(v).ok()?;
        msg["type"] = json!("suggestion_verdict");
        let answer = self.ask_hub(msg).await?;
        if answer.get("type").and_then(Value::as_str) != Some("suggestion_done") {
            return None;
        }
        Some(VerdictAnswer {
            ok: answer.get("ok").and_then(Value::as_bool).unwrap_or(false),
            reason: answer.get("reason").and_then(Value::as_str).map(str::to_string),
            acted_as: answer
                .get("as")
                .and_then(|a| serde_json::from_value::<SuggestionVerdict>(a.clone()).ok()),
        })
    }

    async fn known(&self) -> Known {
        match &self.store {
            Some(store) => store.known_titles().await,
            None => Known::default(),
        }
    }

    async fn judge(&self, prompt: &str) -> Result<Value, String> {
        judge_with(&self.session.config().ephemeral(), prompt, |spec| {
            self.session.run(spec)
        })
        .await
    }
}
