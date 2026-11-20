//! The wizard's Projects step lists the person's retaskable.com projects, in every organisation they are in, so they can
//! pick which to put on this computer (docs/specs/setup-wizard.md section 3.4; retaskable.com's
//! `GET /api/agents/projects`, organisations/03-the-model.md section 3.3.1). The ask goes through the agent's existing
//! cloud link ([`CloudToken`], the one token holder every other cloud call uses): a second holder of the rotating refresh
//! token would race the connector's and unlink the agent.
//!
//! A Worker from before the route answers 404: the page then lists the person's GitHub repos as it did, with a note.
//! Any other failure is said as it is and lists nothing from the cloud; the GitHub picker stays as the secondary way,
//! "Add a repo that isn't a project yet". Ticking a project links it as the site's Link project does: the repo and its
//! organisation's slug, kept in the project's config (`AddProject::start_in`).
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::routes::may_change;
use crate::link::TokenAnswer;
use crate::page_host::{bad_request, json_error, unauthorized};
use crate::settings_server::SettingsServer;
use crate::store_organisation::Pick;
use crate::view::CloudToken;

/// How long retaskable.com has to answer the list.
const ASK_LIMIT: Duration = Duration::from_secs(20);
/// The organisation listed first when the person is in it.
const FIRST: &str = "retaskable";
/// What the page says when the cloud has no such route yet.
pub(super) const NOT_DEPLOYED: &str =
    "retaskable.com can't list your projects yet, so these are your GitHub repositories.";

/// One project of the person's, as `GET /api/agents/projects` says it.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudProject {
    pub organisation_slug: String,
    pub organisation_name: String,
    pub id: u64,
    pub slug: String,
    pub name: String,
    pub code_host: String,
    /// `owner/repo`, when the project has one.
    pub repo: Option<String>,
    pub role: String,
    /// What the cloud says: a hub of this agent is attached to it.
    pub on_cloud: bool,
}

/// What asking retaskable.com for the list came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Listed {
    Projects(Vec<CloudProject>),
    /// `404`: a Worker from before the route.
    NotDeployed,
    Failed(String),
}

/// Where the list comes from: the cloud's link, or a made-up one in tests.
pub trait ProjectSource: Send + Sync {
    fn list(&self) -> BoxFuture<'static, Listed>;
}

/// The real source: the agent's own cloud, through its one token holder.
pub struct CloudSource {
    pub link: Arc<dyn CloudToken>,
    pub http: reqwest::Client,
}

impl ProjectSource for CloudSource {
    fn list(&self) -> BoxFuture<'static, Listed> {
        let (link, http) = (self.link.clone(), self.http.clone());
        Box::pin(async move { fetch(link.as_ref(), &http).await })
    }
}

/// `GET /api/agents/projects` with the link's access token.
pub async fn fetch(link: &dyn CloudToken, http: &reqwest::Client) -> Listed {
    let (url, token) = match link.answer().await {
        TokenAnswer::Token { url, access_token } => (url, access_token),
        TokenAnswer::Idle => return Listed::Failed("Link this agent to retaskable.com first.".into()),
        TokenAnswer::Unreachable => {
            return Listed::Failed("Couldn't reach retaskable.com: try again in a minute.".into());
        }
    };
    let sent = http
        .get(format!("{}/api/agents/projects", url.trim_end_matches('/')))
        .bearer_auth(token)
        .timeout(ASK_LIMIT)
        .send()
        .await;
    let res = match sent {
        Ok(res) => res,
        Err(e) => {
            return Listed::Failed(format!(
                "Couldn't reach retaskable.com: {}",
                crate::http_client::chain(&e)
            ));
        }
    };
    let status = res.status();
    if status.as_u16() == 404 {
        return Listed::NotDeployed;
    }
    if !status.is_success() {
        let why = res
            .json::<Value>()
            .await
            .ok()
            .and_then(|j| j["error"].as_str().map(str::to_string));
        return Listed::Failed(format!(
            "retaskable.com refused the list ({}){}",
            status.as_u16(),
            why.map_or(String::new(), |w| format!(": {w}"))
        ));
    }
    match res.json::<Value>().await.ok().as_ref().and_then(parse) {
        Some(projects) => Listed::Projects(projects),
        None => Listed::Failed("retaskable.com's list of projects was malformed.".into()),
    }
}

