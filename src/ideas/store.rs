//! The shared backlog's own copy of the ideas queue (backlog/06.2-operations.md): [`Ideas::list`] and
//! [`Ideas::answer`] are `ideas_list` and `idea_answer`.
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backlog::{BacklogError, Client};

/// Waiting for an answer, written up (`accepted`), set aside for now (`later`: it may come back), or
/// turned down for good (`never`) — `IDEA_STATES`, shared/storeIdeas.ts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdeaState {
    Waiting,
    Accepted,
    Later,
    Never,
}

/// A person's answer to a waiting idea (`idea_answer`'s `answer`, backlog/06.2-operations.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdeaAnswer {
    Accept,
    Later,
    Never,
}

impl IdeaAnswer {
    fn as_str(self) -> &'static str {
        match self {
            IdeaAnswer::Accept => "accept",
            IdeaAnswer::Later => "later",
            IdeaAnswer::Never => "never",
        }
    }
}

/// What the store still knows about a row's idea, when it carries one (`Idea`, shared/moreTypes.ts,
/// as `StoreIdea.idea` holds it) — missing for a row added with only a title.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreIdeaBody {
    #[serde(default)]
    pub experience: String,
    #[serde(default)]
    pub why: String,
    #[serde(default, rename = "whyNow")]
    pub why_now: String,
    #[serde(default, rename = "for")]
    pub for_profiles: Vec<String>,
    #[serde(default)]
    pub feature: Option<String>,
    #[serde(default, rename = "newFeature")]
    pub new_feature: Option<String>,
    #[serde(default)]
    pub waited: u32,
}

/// Who answered a row and when, and the plan Accept wrote it up as (`StoreIdea.answer`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreIdeaAnswer {
    pub by: String,
    pub at: i64,
    #[serde(default, rename = "draftId")]
    pub draft_id: Option<String>,
}

/// One row of the store's queue (`StoreIdea`, shared/storeIdeas.ts), trimmed to what a round reads.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreIdea {
    pub id: String,
    pub title: String,
    pub state: IdeaState,
    #[serde(default)]
    pub idea: Option<StoreIdeaBody>,
    pub at: i64,
    #[serde(default)]
    pub answer: Option<StoreIdeaAnswer>,
    /// Set when a person added this idea themselves; a round leaves these alone (06.2-operations.md).
    #[serde(default, rename = "addedBy")]
    pub added_by: Option<Value>,
    /// The plan this idea is already written up as, when one is under way.
    #[serde(default, rename = "planId")]
    pub plan_id: Option<String>,
}

/// The last round that ran: when, what prompted it, and who (`IdeasRound`, shared/storeIdeas.ts).
#[derive(Debug, Clone, Deserialize)]
pub struct IdeasRoundInfo {
    pub at: i64,
    #[serde(default)]
    pub after: Option<String>,
    pub login: String,
}

/// `ideas_list`'s answer (backlog/06.2-operations.md): the waiting ideas first,
/// oldest first, then the answered ones, newest answer first.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreIdeas {
    pub round: Option<IdeasRoundInfo>,
    pub ideas: Vec<StoreIdea>,
}

/// This project's ideas on the shared backlog (backlog/06.2-operations.md): [`list`](Ideas::list) the
/// current round and [`answer`](Ideas::answer) a waiting one.
pub struct Ideas<'a> {
    pub client: &'a Client,
    pub repo_id: u64,
    pub token: Option<&'a str>,
}

impl Ideas<'_> {
    /// `ideas_list`.
    pub async fn list(&self) -> Result<StoreIdeas, BacklogError> {
        self.client
            .op(self.repo_id, self.token, "ideas_list", json!({}), "ideas")
            .await
    }

    /// `idea_answer { id, answer, draftId? }`, as this agent carrying out a person's command
    /// (`for: { via: "command", login? }`, backlog/06.2-operations.md).
    pub async fn answer(
        &self,
        id: &str,
        answer: IdeaAnswer,
        draft_id: Option<&str>,
        by: Option<&str>,
    ) -> Result<StoreIdeas, BacklogError> {
        let mut args = json!({ "id": id, "answer": answer.as_str(), "for": { "via": "command" } });
        if let Some(draft_id) = draft_id {
            args["draftId"] = json!(draft_id);
        }
        if let Some(by) = by {
            args["for"]["login"] = json!(by);
        }
        self.client
            .op(self.repo_id, self.token, "idea_answer", args, "ideas")
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fake_store(handler: axum::routing::MethodRouter) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route("/v1/repos/{id}/ops", handler);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn list_reads_the_round_and_ideas_the_store_answers() {
        let base = fake_store(axum::routing::post(|axum::Json(req): axum::Json<Value>| async move {
            assert_eq!(req["op"], "ideas_list");
            axum::Json(json!({
                "value": {
                    "round": { "at": 1, "after": "Chores", "login": "owner" },
                    "ideas": [{ "id": "1", "title": "Waiting", "state": "waiting", "at": 0 }],
                }
            }))
        }))
        .await;
        let client = Client::new(base);
        let ideas = Ideas {
            client: &client,
            repo_id: 1,
            token: Some("tok"),
        };
        let s = ideas.list().await.unwrap();
        assert_eq!(s.round.unwrap().login, "owner");
        assert_eq!(s.ideas[0].id, "1");
        assert_eq!(s.ideas[0].state, IdeaState::Waiting);
    }

    #[tokio::test]
    async fn answer_sends_the_answer_and_via_command_and_reads_the_new_round() {
        let base = fake_store(axum::routing::post(|axum::Json(req): axum::Json<Value>| async move {
            assert_eq!(req["op"], "idea_answer");
            assert_eq!(req["args"]["id"], "1");
            assert_eq!(req["args"]["answer"], "accept");
            assert_eq!(req["args"]["draftId"], "d1");
            assert_eq!(req["args"]["for"]["via"], "command");
            assert_eq!(req["args"]["for"]["login"], "owner");
            axum::Json(json!({ "value": { "round": null, "ideas": [] } }))
        }))
        .await;
        let client = Client::new(base);
        let ideas = Ideas {
            client: &client,
            repo_id: 1,
            token: Some("tok"),
        };
        ideas
            .answer("1", IdeaAnswer::Accept, Some("d1"), Some("owner"))
            .await
            .unwrap();
    }
}
