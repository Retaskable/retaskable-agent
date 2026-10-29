//! The shared backlog's own copy of the ideas queue (backlog/06.2-operations.md): [`Ideas::list`],
//! [`Ideas::round`] and [`Ideas::answer`] are `ideas_list`, `ideas_round` and `idea_answer`;
//! [`queue_from_store`] folds a read into [`Queue`].
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backlog::{BacklogError, Client};

use super::model::{Idea, Queue};

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

/// One row of the store's queue (`StoreIdea`, shared/storeIdeas.ts), trimmed to what a round reads:
/// a person's own idea (`addedBy` set) isn't read here yet — see the module doc comment.
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
}

/// The last round that ran: when, what prompted it, and who (`IdeasRound`, shared/storeIdeas.ts).
#[derive(Debug, Clone, Deserialize)]
pub struct IdeasRoundInfo {
    pub at: i64,
    #[serde(default)]
    pub after: Option<String>,
    pub login: String,
}

/// `ideas_list`'s and `ideas_round`'s answer (backlog/06.2-operations.md): the waiting ideas first,
/// oldest first, then the answered ones, newest answer first.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreIdeas {
    pub round: Option<IdeasRoundInfo>,
    pub ideas: Vec<StoreIdea>,
}

/// [`refilled`](super::model::refilled)'s [`Queue`], folded from the store's own rows (trimmed from
/// `asList`, shared/storeIdeas.ts: a person's waiting ideas, `addedBy` set, aren't folded in yet).
pub fn queue_from_store(s: &StoreIdeas) -> Queue {
    let of = |i: &StoreIdea| {
        let body = i.idea.clone().unwrap_or_default();
        Idea {
            id: i.id.clone(),
            title: i.title.clone(),
            experience: body.experience,
            why: body.why,
            why_now: body.why_now,
            for_profiles: body.for_profiles,
            feature: body.feature,
            new_feature: body.new_feature,
            waited: body.waited,
        }
    };
    Queue {
        ideas: s
            .ideas
            .iter()
            .filter(|i| i.state == IdeaState::Waiting)
            .map(of)
            .collect(),
        accepted_titles: s
            .ideas
            .iter()
            .filter(|i| i.state == IdeaState::Accepted)
            .map(|i| i.title.clone())
            .collect(),
        never: s
            .ideas
            .iter()
            .filter(|i| i.state == IdeaState::Never)
            .map(|i| i.title.clone())
            .collect(),
    }
}

/// `idea` as `ideas_round`'s own `Idea` JSON shape (shared/moreTypes.ts) sends it.
fn idea_json(idea: &Idea) -> Value {
    let mut v = json!({
        "id": idea.id,
        "title": idea.title,
        "experience": idea.experience,
        "why": idea.why,
        "whyNow": idea.why_now,
        "for": idea.for_profiles,
        "waited": idea.waited,
    });
    if let Some(feature) = &idea.feature {
        v["feature"] = json!(feature);
    }
    if let Some(new_feature) = &idea.new_feature {
        v["newFeature"] = json!(new_feature);
    }
    v
}

/// This project's ideas on the shared backlog (backlog/06.2-operations.md): [`list`](Ideas::list) the
/// current round, [`round`](Ideas::round) to replace the waiting ideas after a session runs, and
/// [`answer`](Ideas::answer) a waiting one (Accept/Not now/Never).
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

    /// `ideas_round { ideas, after? }`: at most [`QUEUE`](super::model::QUEUE) ideas, from
    /// [`refilled`](super::model::refilled).
    pub async fn round(&self, ideas: &[Idea], after: Option<&str>) -> Result<StoreIdeas, BacklogError> {
        let mut args = json!({ "ideas": ideas.iter().map(idea_json).collect::<Vec<_>>() });
        if let Some(after) = after {
            args["after"] = json!(after);
        }
        self.client
            .op(self.repo_id, self.token, "ideas_round", args, "ideas")
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

    fn store_idea(id: &str, title: &str, state: IdeaState, idea: Option<StoreIdeaBody>) -> StoreIdea {
        StoreIdea {
            id: id.to_string(),
            title: title.to_string(),
            state,
            idea,
            at: 0,
            answer: None,
        }
    }

    #[test]
    fn queue_from_store_folds_waiting_accepted_and_never_rows() {
        let s = StoreIdeas {
            round: None,
            ideas: vec![
                store_idea(
                    "1",
                    "Waiting",
                    IdeaState::Waiting,
                    Some(StoreIdeaBody {
                        experience: "You see it.".to_string(),
                        why: "Because.".to_string(),
                        why_now: "Now.".to_string(),
                        for_profiles: vec!["shopper".to_string()],
                        feature: Some("inbox".to_string()),
                        new_feature: None,
                        waited: 2,
                    }),
                ),
                store_idea("2", "No body yet", IdeaState::Waiting, None),
                store_idea("3", "Taken", IdeaState::Accepted, None),
                store_idea("4", "Turned down", IdeaState::Never, None),
                store_idea("5", "Set aside", IdeaState::Later, None),
            ],
        };
        let q = queue_from_store(&s);
        assert_eq!(q.ideas.len(), 2, "only the waiting rows become ideas");
        assert_eq!(q.ideas[0].title, "Waiting");
        assert_eq!(q.ideas[0].why_now, "Now.");
        assert_eq!(q.ideas[0].feature, Some("inbox".to_string()));
        assert_eq!(q.ideas[0].waited, 2);
        assert_eq!(
            q.ideas[1].title, "No body yet",
            "a row with no idea body still waits, empty"
        );
        assert_eq!(q.ideas[1].experience, "");
        assert_eq!(q.accepted_titles, vec!["Taken".to_string()]);
        assert_eq!(q.never, vec!["Turned down".to_string()]);
    }

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
    async fn round_sends_the_ideas_and_after_and_reads_the_new_round() {
        let base = fake_store(axum::routing::post(|axum::Json(req): axum::Json<Value>| async move {
            assert_eq!(req["op"], "ideas_round");
            assert_eq!(req["args"]["after"], "Chores");
            let sent = &req["args"]["ideas"][0];
            assert_eq!(sent["id"], "t-0");
            assert_eq!(sent["feature"], "inbox");
            assert!(sent.get("newFeature").is_none());
            axum::Json(json!({ "value": { "round": null, "ideas": [] } }))
        }))
        .await;
        let client = Client::new(base);
        let ideas = Ideas {
            client: &client,
            repo_id: 1,
            token: Some("tok"),
        };
        let idea = Idea {
            id: "t-0".to_string(),
            title: "A".to_string(),
            experience: "x".to_string(),
            why: "y".to_string(),
            why_now: "z".to_string(),
            for_profiles: vec!["shopper".to_string()],
            feature: Some("inbox".to_string()),
            new_feature: None,
            waited: 0,
        };
        ideas.round(&[idea], Some("Chores")).await.unwrap();
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
