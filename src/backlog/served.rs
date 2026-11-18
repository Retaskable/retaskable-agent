//! `projects_served` (chat-agent-process.md section 5.1): the one op that names no repo, so it has a route of its own,
//! `POST <store>/v1/agent/ops`. Only a Chat Agent's access token may send it; it answers the projects the person's
//! linked agents serve, so a Chat Agent learns what it may work on from the store instead of from a config file.
use serde::Deserialize;
use serde_json::json;

use super::{BacklogError, Client};

/// A project with a GitHub repository. `repo_id` is the string `/v1/repos/<repo id>/ops` takes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServedRepo {
    pub repo_id: String,
    pub github_repo: String,
    /// GitHub's default branch: the store doesn't hold a project's own base branch.
    pub default_branch: String,
    /// `read` or `write`.
    pub access: String,
}

/// A project with no GitHub repository, as the person's agents last connected to it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServedBacklog {
    pub client: String,
    pub project_id: String,
}

/// What the store says this Chat Agent may serve. Empty lists mean nothing is served yet.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Served {
    #[serde(default)]
    pub projects: Vec<ServedRepo>,
    #[serde(default)]
    pub project_backlogs: Vec<ServedBacklog>,
}

impl Client {
    /// `projects_served` with this Chat Agent's store access token. A refusal keeps its status: `401` not signed in,
    /// `403` not a chat link, `429` over the limit, `503` the store is down.
    pub async fn projects_served(&self, token: &str) -> Result<Served, BacklogError> {
        let url = format!("{}/v1/agent/ops", self.base_url);
        let (status, body) = self
            .post_op(&url, Some(token), "projects_served", json!({}))
            .await
            .map_err(|(e, _)| e)?;
        let bad = |message: String| BacklogError::BadAnswer {
            status,
            code: None,
            message,
        };
        let value = body.value.ok_or_else(|| bad("its answer had no value".to_string()))?;
        serde_json::from_value(value).map_err(|e| bad(format!("couldn't read the projects it sent: {e}")))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::http::{HeaderMap, StatusCode};
    use axum::{Json, Router, routing::post};
    use serde_json::Value;
    use std::sync::{Arc, Mutex};

    /// A fake store answering `projects_served` with `status` and `reply`, keeping each body and bearer token.
    pub(crate) async fn store(status: u16, reply: Value) -> (String, Arc<Mutex<Vec<(Value, String)>>>) {
        let seen: Arc<Mutex<Vec<(Value, String)>>> = Arc::default();
        let kept = seen.clone();
        let app = Router::new().route(
            "/v1/agent/ops",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let (kept, reply) = (kept.clone(), reply.clone());
                async move {
                    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
                    kept.lock().unwrap().push((body, auth.to_string()));
                    (StatusCode::from_u16(status).unwrap(), Json(reply))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), seen)
    }

    #[tokio::test]
    async fn the_answer_is_read_and_the_request_names_no_repo() {
        let reply = json!({ "value": {
            "projects": [{ "repoId": "123", "githubRepo": "o/r", "defaultBranch": "main", "access": "write" }],
            "projectBacklogs": [{ "client": "retaskable-production", "projectId": "77" }],
        }});
        let (url, seen) = store(200, reply).await;
        let served = Client::new(url).projects_served("tok").await.unwrap();
        assert_eq!(served.projects[0].repo_id, "123");
        assert_eq!(served.projects[0].default_branch, "main");
        assert_eq!(served.project_backlogs[0].project_id, "77");
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, json!({ "op": "projects_served", "args": {} }));
        assert_eq!(seen[0].1, "Bearer tok");
    }

    #[tokio::test]
    async fn a_refusal_keeps_its_status_and_words() {
        let (url, _) = store(403, json!({ "error": "forbidden", "message": "Only a Chat Agent" })).await;
        match Client::new(url).projects_served("tok").await {
            Err(BacklogError::BadAnswer { status, code, message }) => {
                assert_eq!(
                    (status, code.as_deref(), message.as_str()),
                    (403, Some("forbidden"), "Only a Chat Agent")
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn no_store_is_unreachable() {
        let err = Client::new("http://127.0.0.1:1")
            .projects_served("t")
            .await
            .unwrap_err();
        assert!(matches!(err, BacklogError::Unreachable(_)));
    }
}
