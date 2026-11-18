//! Linear as an issue source (issue-import.md section 3.5): its GraphQL API with this computer's own key. The list is a
//! team's open issues with a label; done is a comment and, with Close issues when done, the team's first "completed"
//! state. Every author is trusted: the label is the filter (`LinearIssues`, `agent/src/issues/linear.ts`).
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::keys::{Keyed, Keys};
use super::{Filter, Issue, IssueList, IssueRef, Provider, first_line};

const API: &str = "https://api.linear.app/graphql";
const LIMIT: u32 = 50;
const TIMEOUT: Duration = Duration::from_secs(30);
pub const NO_KEY: &str = "No Linear key on this computer: set one with retaskable secret set LINEAR_API_KEY";

const LIST: &str = r#"query Issues($team: String!, $label: String!, $first: Int!) {
  issues(first: $first, filter: { team: { key: { eqIgnoreCase: $team } }, labels: { some: { name: { eqIgnoreCase: $label } } }, state: { type: { nin: ["completed", "canceled"] } } }) {
    nodes { identifier title description url creator { displayName } }
  }
}"#;
const FIND: &str = r#"query Issue($id: String!) {
  issue(id: $id) { id team { states(filter: { type: { eq: "completed" } }) { nodes { id position } } } }
}"#;
const COMMENT: &str = "mutation Comment($issueId: String!, $body: String!) { commentCreate(input: { issueId: $issueId, body: $body }) { success } }";
const VIEWER: &str = "query Viewer { viewer { name } }";
const CLOSE: &str = "mutation Close($id: String!, $stateId: String!) { issueUpdate(id: $id, input: { stateId: $stateId }) { success } }";

pub struct LinearIssues {
    keys: Arc<dyn Keys>,
    api: String,
    http: reqwest::Client,
}

impl LinearIssues {
    pub fn new(keys: Arc<dyn Keys>) -> Self {
        Self::at(keys, API)
    }

    /// Against another address (a test's server).
    pub fn at(keys: Arc<dyn Keys>, api: impl Into<String>) -> Self {
        Self {
            keys,
            api: api.into(),
            http: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .expect("building the Linear HTTP client"),
        }
    }

    /// The data of one request; a failure is Linear's first error message, or its status.
    async fn gql(&self, query: &str, variables: Value) -> Result<Value, String> {
        let key = self.keys.get(Keyed::Linear).ok_or_else(|| NO_KEY.to_string())?;
        let sent = self
            .http
            .post(&self.api)
            .header("Content-Type", "application/json")
            .header("Authorization", key)
            .body(json!({ "query": query, "variables": variables }).to_string())
            .send()
            .await
            .map_err(|e| format!("Linear: {}", first_line(&e.without_url().to_string())))?;
        let status = sent.status().as_u16();
        let text = sent.text().await.unwrap_or_default();
        // Not JSON: the status says what went wrong.
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let error = body["errors"][0]["message"].as_str().map(first_line);
        match body.get("data").filter(|d| !d.is_null()) {
            Some(data) if error.is_none() && status == 200 => Ok(data.clone()),
            _ => Err(format!("Linear: {}", error.unwrap_or_else(|| format!("HTTP {status}")))),
        }
    }

    /// Whose key it is: Linear's name for its owner. Fails when Linear refuses it.
    pub async fn who_am_i(&self) -> Result<String, String> {
        let data = self.gql(VIEWER, json!({})).await?;
        Ok(data["viewer"]["name"].as_str().unwrap_or_default().to_string())
    }

    pub async fn list(&self, filter: &Filter) -> Result<IssueList, String> {
        let (Some(team), Some(label)) = (filter.team.as_deref(), filter.label.as_deref()) else {
            return Ok(IssueList::default());
        };
        let data = self
            .gql(LIST, json!({ "team": team, "label": label, "first": LIMIT }))
            .await?;
        let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
        let issues = data["issues"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| Issue {
                issue: IssueRef {
                    provider: Provider::Linear,
                    key: text(&i["identifier"]),
                    url: text(&i["url"]),
                },
                title: text(&i["title"]),
                body: text(&i["description"]),
                author: text(&i["creator"]["displayName"]),
                trusted: true,
            })
            .collect();
        Ok(IssueList {
            issues,
            milestone_missing: false,
        })
    }

    pub async fn comment(&self, issue: &IssueRef, text: &str) -> Result<(), String> {
        let found = self.find(issue).await?;
        self.gql(COMMENT, json!({ "issueId": found.id, "body": text }))
            .await
            .map(|_| ())
    }

    /// Moves it to its team's first "completed" state, by position.
    pub async fn close(&self, issue: &IssueRef) -> Result<(), String> {
        let found = self.find(issue).await?;
        let done = found
            .states
            .iter()
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .ok_or_else(|| format!("Linear: {}'s team has no completed state", issue.key))?;
        self.gql(CLOSE, json!({ "id": found.id, "stateId": done.0 }))
            .await
            .map(|_| ())
    }

    async fn find(&self, issue: &IssueRef) -> Result<Found, String> {
        if issue.provider != Provider::Linear {
            return Err(format!("{} isn't a Linear issue", issue.key));
        }
        let data = self.gql(FIND, json!({ "id": issue.key })).await?;
        let Some(found) = data.get("issue").filter(|i| !i.is_null()) else {
            return Err(format!("Linear: no issue {}", issue.key));
        };
        Ok(Found {
            id: found["id"].as_str().unwrap_or_default().to_string(),
            states: found["team"]["states"]["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| {
                    (
                        s["id"].as_str().unwrap_or_default().to_string(),
                        s["position"].as_f64().unwrap_or(0.0),
                    )
                })
                .collect(),
        })
    }
}

struct Found {
    id: String,
    /// The team's completed states: id and position.
    states: Vec<(String, f64)>,
}
