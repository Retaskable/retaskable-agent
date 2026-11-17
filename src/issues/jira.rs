//! Jira as an issue source (issue-import.md section 3.5): its REST API (v2, whose texts are plain) on the project's Jira
//! site, with this computer's own email and API token, kept with the site they were checked on and sent nowhere else (a
//! shared setting could name any host). The list is a project's open issues with a label; done is a comment and, with
//! Close issues when done, the transition named "Done". Every author is trusted: the label is the filter (`JiraIssues`,
//! `agent/src/issues/jira.ts`).
use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::keys::{Keyed, Keys};
use super::{Filter, Issue, IssueList, IssueRef, Provider, first_line};

const LIMIT: u32 = 50;
const TIMEOUT: Duration = Duration::from_secs(30);
pub const NO_KEY: &str = "No Jira key on this computer: set one with retaskable secret set JIRA_API_KEY";

/// The secret's value: `<site> <email>:<token>`, the site the person typed and the key was checked on.
pub fn jira_key(site: &str, auth: &str) -> String {
    format!("{site} {auth}")
}

/// The site (lowercase) and `email:token` of a stored key.
fn parse_key(key: &str) -> Option<(String, &str)> {
    let at = key.find(' ').filter(|&at| at > 0)?;
    Some((key[..at].to_lowercase(), &key[at + 1..]))
}

/// A JQL string: quoted, with its quotes and backslashes escaped.
fn jql(v: &str) -> String {
    format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

pub struct JiraIssues {
    /// The Jira site's host (`acme.atlassian.net`), needed to list; an issue's own url says where it is.
    site: Option<String>,
    keys: Arc<dyn Keys>,
    scheme: &'static str,
    http: reqwest::Client,
}

impl JiraIssues {
    pub fn new(site: Option<String>, keys: Arc<dyn Keys>) -> Self {
        Self::with_scheme(site, keys, "https")
    }

    /// With another scheme (a test's plain-HTTP server).
    pub fn with_scheme(site: Option<String>, keys: Arc<dyn Keys>, scheme: &'static str) -> Self {
        Self {
            site,
            keys,
            scheme,
            http: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .expect("building the Jira HTTP client"),
        }
    }

    /// One request's JSON; a failure is Jira's first error message, or its status. The key goes only to its own site.
    async fn call(&self, url: &str, method: Method, body: Option<Value>) -> Result<Value, String> {
        let stored = self.keys.get(Keyed::Jira);
        let (site, auth) = stored
            .as_deref()
            .and_then(parse_key)
            .ok_or_else(|| NO_KEY.to_string())?;
        let url = Url::parse(url).map_err(|e| format!("Jira: {e}"))?;
        let host = match (url.host_str(), url.port()) {
            (Some(h), Some(p)) => format!("{}:{p}", h.to_lowercase()),
            (Some(h), None) => h.to_lowercase(),
            _ => String::new(),
        };
        if host != site {
            return Err(format!("Jira: this computer's key is for {site}, not {host}"));
        }
        let (user, token) = auth.split_once(':').unwrap_or((auth, ""));
        let mut req = self
            .http
            .request(method, url)
            .header("Accept", "application/json")
            .basic_auth(user, Some(token));
        if let Some(body) = body {
            req = req.header("Content-Type", "application/json").body(body.to_string());
        }
        let sent = req
            .send()
            .await
            .map_err(|e| format!("Jira: {}", first_line(&e.without_url().to_string())))?;
        let status = sent.status().as_u16();
        let text = sent.text().await.unwrap_or_default();
        // Not JSON: the status says what went wrong.
        let out: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        if (200..300).contains(&status) {
            return Ok(out);
        }
        let error = out["errorMessages"][0]
            .as_str()
            .or_else(|| {
                out["errors"]
                    .as_object()
                    .and_then(|e| e.values().next())
                    .and_then(Value::as_str)
            })
            .map(first_line);
        Err(format!("Jira: {}", error.unwrap_or_else(|| format!("HTTP {status}"))))
    }

    fn api(&self, site: &str, path: &str) -> String {
        format!("{}://{site}/rest/api/2/{path}", self.scheme)
    }

    /// Whose key it is: Jira's name for its owner. Fails when Jira refuses it.
    pub async fn who_am_i(&self) -> Result<String, String> {
        let site = self.site.as_deref().ok_or("Jira: no site")?;
        let me = self.call(&self.api(site, "myself"), Method::GET, None).await?;
        Ok(me["displayName"].as_str().unwrap_or_default().to_string())
    }

    pub async fn list(&self, filter: &Filter) -> Result<IssueList, String> {
        let (Some(site), Some(project), Some(label)) =
            (self.site.as_deref(), filter.project.as_deref(), filter.label.as_deref())
        else {
            return Ok(IssueList::default());
        };
        let query = format!(
            "project = {} AND labels = {} AND statusCategory != Done ORDER BY created DESC",
            jql(project),
            jql(label)
        );
        let url = Url::parse_with_params(
            &self.api(site, "search/jql"),
            [
                ("jql", query.as_str()),
                ("maxResults", &LIMIT.to_string()),
                ("fields", "summary,description,reporter"),
            ],
        )
        .map_err(|e| format!("Jira: {e}"))?;
        let data = self.call(url.as_str(), Method::GET, None).await?;
        let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
        let issues = data["issues"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| Issue {
                issue: IssueRef {
                    provider: Provider::Jira,
                    key: text(&i["key"]),
                    url: format!("https://{site}/browse/{}", text(&i["key"])),
                },
                title: text(&i["fields"]["summary"]),
                body: text(&i["fields"]["description"]),
                author: text(&i["fields"]["reporter"]["displayName"]),
                trusted: true,
            })
            .collect();
        Ok(IssueList {
            issues,
            milestone_missing: false,
        })
    }

    pub async fn comment(&self, issue: &IssueRef, text: &str) -> Result<(), String> {
        let url = self.api(&site_of(issue)?, &format!("issue/{}/comment", encode(&issue.key)));
        self.call(&url, Method::POST, Some(json!({ "body": text })))
            .await
            .map(|_| ())
    }

    /// Moves it by the transition named "Done", else the first into a done status.
    pub async fn close(&self, issue: &IssueRef) -> Result<(), String> {
        let url = self.api(&site_of(issue)?, &format!("issue/{}/transitions", encode(&issue.key)));
        let got = self.call(&url, Method::GET, None).await?;
        let all = got["transitions"].as_array().cloned().unwrap_or_default();
        let done = all
            .iter()
            .find(|t| t["name"].as_str().is_some_and(|n| n.to_lowercase() == "done"))
            .or_else(|| {
                all.iter()
                    .find(|t| t["to"]["statusCategory"]["key"].as_str() == Some("done"))
            })
            .ok_or_else(|| format!("Jira: {} has no Done transition", issue.key))?;
        self.call(&url, Method::POST, Some(json!({ "transition": { "id": done["id"] } })))
            .await
            .map(|_| ())
    }
}

/// `encodeURIComponent`: everything but letters, digits and `-_.!~*'()`.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The Jira site an issue is on, from its url.
fn site_of(issue: &IssueRef) -> Result<String, String> {
    if issue.provider != Provider::Jira {
        return Err(format!("{} isn't a Jira issue", issue.key));
    }
    let url = Url::parse(&issue.url).map_err(|e| format!("Jira: {e}"))?;
    let host = url
        .host_str()
        .ok_or_else(|| format!("Jira: {} has no site", issue.key))?;
    Ok(match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    })
}