/// The answer's `projects`, or `None` when any row is not the shape the route promises.
pub fn parse(answer: &Value) -> Option<Vec<CloudProject>> {
    answer
        .get("projects")?
        .as_array()?
        .iter()
        .map(|row| {
            let (org, p) = (row.get("organisation")?, row.get("project")?);
            Some(CloudProject {
                organisation_slug: org.get("slug")?.as_str()?.to_string(),
                organisation_name: org.get("name")?.as_str()?.to_string(),
                id: p.get("id")?.as_u64()?,
                slug: p.get("slug")?.as_str()?.to_string(),
                name: p.get("name")?.as_str()?.to_string(),
                code_host: p.get("codeHost")?.as_str()?.to_string(),
                repo: p
                    .get("codeHostRef")
                    .and_then(Value::as_str)
                    .filter(|r| !r.is_empty())
                    .map(str::to_string),
                role: row.get("role").and_then(Value::as_str).unwrap_or("member").to_string(),
                on_cloud: row.get("onThisComputer").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

/// The projects by organisation, in the order the cloud gave them, Retaskable's organisation first when present. A
/// project is on this computer when the cloud says so or its repo is one of `on_computer` (this agent's own config, which
/// is the truth for a project not yet attached to a hub), in any case. Only a GitHub project with a repo can be linked here.
pub fn group(projects: &[CloudProject], on_computer: &[String]) -> Vec<Value> {
    let mut groups: Vec<(String, String, Vec<Value>)> = Vec::new();
    for p in projects {
        let here = p.on_cloud
            || p.repo
                .as_deref()
                .is_some_and(|r| on_computer.iter().any(|c| c.eq_ignore_ascii_case(r)));
        let row = json!({
            "id": p.id, "slug": p.slug, "name": p.name, "repo": p.repo, "codeHost": p.code_host, "role": p.role,
            "onThisComputer": here,
            "linkable": p.code_host == "github" && p.repo.is_some(),
        });
        match groups.iter_mut().find(|g| g.0 == p.organisation_slug) {
            Some(g) => g.2.push(row),
            None => groups.push((p.organisation_slug.clone(), p.organisation_name.clone(), vec![row])),
        }
    }
    // Stable: the rest keep the cloud's order.
    groups.sort_by_key(|g| !g.0.eq_ignore_ascii_case(FIRST));
    groups
        .into_iter()
        .map(|(slug, name, projects)| json!({ "slug": slug, "name": name, "projects": projects }))
        .collect()
}

/// The answer of `GET /api/wizard/projects`.
pub fn answer(listed: Listed, on_computer: &[String]) -> Value {
    match listed {
        Listed::Projects(projects) => json!({ "source": "cloud", "groups": group(&projects, on_computer) }),
        Listed::NotDeployed => json!({ "source": "github", "note": NOT_DEPLOYED, "groups": [] }),
        Listed::Failed(why) => json!({ "source": "cloud", "error": why, "groups": [] }),
    }
}

impl SettingsServer {
    /// Replaces where the Projects step's list comes from (tests; the agent's cloud link is used otherwise).
    pub fn set_wizard_projects(&self, source: Arc<dyn ProjectSource>) {
        let _ = self.wizard.projects.set(source);
    }

    fn project_source(&self) -> Option<Arc<dyn ProjectSource>> {
        if let Some(source) = self.wizard.projects.get() {
            return Some(source.clone());
        }
        let link = self.agent_view()?.item_commands.cloud.get()?;
        Some(Arc::new(CloudSource {
            link,
            http: crate::http_client::build(ASK_LIMIT),
        }))
    }
}

/// `GET /api/wizard/projects`: the person's retaskable.com projects by organisation, or why not.
pub(in crate::settings_server) async fn get_projects(
    State(settings): State<Arc<SettingsServer>>,
    headers: HeaderMap,
) -> Response {
    if settings.authenticate(&headers).is_none() {
        return unauthorized("Sign in first.");
    }
    let on_computer: Vec<String> = settings
        .add_project
        .get()
        .map(|a| a.projects().into_iter().map(|(_, repo)| repo).collect())
        .unwrap_or_default();
    let listed = match settings.project_source() {
        Some(source) => source.list().await,
        None => Listed::Failed("Link this agent to retaskable.com first.".into()),
    };
    Json(answer(listed, &on_computer)).into_response()
}

#[derive(Deserialize)]
pub(in crate::settings_server) struct LinkBody {
    repo: String,
    #[serde(default)]
    organisation: String,
}

/// `POST /api/wizard/projects` `{ repo, organisation }`: starts linking it, as the site's `link_project` does, the project
/// going in that organisation. Answers at once; `GET /api/projects` says how the add goes.
pub(in crate::settings_server) async fn post_project(
    State(settings): State<Arc<SettingsServer>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(r) = may_change(&settings, &peer, &headers) {
        return *r;
    }
    let Some(add) = settings.add_project.get().cloned() else {
        return crate::page_host::internal_error("This agent can't add a project from the page.");
    };
    let Ok(LinkBody { repo, organisation }) = serde_json::from_slice(&body) else {
        return bad_request("No JSON body with a repo.");
    };
    let slug = Some(organisation.as_str()).filter(|s| !s.is_empty());
    let pick = match settings.agent_view() {
        Some(view) => match view.item_commands.organisations.pick(slug, None) {
            Ok(pick) => pick,
            Err(_) => return bad_request("That organisation isn't one this computer is linked to."),
        },
        None => Pick {
            id: None,
            slug: slug.map(str::to_string),
        },
    };
    match add.start_in(&repo, pick) {
        Ok(()) => (axum::http::StatusCode::ACCEPTED, Json(json!({ "adding": true }))).into_response(),
        Err(crate::add_project::Refusal::Bad(m)) => bad_request(&m),
        Err(crate::add_project::Refusal::Conflict(m)) => json_error(axum::http::StatusCode::CONFLICT, &m),
    }
}
